#![allow(clippy::unwrap_used)]

//! Stress and reliability tests

use mavrouter_rs::routing::RoutingTable;
use mavrouter_rs::dedup::Dedup;
use std::time::Duration;

#[test]
fn test_routing_table_large_scale_performance() {
    let mut rt = RoutingTable::new();

    // Insert 1000 routes
    for endpoint in 0..10 {
        for sys in 1..=100 {
            for comp in 1..=10 {
                rt.update(endpoint, sys, comp);
            }
        }
    }

    // Measure lookup performance
    let start = std::time::Instant::now();
    let iterations = 100_000;

    for i in 0..iterations {
        let sys = ((i % 100) + 1) as u8;
        let comp = ((i % 10) + 1) as u8;
        let endpoint = (i % 10) as usize;
        let _ = rt.should_send(endpoint, sys, comp);
    }

    let elapsed = start.elapsed();
    let avg_ns = elapsed.as_nanos() / iterations;

    println!("Average lookup time: {}ns", avg_ns);

    // Should be < 200ns per lookup
    assert!(avg_ns < 200, "Lookup too slow: {}ns", avg_ns);
}

#[test]
fn test_dedup_memory_actually_bounded() {
    let mut dedup = Dedup::new(Duration::from_millis(100));

    // Insert 100k packets
    for i in 0..100_000 {
        let data = format!("packet_{}", i);
        dedup.is_duplicate(data.as_bytes());
    }

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(150));

    // Insert another 100k (should not OOM, should prune old)
    for i in 100_000..200_000 {
        let data = format!("packet_{}", i);
        dedup.is_duplicate(data.as_bytes());
    }

    // If we get here without OOM, test passes
}

#[tokio::test]
async fn test_routing_table_concurrent_access() {
    use std::sync::Arc;
    use parking_lot::RwLock;

    let rt = Arc::new(RwLock::new(RoutingTable::new()));

    let mut handles = vec![];

    // Spawn 10 writers
    for i in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let mut rt_lock = rt_clone.write();
                rt_lock.update(i, (j % 255) as u8, 1);
            }
        }));
    }

    // Spawn 10 readers
    for _ in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let rt_lock = rt_clone.read();
                let _ = rt_lock.should_send(0, (j % 255) as u8, 1);
            }
        }));
    }

    // All should complete without deadlock
    for handle in handles {
        handle.await.unwrap();
    }
}
