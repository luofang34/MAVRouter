mod config;
mod endpoint_core;
mod error;
mod orchestration;
mod router;
mod endpoints {
    pub mod serial;
    pub mod tcp;
    pub mod tlog;
    pub mod udp;
}
mod dedup;
mod filter;
mod framing;
mod mavlink_utils;
mod routing;

use crate::config::Config;
use crate::error::Result;
use clap::Parser;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(unix)]
use tokio::signal::unix::{signal, Signal, SignalKind};

/// Helper struct to handle SIGHUP on Unix and do nothing on Windows
struct ReloadSignal {
    #[cfg(unix)]
    inner: Signal,
}

impl ReloadSignal {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: signal(SignalKind::hangup())
                    .map_err(|e| crate::error::RouterError::network("SIGHUP handler", e))?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn recv(&mut self) -> Option<()> {
        #[cfg(unix)]
        {
            self.inner.recv().await
        }
        #[cfg(not(unix))]
        {
            std::future::pending().await
        }
    }
}

/// Helper struct to handle SIGTERM on Unix and do nothing on Windows
struct TerminateSignal {
    #[cfg(unix)]
    inner: Signal,
}

impl TerminateSignal {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: signal(SignalKind::terminate())
                    .map_err(|e| crate::error::RouterError::network("SIGTERM handler", e))?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn recv(&mut self) -> Option<()> {
        #[cfg(unix)]
        {
            self.inner.recv().await
        }
        #[cfg(not(unix))]
        {
            std::future::pending().await
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to configuration file
    #[arg(
        short,
        long,
        default_value = "mavrouter.toml",
        env = "MAVROUTER_CONF_FILE"
    )]
    config: String,
    /// Path to configuration directory (*.toml files merged alphabetically)
    #[arg(long, env = "MAVROUTER_CONF_DIR")]
    config_dir: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // AGPL-3.0 License Notice
    info!("mavrouter-rs v{} - AGPL-3.0", env!("CARGO_PKG_VERSION"));
    info!("Source code: {}", env!("CARGO_PKG_REPOSITORY"));
    info!("This program comes with ABSOLUTELY NO WARRANTY.");

    let args = Args::parse();

    info!("Starting mavrouter-rs with config: {}", args.config);
    if let Some(ref dir) = args.config_dir {
        info!("Config directory: {}", dir);
    }

    // Initialize reload signal handler
    let mut sig_hup = ReloadSignal::new()?;

    // Initialize SIGTERM handler for graceful shutdown
    let mut sig_term = TerminateSignal::new()?;

    // Pre-validated config for SIGHUP reload (fixes TOCTOU race - issue #8)
    let mut next_config: Option<Config> = None;

    loop {
        let config = if let Some(cfg) = next_config.take() {
            cfg
        } else {
            match Config::load_merged(Some(args.config.as_str()), args.config_dir.as_deref()).await
            {
                Ok(c) => c,
                Err(e) => {
                    error!("Error loading config: {:#}", e);
                    return Err(e);
                }
            }
        };

        info!(
            "Loaded configuration with {} endpoints",
            config.endpoint.len()
        );

        let has_endpoints = !config.endpoint.is_empty() || config.general.tcp_port.is_some();
        if !has_endpoints {
            info!("No endpoints configured. Exiting.");
            return Ok(());
        }

        let cancel_token = CancellationToken::new();

        // Spawn all endpoints and background tasks via shared orchestration
        let mut orchestrated = crate::orchestration::spawn_all(&config, &cancel_token);

        // Stats Reporter + Stats Socket are spawned from orchestration to
        // keep main.rs focused on signals and shutdown. Both are optional:
        // the reporter is gated on non-zero intervals, the socket on a
        // configured path (and a unix-only build).
        if let Some(reporter) = crate::orchestration::spawn_stats_reporter(
            orchestrated.routing_table.clone(),
            orchestrated.endpoint_stats.clone(),
            config.general.stats_sample_interval_secs,
            config.general.stats_retention_secs,
            config.general.stats_log_interval_secs,
            cancel_token.child_token(),
        ) {
            orchestrated.tasks.push(reporter);
        }

        #[cfg(unix)]
        if let Some(socket_path) = config.general.stats_socket_path.clone() {
            if let Some(socket_task) = crate::orchestration::spawn_stats_socket(
                socket_path,
                orchestrated.routing_table.clone(),
                orchestrated.endpoint_stats.clone(),
                cancel_token.child_token(),
            ) {
                orchestrated.tasks.push(socket_task);
            }
        }

        let mut reload = false;

        // Snapshot task names + AbortHandles before consuming JoinHandles into
        // the join_all future — needed to enumerate which tasks are still
        // running if the shutdown budget expires.
        let task_report: Vec<(String, tokio::task::AbortHandle)> = orchestrated
            .tasks
            .iter()
            .map(|t| (t.name.clone(), t.handle.abort_handle()))
            .collect();

        let join_futures = orchestrated.tasks.into_iter().map(|t| {
            let name = t.name;
            let handle = t.handle;
            async move {
                let res = handle.await;
                (name, res)
            }
        });
        let mut all_tasks = Box::pin(futures::future::join_all(join_futures));

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Ctrl+C received. Initiating graceful shutdown...");
                    break;
                }
                _ = sig_term.recv() => {
                    info!("SIGTERM received. Initiating graceful shutdown...");
                    break;
                }
                _ = sig_hup.recv() => {
                    info!("SIGHUP received. Checking configuration for reload...");
                    // Save validated config to avoid TOCTOU race (issue #8)
                    match Config::load_merged(
                        Some(args.config.as_str()),
                        args.config_dir.as_deref(),
                    ).await {
                        Ok(validated_config) => {
                            info!("Configuration valid. Restarting...");
                            next_config = Some(validated_config);
                            reload = true;
                            break;
                        }
                        Err(e) => {
                            error!("Configuration invalid: {}. Ignoring SIGHUP.", e);
                        }
                    }
                }
                _ = &mut all_tasks => {
                    info!("All supervised tasks completed/failed. Shutting down.");
                    break;
                }
            }
        }

        cancel_token.cancel();

        // Join all task handles with timeout (issue #9 - was just sleep(1s)).
        // On timeout, use AbortHandle::is_finished() to report which tasks
        // did not exit in time, then abort them so the process can exit.
        match tokio::time::timeout(Duration::from_secs(5), all_tasks).await {
            Ok(results) => {
                for (name, res) in results {
                    if let Err(e) = res {
                        warn!("Task '{}' did not exit cleanly: {}", name, e);
                    }
                }
            }
            Err(_) => {
                let remaining: Vec<&str> = task_report
                    .iter()
                    .filter(|(_, ah)| !ah.is_finished())
                    .map(|(n, _)| n.as_str())
                    .collect();
                error!(
                    "Shutdown timed out after 5s; {} task(s) still running: {:?}",
                    remaining.len(),
                    remaining
                );
                for (_, ah) in &task_report {
                    if !ah.is_finished() {
                        ah.abort();
                    }
                }
            }
        }

        if !reload {
            break;
        }

        info!("Restarting system...");
    }

    info!("Shutdown complete.");
    Ok(())
}
