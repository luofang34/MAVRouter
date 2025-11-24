#![allow(clippy::unwrap_used)]

//! Stress and reliability tests

use mavrouter_rs::routing::RoutingTable;
use mavrouter_rs::dedup::Dedup;
use std::time::Duration;

#[test]
fn test_routing_table_large_scale() {
    let mut rt = RoutingTable::new();

    // Simulate 100 systems, 10 components each, 20 endpoints
    for endpoint in 0..20 {
        for sys in 1..=100 {
            for comp in 1..=10 {
                rt.update(endpoint, sys, comp);
            }
        }
    }

    // Verify no panic and correct routing
    assert!(rt.should_send(0, 50, 5));
    assert!(!rt.should_send(0, 255, 1)); // Unknown system
}

#[test]
fn test_dedup_memory_bounded() {
    let mut dedup = Dedup::new(Duration::from_millis(100));

    // Simulate 10000 different packets
    for i in 0..10000 {
        let data = format!("packet_{}", i);
        assert!(!dedup.is_duplicate(data.as_bytes()));
    }

    // Old entries should be pruned (no OOM)
    std::thread::sleep(Duration::from_millis(150));

    // First packet should be re-accepted
    assert!(!dedup.is_duplicate(b"packet_0"));
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
