//! `supervise`: restarts a task on failure with exponential backoff until
//! its `CancellationToken` fires.

use crate::endpoint_core::ExponentialBackoff;
use crate::error::Result;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Supervisor function that restarts tasks on failure with exponential backoff.
///
/// Wraps a task factory, restarting the task whenever it completes (either
/// successfully or with an error). Uses exponential backoff to avoid rapid
/// restart loops, resetting after 60 seconds of stable operation.
pub async fn supervise<F, Fut>(name: String, cancel_token: CancellationToken, task_factory: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(30), 2.0);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} shutting down.", name);
                break;
            }
            _ = async {
                let start_time = std::time::Instant::now();
                let result = task_factory().await;

                // If task ran for more than 60 seconds, reset backoff
                if start_time.elapsed() > Duration::from_secs(60) {
                    backoff.reset();
                }

                match result {
                    Ok(_) => {
                        warn!("Supervisor: Task {} finished cleanly (unexpected). Restarting...", name);
                    }
                    Err(e) => {
                        error!("Supervisor: Task {} failed: {:#}. Restarting...", name, e);
                    }
                }
            } => {}
        }

        let wait = backoff.next_backoff();
        info!(
            "Supervisor: Waiting {:.1?} before restarting {}",
            wait, name
        );
        tokio::select! {
            _ = tokio::time::sleep(wait) => {},
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} shutting down during backoff.", name);
                break;
            }
        }
    }
}
