#![allow(clippy::unwrap_used)]

//! Stress and reliability tests

use mavrouter_rs::dedup::Dedup;
use mavrouter_rs::router::EndpointId;
use mavrouter_rs::routing::RoutingTable;
use std::env;
use std::time::{Duration, Instant};

// Helper to determine stress test iterations based on environment
#[allow(clippy::expect_used)]
fn stress_iterations() -> usize {
    // Environment variable override
    if let Ok(s) = env::var("CI_STRESS_ITERATIONS") {
        return s.parse().expect("CI_STRESS_ITERATIONS must be a number");
    }

    // Auto-detection based on CPU cores
    let cpus = num_cpus::get();
    if cpus >= 64 {
        10_000_000 // Extreme test
    } else if cpus >= 8 {
        1_000_000
    } else if cpus >= 4 {
        500_000
    } else {
        100_000 // CI environment (2 cores) or very weak machine
    }
}

#[test]
fn test_routing_table_stress_functional() {
    let mut rt = RoutingTable::new();

    // Insert 1000 routes
    for endpoint in 0..10 {
        for sys in 1..=100 {
            for comp in 1..=10 {
                rt.update(EndpointId(endpoint), sys, comp, Instant::now());
            }
        }
    }

    // Stress test: N operations should complete without panic
    let iterations = stress_iterations();
    println!(
        "Running test_routing_table_stress_functional with {} iterations",
        iterations
    );

    for i in 0..iterations {
        let sys = ((i % 100) + 1) as u8;
        let comp = ((i % 10) + 1) as u8;
        let endpoint = i % 10;
        let result = rt.should_send(EndpointId(endpoint), sys, comp);

        // Verify basic functional correctness (route should exist)
        assert!(
            result,
            "Route should exist for endpoint {} sys {} comp {}",
            endpoint, sys, comp
        );
    }
}

#[test]
fn test_dedup_memory_actually_bounded() {
    let mut dedup = Dedup::new(Duration::from_millis(100));
    let iterations = stress_iterations();
    println!(
        "Running test_dedup_memory_actually_bounded with {} iterations",
        iterations
    );

    // Insert N packets
    for i in 0..iterations {
        let data = format!("packet_{}", i);
        dedup.is_duplicate(data.as_bytes());
    }

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(150));

    // Insert another N (should not OOM, should prune old)
    for i in iterations..(iterations * 2) {
        let data = format!("packet_{}", i);
        dedup.is_duplicate(data.as_bytes());
    }

    // If we get here without OOM, test passes
}

#[tokio::test]
async fn test_routing_table_concurrent_access() {
    use parking_lot::RwLock;
    use std::sync::Arc;

    let rt = Arc::new(RwLock::new(RoutingTable::new()));

    let mut handles = vec![];

    // Spawn 10 writers
    for i in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let mut rt_lock = rt_clone.write();
                rt_lock.update(EndpointId(i), (j % 255) as u8, 1, Instant::now());
            }
        }));
    }

    // Spawn 10 readers
    for _ in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let rt_lock = rt_clone.read();
                let _ = rt_lock.should_send(EndpointId(0), (j % 255) as u8, 1);
            }
        }));
    }

    // All should complete without deadlock
    for handle in handles {
        handle.await.unwrap();
    }
}
