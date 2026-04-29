//! Top-level reload loop extracted from `main()` so tests can drive it
//! with synthetic signals through `mpsc::Receiver<()>` instead of having
//! to fork the binary and send real `SIGHUP` / `SIGTERM`.

use crate::config::Config;
use crate::error::{Result, RouterError};
use crate::orchestration;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(unix)]
use tokio::signal::unix::{signal, Signal, SignalKind};

#[cfg(test)]
mod tests;

/// Source of a one-shot signal event. Real `tokio::signal::unix::Signal`
/// wraps it for production; `mpsc::Receiver<()>` does for tests.
pub(crate) trait SignalSource: Send {
    /// Returns `Some(())` for the next signal, `None` if the underlying
    /// source is closed.
    async fn recv(&mut self) -> Option<()>;
}

impl SignalSource for mpsc::Receiver<()> {
    async fn recv(&mut self) -> Option<()> {
        mpsc::Receiver::recv(self).await
    }
}

/// SIGHUP wrapper. On non-unix builds, `recv()` is `pending` (never fires).
pub(crate) struct ReloadSignal {
    #[cfg(unix)]
    inner: Signal,
}

impl ReloadSignal {
    pub(crate) fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: signal(SignalKind::hangup())
                    .map_err(|e| RouterError::network("SIGHUP handler", e))?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }
}

impl SignalSource for ReloadSignal {
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

/// Combined SIGTERM + Ctrl-C wrapper. `recv()` resolves on whichever
/// arrives first. On non-unix builds, only Ctrl-C is wired.
pub(crate) struct ShutdownSignal {
    #[cfg(unix)]
    sigterm: Signal,
}

impl ShutdownSignal {
    pub(crate) fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                sigterm: signal(SignalKind::terminate())
                    .map_err(|e| RouterError::network("SIGTERM handler", e))?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }
}

impl SignalSource for ShutdownSignal {
    async fn recv(&mut self) -> Option<()> {
        #[cfg(unix)]
        {
            tokio::select! {
                v = self.sigterm.recv() => v,
                r = tokio::signal::ctrl_c() => r.ok().map(|_| ()),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok().map(|_| ())
        }
    }
}

/// Test-only event surface. Tests pass `Some(sender)` so they can
/// observe reload semantics without scraping logs or polling external
/// sockets. Production passes `None`; the `if let Some` short-circuit
/// keeps the cost negligible on the hot path (the events fire only at
/// startup / reload / shutdown, not per message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunEvent {
    /// Emitted after each successful `Config::load_merged`. Fires once
    /// at startup and once after each successful SIGHUP reload.
    ConfigLoaded { endpoints: usize },
    /// SIGHUP arrived; `Config::load_merged` is about to run.
    ReloadAttempted,
    /// The SIGHUP-triggered reload's config validated; the next loop
    /// iteration will tear down and respawn.
    ReloadSucceeded,
    /// The SIGHUP-triggered reload was rejected because the new config
    /// was invalid. The router keeps running on the previous config.
    ReloadSkipped { reason: String },
    /// `cancel_token.cancel()` is about to be called. Always fires
    /// once per session, before either teardown or final exit.
    ShutdownStarted,
}

async fn emit(events: Option<&mpsc::Sender<RunEvent>>, event: RunEvent) {
    if let Some(tx) = events {
        // Test channel may be dropped before the sender catches up, e.g.
        // when the test asserts on an early event and exits without
        // draining the rest of the session. Discarding here is fine —
        // the events sink is observability, not control flow.
        tx.send(event).await.ok();
    }
}

/// Returns `Ok(())` on clean shutdown. The *initial* config-load
/// failure propagates as `Err`; SIGHUP-triggered reload failures are
/// non-fatal and surface as `RunEvent::ReloadSkipped`.
pub(crate) async fn run_with_signals<H, T>(
    config_path: String,
    config_dir: Option<String>,
    mut sig_hup: H,
    mut shutdown: T,
    events: Option<mpsc::Sender<RunEvent>>,
) -> Result<()>
where
    H: SignalSource,
    T: SignalSource,
{
    let mut next_config: Option<Config> = None;

    loop {
        let config = if let Some(cfg) = next_config.take() {
            cfg
        } else {
            match Config::load_merged(Some(config_path.as_str()), config_dir.as_deref()).await {
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
        emit(
            events.as_ref(),
            RunEvent::ConfigLoaded {
                endpoints: config.endpoint.len(),
            },
        )
        .await;

        let has_endpoints = !config.endpoint.is_empty() || config.general.tcp_port.is_some();
        if !has_endpoints {
            info!("No endpoints configured. Exiting.");
            return Ok(());
        }

        let cancel_token = CancellationToken::new();
        let mut orchestrated = orchestration::spawn_all(&config, &cancel_token);

        if let Some(reporter) = orchestration::spawn_stats_reporter(
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
            if let Some(socket_task) = orchestration::spawn_stats_socket(
                socket_path,
                orchestrated.routing_table.clone(),
                orchestrated.endpoint_stats.clone(),
                cancel_token.child_token(),
            ) {
                orchestrated.tasks.push(socket_task);
            }
        }

        let mut reload = false;

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
                _ = shutdown.recv() => {
                    info!("Shutdown signal received. Initiating graceful shutdown...");
                    break;
                }
                _ = sig_hup.recv() => {
                    info!("SIGHUP received. Checking configuration for reload...");
                    emit(events.as_ref(), RunEvent::ReloadAttempted).await;
                    match Config::load_merged(
                        Some(config_path.as_str()),
                        config_dir.as_deref(),
                    ).await {
                        Ok(validated_config) => {
                            info!("Configuration valid. Restarting...");
                            next_config = Some(validated_config);
                            reload = true;
                            emit(events.as_ref(), RunEvent::ReloadSucceeded).await;
                            break;
                        }
                        Err(e) => {
                            error!("Configuration invalid: {}. Ignoring SIGHUP.", e);
                            emit(
                                events.as_ref(),
                                RunEvent::ReloadSkipped { reason: e.to_string() },
                            )
                            .await;
                        }
                    }
                }
                _ = &mut all_tasks => {
                    info!("All supervised tasks completed/failed. Shutting down.");
                    break;
                }
            }
        }

        emit(events.as_ref(), RunEvent::ShutdownStarted).await;
        cancel_token.cancel();

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
