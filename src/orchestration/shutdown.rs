//! `shutdown_with_timeout`: drives a set of `NamedTask`s to completion,
//! enumerating and aborting stragglers if the budget expires.

use super::NamedTask;
use futures::future::join_all;
use std::time::Duration;
use tokio::task::AbortHandle;
use tracing::{error, warn};

// Used by the library (high_level.rs) and by integration tests; the binary
// has its own inlined variant tailored to the main-loop select. Suppress the
// bin-side "never used" warning without hiding real dead code.
#[allow(dead_code)]
/// Drive a set of [`NamedTask`]s to completion, bounded by `timeout_dur`.
///
/// On timeout, enumerates the task names that have not finished via
/// [`AbortHandle::is_finished`], logs them at `error!` level, and aborts
/// them so the process can exit instead of hanging. Returns `true` if all
/// tasks exited on their own within the budget.
///
/// Use this after cancelling the shared [`CancellationToken`] so tasks
/// have a reason to exit promptly.
pub async fn shutdown_with_timeout(tasks: Vec<NamedTask>, timeout_dur: Duration) -> bool {
    if tasks.is_empty() {
        return true;
    }

    // Snapshot (name, AbortHandle) before we move each JoinHandle into the
    // joined future — AbortHandles remain valid after the JoinHandle is
    // consumed and let us ask "did this task actually finish?" on timeout.
    let abort_view: Vec<(String, AbortHandle)> = tasks
        .iter()
        .map(|t| (t.name.clone(), t.handle.abort_handle()))
        .collect();

    let joins = tasks.into_iter().map(|t| {
        let name = t.name;
        let handle = t.handle;
        async move {
            let res = handle.await;
            (name, res)
        }
    });

    match tokio::time::timeout(timeout_dur, join_all(joins)).await {
        Ok(results) => {
            for (name, res) in results {
                if let Err(e) = res {
                    warn!("Task '{}' did not exit cleanly: {}", name, e);
                }
            }
            true
        }
        Err(_) => {
            let remaining: Vec<&str> = abort_view
                .iter()
                .filter(|(_, ah)| !ah.is_finished())
                .map(|(n, _)| n.as_str())
                .collect();
            error!(
                "Shutdown timed out after {:?}; {} task(s) still running: {:?}",
                timeout_dur,
                remaining.len(),
                remaining
            );
            for (_, ah) in &abort_view {
                if !ah.is_finished() {
                    ah.abort();
                }
            }
            false
        }
    }
}
