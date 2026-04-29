#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use crate::endpoint_core::{EndpointStats, EndpointStatsSnapshot};
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[test]
fn test_endpoint_stats_default_zero() {
    let stats = EndpointStats::default();
    let snap = stats.snapshot();
    assert_eq!(snap, EndpointStatsSnapshot::default());
    assert_eq!(snap.msgs_in, 0);
    assert_eq!(snap.msgs_out, 0);
    assert_eq!(snap.bytes_in, 0);
    assert_eq!(snap.bytes_out, 0);
    assert_eq!(snap.errors, 0);
}

#[test]
fn test_endpoint_stats_increment_and_snapshot() {
    let stats = EndpointStats::new();
    stats.msgs_in.fetch_add(5, Ordering::Relaxed);
    stats.bytes_in.fetch_add(500, Ordering::Relaxed);
    stats.record_outgoing(100);
    stats.record_outgoing(200);
    stats.errors.fetch_add(1, Ordering::Relaxed);

    let snap = stats.snapshot();
    assert_eq!(snap.msgs_in, 5);
    assert_eq!(snap.bytes_in, 500);
    assert_eq!(snap.msgs_out, 2);
    assert_eq!(snap.bytes_out, 300);
    assert_eq!(snap.errors, 1);
}

#[test]
fn test_endpoint_stats_snapshot_display() {
    let snap = EndpointStatsSnapshot {
        msgs_in: 10,
        msgs_out: 20,
        bytes_in: 1000,
        bytes_out: 2000,
        errors: 3,
        bus_lagged: 7,
    };
    assert_eq!(format!("{}", snap), "in=10/1000 out=20/2000 err=3 lagged=7");
}

#[test]
fn test_endpoint_stats_concurrent_access() {
    use std::thread;

    let stats = Arc::new(EndpointStats::new());
    let num_threads = 8;
    let ops_per_thread = 1000u64;

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let stats = stats.clone();
            thread::spawn(move || {
                for _ in 0..ops_per_thread {
                    stats.msgs_in.fetch_add(1, Ordering::Relaxed);
                    stats.bytes_in.fetch_add(10, Ordering::Relaxed);
                    stats.record_outgoing(20);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    let snap = stats.snapshot();
    let total = num_threads as u64 * ops_per_thread;
    assert_eq!(snap.msgs_in, total);
    assert_eq!(snap.bytes_in, total * 10);
    assert_eq!(snap.msgs_out, total);
    assert_eq!(snap.bytes_out, total * 20);
}
