//! Shutdown behaviour tests for [`mavrouter::orchestration::shutdown_with_timeout`].
//!
//! The high-level `Router::stop()` delegates its bounded-time guarantee to
//! `shutdown_with_timeout`, so exercising the helper directly with a deliberately
//! stuck task is what pins the contract down:
//!
//! - a task that refuses to observe cancellation must not wedge shutdown,
//! - the timeout branch must log the offending task names at `error!`, and
//! - the helper must abort the stragglers on its way out so the process can exit.

#![allow(clippy::expect_used)]

use std::time::{Duration, Instant};

use mavrouter::orchestration::{shutdown_with_timeout, NamedTask};
use tokio_util::sync::CancellationToken;
use tracing_test::traced_test;

/// Spawn a task that ignores its `CancellationToken` and sleeps for far longer
/// than any reasonable shutdown budget — the point is to make `shutdown_with_timeout`
/// hit its timeout branch, not to let the task exit on its own.
fn spawn_stuck_task(name: &str) -> NamedTask {
    let handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    NamedTask::new(name, handle)
}

#[tokio::test(flavor = "current_thread")]
#[traced_test]
async fn shutdown_with_timeout_reports_and_aborts_stuck_tasks() {
    let stuck = spawn_stuck_task("Stuck Task A");
    let abort_handle = stuck.handle.abort_handle();

    let start = Instant::now();
    let clean_exit = shutdown_with_timeout(vec![stuck], Duration::from_millis(500)).await;
    let elapsed = start.elapsed();

    assert!(
        !clean_exit,
        "timeout path must report !clean_exit, got true after {:?}",
        elapsed
    );
    // The user contract is "returns within 6 seconds"; we're stricter here
    // because we used a 500ms budget, but the 6s ceiling is the public promise.
    assert!(
        elapsed < Duration::from_secs(6),
        "shutdown_with_timeout must return within 6s even for stuck tasks, took {:?}",
        elapsed
    );
    assert!(
        logs_contain("Shutdown timed out"),
        "expected an error log containing 'Shutdown timed out'"
    );
    assert!(
        logs_contain("Stuck Task A"),
        "expected the stuck task name to be enumerated in the timeout log"
    );

    // Helper must abort stragglers so they can't keep running forever.
    // Wait briefly for the abort to propagate through the runtime.
    for _ in 0..20 {
        if abort_handle.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        abort_handle.is_finished(),
        "stuck task should have been aborted after the timeout"
    );
}

#[tokio::test(flavor = "current_thread")]
#[traced_test]
async fn shutdown_with_timeout_is_fast_path_for_cooperative_tasks() {
    let token = CancellationToken::new();
    let cooperative_token = token.clone();
    let cooperative = NamedTask::new(
        "Cooperative Task",
        tokio::spawn(async move {
            cooperative_token.cancelled().await;
        }),
    );

    token.cancel();

    let start = Instant::now();
    let clean_exit = shutdown_with_timeout(vec![cooperative], Duration::from_secs(5)).await;
    let elapsed = start.elapsed();

    assert!(clean_exit, "cooperative task must hit the Ok branch");
    assert!(
        elapsed < Duration::from_secs(1),
        "cooperative shutdown should finish well inside the budget, took {:?}",
        elapsed
    );
    assert!(
        !logs_contain("Shutdown timed out"),
        "no timeout log should be emitted when all tasks exit cleanly"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_with_timeout_is_noop_for_empty_task_list() {
    let start = Instant::now();
    let clean_exit = shutdown_with_timeout(Vec::new(), Duration::from_millis(1)).await;
    assert!(clean_exit);
    assert!(start.elapsed() < Duration::from_millis(50));
}
