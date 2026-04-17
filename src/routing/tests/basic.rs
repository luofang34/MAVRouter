//! Routing table semantics: basic operations, capacity limits,
//! concurrent access patterns, `EndpointId` edge cases, and
//! `needs_update_for_endpoint` staleness.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::print_stdout)]

use crate::router::EndpointId;
use crate::routing::*;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
// ============================================================================
// Basic Routing Operations
// ============================================================================

#[test]
fn test_basic_routing() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    assert!(!rt.should_send(EndpointId(0), 1, 1));

    rt.update(EndpointId(0), 1, 1, now);

    assert!(rt.should_send(EndpointId(0), 1, 1));
    assert!(!rt.should_send(EndpointId(1), 1, 1));

    let stats = rt.stats();
    assert_eq!(stats.total_systems, 1);
    assert_eq!(stats.total_routes, 1);
    assert_eq!(stats.total_endpoints, 1);
}

#[test]
fn test_component_broadcast_routing() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    rt.update(EndpointId(0), 1, 1, now);
    rt.update(EndpointId(1), 1, 2, now);

    // Message targeting system 1, component 0 (all components) should go to
    // both endpoints that have seen system 1.
    assert!(rt.should_send(EndpointId(0), 1, 0));
    assert!(rt.should_send(EndpointId(1), 1, 0));
    assert!(!rt.should_send(EndpointId(2), 1, 0));
}

#[test]
fn test_routing_component_fallback() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    rt.update(EndpointId(0), 1, 1, now);

    // Target component 99 unknown, but system 1 is known — fall back to
    // system-level route.
    assert!(
        rt.should_send(EndpointId(0), 1, 99),
        "Should fallback to system route when component unknown"
    );
}

#[test]
fn test_multi_system_endpoint() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    rt.update(EndpointId(0), 1, 1, now);
    rt.update(EndpointId(0), 2, 1, now);
    rt.update(EndpointId(0), 3, 1, now);

    assert!(rt.should_send(EndpointId(0), 1, 1));
    assert!(rt.should_send(EndpointId(0), 2, 1));
    assert!(rt.should_send(EndpointId(0), 3, 1));

    let stats = rt.stats();
    assert_eq!(stats.total_systems, 3);
    assert_eq!(stats.total_endpoints, 1);
}

#[test]
fn test_broadcast_combinations() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    rt.update(EndpointId(0), 1, 1, now);

    // (0, 0) - full broadcast
    assert!(rt.should_send(EndpointId(0), 0, 0));
    assert!(rt.should_send(EndpointId(99), 0, 0));

    // (0, X) - sysid==0 always broadcasts regardless of component
    assert!(rt.should_send(EndpointId(0), 0, 1));
    assert!(rt.should_send(EndpointId(99), 0, 1));

    // (X, 0) - specific system, all components
    assert!(rt.should_send(EndpointId(0), 1, 0));
    assert!(!rt.should_send(EndpointId(99), 1, 0));

    // (X, Y) - specific system and component
    assert!(rt.should_send(EndpointId(0), 1, 1));
    assert!(!rt.should_send(EndpointId(99), 1, 1));
}

// ============================================================================
// Capacity Limits
// ============================================================================

#[test]
fn test_routing_table_capacity_limit() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // MAX_ROUTES = 100_000, MAX_SYSTEMS = 1_000, but u8 sysid/compid means
    // at most 255*256 = 65_280 routes physically possible.
    for sysid in 1..=255u8 {
        for compid in 0..=255u8 {
            rt.update(EndpointId(0), sysid, compid, now);
        }
    }

    let stats = rt.stats();
    assert_eq!(stats.total_routes, 255 * 256, "Should have 255*256 routes");
    assert_eq!(stats.total_systems, 255, "Should have 255 systems");
}

#[test]
fn test_routing_table_churn_stability() {
    let mut rt = RoutingTable::new();

    let iterations = 100_000usize;
    let prune_interval = 10_000usize;

    for i in 0..iterations {
        let now = Instant::now();
        let sysid = ((i % 50) + 1) as u8;
        let compid = ((i / 50) % 10) as u8;
        let endpoint = i % 5;

        rt.update(EndpointId(endpoint), sysid, compid, now);

        if i > 0 && i % prune_interval == 0 {
            rt.prune(Duration::from_nanos(1));
        }
    }

    assert!(
        rt.stats().total_routes <= 500,
        "Routes should be bounded after pruning"
    );
}

#[test]
fn test_route_ttl_pruning() {
    let mut rt = RoutingTable::new();

    let old_time = Instant::now();
    rt.update(EndpointId(0), 1, 1, old_time);
    rt.update(EndpointId(0), 2, 1, old_time);

    assert_eq!(rt.stats().total_routes, 2);

    rt.prune(Duration::ZERO);

    assert_eq!(rt.stats().total_routes, 0);
    assert_eq!(rt.stats().total_systems, 0);
    assert_eq!(rt.stats().total_endpoints, 0);
}

// ============================================================================
// Concurrent Access
// ============================================================================

#[test]
fn test_concurrent_updates_high_load() {
    let rt = Arc::new(RwLock::new(RoutingTable::new()));
    let mut handles = vec![];

    // 20 writers
    for i in 0..20 {
        let rt_clone = rt.clone();
        handles.push(thread::spawn(move || {
            for j in 0..1000 {
                let sys = ((j % 50) + 1) as u8;
                let comp = ((j % 20) + 1) as u8;
                let mut lock = rt_clone.write();
                lock.update(EndpointId(i), sys, comp, Instant::now());
                if j % 100 == 0 {
                    drop(lock);
                    thread::sleep(Duration::from_micros(10));
                }
            }
        }));
    }

    // 20 readers
    for i in 0..20 {
        let rt_clone = rt.clone();
        handles.push(thread::spawn(move || {
            for j in 0..1000 {
                let sys = ((j % 50) + 1) as u8;
                let comp = ((j % 20) + 1) as u8;
                let lock = rt_clone.read();
                let _ = lock.should_send(EndpointId(i), sys, comp);
                if j % 100 == 0 {
                    drop(lock);
                    thread::sleep(Duration::from_micros(10));
                }
            }
        }));
    }

    // 1 pruner
    let rt_clone = rt.clone();
    handles.push(thread::spawn(move || {
        for _ in 0..10 {
            thread::sleep(Duration::from_millis(5));
            let mut lock = rt_clone.write();
            lock.prune(Duration::from_millis(100));
        }
    }));

    for handle in handles {
        handle.join().expect("Thread join failed");
    }
    // No deadlocks / panics is the assertion.
}

#[tokio::test]
async fn test_async_concurrent_access() {
    let rt = Arc::new(RwLock::new(RoutingTable::new()));

    let mut handles = vec![];

    for i in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let mut rt_lock = rt_clone.write();
                rt_lock.update(EndpointId(i), (j % 255) as u8, 1, Instant::now());
            }
        }));
    }

    for _ in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let rt_lock = rt_clone.read();
                let _ = rt_lock.should_send(EndpointId(0), (j % 255) as u8, 1);
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ============================================================================
// EndpointId Edge Cases
// ============================================================================

#[test]
fn test_endpoint_id_wrap_behavior() {
    let counter = AtomicUsize::new(usize::MAX - 5);

    let mut ids = Vec::new();
    for _ in 0..10 {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        ids.push(EndpointId(id));
    }

    assert_eq!(ids[5].0, usize::MAX);
    assert_eq!(ids[6].0, 0, "Counter wraps to 0 after usize::MAX");

    let mut rt = RoutingTable::new();
    let now = Instant::now();
    for id in &ids {
        rt.update(*id, 1, 1, now);
    }

    assert_eq!(rt.stats().total_endpoints, 10);
    assert!(rt.should_send(ids[0], 1, 1));
    assert!(rt.should_send(ids[9], 1, 1));
}

#[test]
fn test_tcp_endpoint_id_collision_scenario() {
    // TCP server uses: client_id = base_id * 1000 + counter; collision
    // occurs when endpoint0's counter reaches 1000 and meets endpoint1's
    // first client. Verify routing is merely "see traffic from both
    // systems" — not wrong, just extra matches.
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    for i in 0..1001usize {
        rt.update(EndpointId(i), 1, 1, now);
    }
    rt.update(EndpointId(1000), 2, 1, now);

    assert!(rt.should_send(EndpointId(1000), 1, 1));
    assert!(rt.should_send(EndpointId(1000), 2, 1));
    assert!(rt.stats().total_endpoints <= 1001);
}

#[test]
fn test_max_header_values() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    rt.update(EndpointId(usize::MAX), 255, 255, now);

    let stats = rt.stats();
    assert_eq!(stats.total_systems, 1);
    assert_eq!(stats.total_routes, 1);
    assert!(rt.should_send(EndpointId(usize::MAX), 255, 255));
}

// ============================================================================
// needs_update_for_endpoint Logic
// ============================================================================

#[test]
fn test_needs_update_logic() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    assert!(rt.needs_update_for_endpoint(EndpointId(0), 1, 1, now));
    rt.update(EndpointId(0), 1, 1, now);
    assert!(!rt.needs_update_for_endpoint(EndpointId(0), 1, 1, now));
    assert!(rt.needs_update_for_endpoint(EndpointId(1), 1, 1, now));
    assert!(rt.needs_update_for_endpoint(EndpointId(0), 2, 1, now));
}
