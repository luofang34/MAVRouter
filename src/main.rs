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
mod stats;

#[cfg(unix)]
mod main_stats_server;
#[cfg(unix)]
use crate::main_stats_server::run_stats_server;

use crate::config::Config;
use crate::error::Result;
use crate::orchestration::NamedTask;
use crate::stats::StatsHistory;
use clap::Parser;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(unix)]
use crate::orchestration::supervise;

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

        // Stats Reporting Task (main.rs-specific)
        let rt_stats = orchestrated.routing_table.clone();
        let stats_token = cancel_token.child_token();
        let ep_stats_for_reporter = orchestrated.endpoint_stats.clone();

        let sample_interval = config.general.stats_sample_interval_secs;
        let retention = config.general.stats_retention_secs;
        let log_interval = config.general.stats_log_interval_secs;

        if sample_interval > 0 && retention > 0 {
            orchestrated.tasks.push(NamedTask::new(
                "Stats Reporter",
                tokio::spawn(async move {
                let mut history = StatsHistory::new(retention);
                let mut last_log_time = 0u64;

                loop {
                    tokio::select! {
                        _ = stats_token.cancelled() => {
                            info!("Stats Reporter shutting down.");
                            break;
                        }
                        _ = tokio::time::sleep(Duration::from_secs(sample_interval)) => {
                            let mut stats = rt_stats.stats();
                            // Ensure timestamp reflects sample time
                            stats.timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
                                Ok(d) => d.as_secs(),
                                Err(e) => {
                                    // System clock predates the UNIX epoch — can happen on
                                    // embedded boards with an unset RTC. Record 0 and warn
                                    // so the anomaly shows up rather than being swallowed.
                                    warn!("System clock is before UNIX epoch: {}", e);
                                    0
                                }
                            };

                            let current_timestamp = stats.timestamp;
                            history.push(stats.clone());

                            // Periodic logging
                            if current_timestamp.saturating_sub(last_log_time) >= log_interval {
                                // 1 minute aggregation
                                if let Some(min1) = history.aggregate(60) {
                                    info!(
                                        "Stats [1min] avg={:.1} routes, range=[{}-{}]",
                                        min1.avg_routes, min1.min_routes, min1.max_routes
                                    );
                                }

                                // 1 hour aggregation
                                if let Some(hour1) = history.aggregate(3600) {
                                    info!(
                                        "Stats [1hr] avg={:.1} routes, max={}",
                                        hour1.avg_routes, hour1.max_routes
                                    );
                                }

                                // Full retention aggregation
                                if let Some(all) = history.aggregate(retention) {
                                    info!(
                                        "Stats [{}h] avg={:.1} routes, samples={}, systems={}, endpoints={}",
                                        retention / 3600,
                                        all.avg_routes,
                                        all.sample_count,
                                        stats.total_systems,
                                        stats.total_endpoints
                                    );
                                }

                                // Per-endpoint stats
                                for (ep_id, ep_name, ep_stats) in &ep_stats_for_reporter {
                                    let snap = ep_stats.snapshot();
                                    info!(
                                        "Endpoint {} ({}) {}",
                                        ep_id, ep_name, snap
                                    );
                                }

                                last_log_time = current_timestamp;
                            }
                        }
                    }
                }
            }),
            ));
        }

        // Stats Query Interface (Unix Socket, main.rs-specific)
        #[cfg(unix)]
        if let Some(socket_path) = config.general.stats_socket_path.clone() {
            if !socket_path.is_empty() {
                let name = format!("Stats Socket {}", socket_path);
                let rt = orchestrated.routing_table.clone();
                let ep_stats_for_server = orchestrated.endpoint_stats.clone();
                let task_token = cancel_token.child_token();
                let path = socket_path.clone();

                let supervisor_name = name.clone();
                orchestrated.tasks.push(NamedTask::new(
                    name,
                    tokio::spawn(supervise(supervisor_name, task_token.clone(), move || {
                        let rt = rt.clone();
                        let ep_st = ep_stats_for_server.clone();
                        let token = task_token.clone();
                        let p = path.clone();
                        async move { run_stats_server(p, rt, ep_st, token).await }
                    })),
                ));
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
