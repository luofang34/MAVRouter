#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::arithmetic_side_effects)]

use super::{run_with_signals, RunEvent};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Reserve an ephemeral UDP port using the bind-and-release pattern. Same
/// idiom as the Rust integration tests' `claim_udp_port` and the Python
/// suite's `claim_udp_port` — keeps these tests off CLAUDE.md's
/// hard-coded-port guardrail and out of each other's way on shared CI.
fn claim_udp_port() -> u16 {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("reserve udp port");
    sock.local_addr().expect("local_addr").port()
}

fn write_minimal_config(dir: &std::path::Path, port: u16) -> std::path::PathBuf {
    let cfg = format!(
        r#"
[[endpoint]]
type = "udp"
address = "127.0.0.1:{port}"
mode = "server"
"#
    );
    let path = dir.join("config.toml");
    std::fs::write(&path, cfg).expect("write config");
    path
}

/// Pull the next event matching `pred` off the events channel, panicking
/// if none arrives within `budget`. Skips non-matching events so this
/// helper can be used to assert "X happens *eventually*" rather than
/// "X is the very next event" — the loop emits multiple events per
/// session and the order between unrelated subsystems isn't part of the
/// contract being tested.
async fn expect_event(
    rx: &mut mpsc::Receiver<RunEvent>,
    label: &str,
    pred: impl Fn(&RunEvent) -> bool,
    budget: Duration,
) -> RunEvent {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(event)) => {
                if pred(&event) {
                    return event;
                }
            }
            Ok(None) => panic!("events channel closed before {label} arrived"),
            Err(_) => break,
        }
    }
    panic!("event '{label}' did not arrive within {budget:?}");
}

#[tokio::test]
async fn test_clean_shutdown_returns_ok() {
    let port = claim_udp_port();
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = write_minimal_config(dir.path(), port);

    let (_hup_tx, hup_rx) = mpsc::channel::<()>(8);
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(8);
    let (events_tx, mut events_rx) = mpsc::channel::<RunEvent>(64);

    let cfg_path_str = cfg_path.to_str().expect("utf8 path").to_string();
    let handle = tokio::spawn(async move {
        run_with_signals(cfg_path_str, None, hup_rx, shutdown_rx, Some(events_tx)).await
    });

    expect_event(
        &mut events_rx,
        "ConfigLoaded",
        |e| matches!(e, RunEvent::ConfigLoaded { endpoints: 1 }),
        Duration::from_secs(5),
    )
    .await;

    shutdown_tx.send(()).await.expect("send shutdown");

    let result = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("run_with_signals should return within 10s")
        .expect("join error");
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}

#[tokio::test]
async fn test_sighup_reload_emits_events() {
    let port = claim_udp_port();
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = write_minimal_config(dir.path(), port);

    let (hup_tx, hup_rx) = mpsc::channel::<()>(8);
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(8);
    let (events_tx, mut events_rx) = mpsc::channel::<RunEvent>(64);

    let cfg_path_str = cfg_path.to_str().expect("utf8 path").to_string();
    let handle = tokio::spawn(async move {
        run_with_signals(cfg_path_str, None, hup_rx, shutdown_rx, Some(events_tx)).await
    });

    expect_event(
        &mut events_rx,
        "first ConfigLoaded",
        |e| matches!(e, RunEvent::ConfigLoaded { endpoints: 1 }),
        Duration::from_secs(5),
    )
    .await;

    hup_tx.send(()).await.expect("send sighup");

    expect_event(
        &mut events_rx,
        "ReloadAttempted",
        |e| matches!(e, RunEvent::ReloadAttempted),
        Duration::from_secs(5),
    )
    .await;
    expect_event(
        &mut events_rx,
        "ReloadSucceeded",
        |e| matches!(e, RunEvent::ReloadSucceeded),
        Duration::from_secs(5),
    )
    .await;
    expect_event(
        &mut events_rx,
        "ShutdownStarted (between sessions)",
        |e| matches!(e, RunEvent::ShutdownStarted),
        Duration::from_secs(10),
    )
    .await;
    expect_event(
        &mut events_rx,
        "second ConfigLoaded (post-reload)",
        |e| matches!(e, RunEvent::ConfigLoaded { endpoints: 1 }),
        Duration::from_secs(10),
    )
    .await;

    shutdown_tx.send(()).await.expect("send shutdown");
    tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .ok();
}

#[tokio::test]
async fn test_sighup_invalid_config_skipped() {
    let port = claim_udp_port();
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = write_minimal_config(dir.path(), port);

    let (hup_tx, hup_rx) = mpsc::channel::<()>(8);
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(8);
    let (events_tx, mut events_rx) = mpsc::channel::<RunEvent>(64);

    let cfg_path_str = cfg_path.to_str().expect("utf8 path").to_string();
    let cfg_path_for_corruption = cfg_path.clone();
    let handle = tokio::spawn(async move {
        run_with_signals(cfg_path_str, None, hup_rx, shutdown_rx, Some(events_tx)).await
    });

    expect_event(
        &mut events_rx,
        "ConfigLoaded",
        |e| matches!(e, RunEvent::ConfigLoaded { endpoints: 1 }),
        Duration::from_secs(5),
    )
    .await;

    std::fs::write(&cfg_path_for_corruption, "this is not valid toml [[[")
        .expect("overwrite config");

    hup_tx.send(()).await.expect("send sighup");

    expect_event(
        &mut events_rx,
        "ReloadAttempted",
        |e| matches!(e, RunEvent::ReloadAttempted),
        Duration::from_secs(5),
    )
    .await;
    expect_event(
        &mut events_rx,
        "ReloadSkipped",
        |e| matches!(e, RunEvent::ReloadSkipped { .. }),
        Duration::from_secs(5),
    )
    .await;

    shutdown_tx.send(()).await.expect("send shutdown");

    expect_event(
        &mut events_rx,
        "ShutdownStarted",
        |e| matches!(e, RunEvent::ShutdownStarted),
        Duration::from_secs(10),
    )
    .await;

    let result = tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("run_with_signals should return within 15s")
        .expect("join error");
    assert!(
        result.is_ok(),
        "expected Ok after invalid SIGHUP, got {result:?}"
    );
}
