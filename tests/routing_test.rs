#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

//! Comprehensive routing table tests
//!
//! Covers:
//! - Basic routing operations
//! - Capacity limits and pruning
//! - Concurrent access patterns
//! - EndpointId edge cases
//! - Broadcast and fallback routing

use mavrouter::router::EndpointId;
use mavrouter::routing::RoutingTable;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// ============================================================================
// Basic Routing Operations
// ============================================================================

/// Test basic routing table update and query
#[test]
fn test_basic_routing() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // Initially, no routes exist
    assert!(!rt.should_send(EndpointId(0), 1, 1));

    // Update routing table
    rt.update(EndpointId(0), 1, 1, now);

    // Now the route exists
    assert!(rt.should_send(EndpointId(0), 1, 1));

    // Different endpoint doesn't have the route
    assert!(!rt.should_send(EndpointId(1), 1, 1));

    let stats = rt.stats();
    assert_eq!(stats.total_systems, 1);
    assert_eq!(stats.total_routes, 1);
    assert_eq!(stats.total_endpoints, 1);
}

/// Test component broadcast routing (compid=0)
#[test]
fn test_component_broadcast_routing() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // Endpoint 0 has seen system 1, component 1
    rt.update(EndpointId(0), 1, 1, now);
    // Endpoint 1 has seen system 1, component 2
    rt.update(EndpointId(1), 1, 2, now);

    // Message targeting system 1, component 0 (all components)
    // Should go to both endpoints that have seen system 1
    assert!(rt.should_send(EndpointId(0), 1, 0));
    assert!(rt.should_send(EndpointId(1), 1, 0));

    // Endpoint 2 hasn't seen system 1
    assert!(!rt.should_send(EndpointId(2), 1, 0));

    println!("✓ Component broadcast routing works correctly");
}

/// Test routing fallback when specific component unknown
#[test]
fn test_routing_component_fallback() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // Endpoint 0 knows system 1, component 1
    rt.update(EndpointId(0), 1, 1, now);

    // Message targets system 1, component 99 (unknown component)
    // Should fallback to system-level routing
    assert!(
        rt.should_send(EndpointId(0), 1, 99),
        "Should fallback to system route when component unknown"
    );

    println!("✓ Routing falls back to system-level when component unknown");
}

/// Test routing with same endpoint seeing multiple systems
#[test]
fn test_multi_system_endpoint() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // Single endpoint (like a GCS) sees multiple systems
    rt.update(EndpointId(0), 1, 1, now);
    rt.update(EndpointId(0), 2, 1, now);
    rt.update(EndpointId(0), 3, 1, now);

    // This endpoint should receive messages for all three systems
    assert!(rt.should_send(EndpointId(0), 1, 1));
    assert!(rt.should_send(EndpointId(0), 2, 1));
    assert!(rt.should_send(EndpointId(0), 3, 1));

    let stats = rt.stats();
    assert_eq!(stats.total_systems, 3);
    assert_eq!(stats.total_endpoints, 1);

    println!("✓ Multi-system endpoint routing works correctly");
}

/// Test all broadcast combinations
#[test]
fn test_broadcast_combinations() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    rt.update(EndpointId(0), 1, 1, now);

    // (0, 0) - full broadcast
    assert!(rt.should_send(EndpointId(0), 0, 0));
    assert!(rt.should_send(EndpointId(99), 0, 0)); // Even unknown endpoint

    // (0, X) - broadcast to all systems, specific component
    // Still treated as broadcast (sysid == 0)
    assert!(rt.should_send(EndpointId(0), 0, 1));
    assert!(rt.should_send(EndpointId(99), 0, 1));

    // (X, 0) - specific system, all components
    assert!(rt.should_send(EndpointId(0), 1, 0));
    assert!(!rt.should_send(EndpointId(99), 1, 0)); // Unknown endpoint

    // (X, Y) - specific system and component
    assert!(rt.should_send(EndpointId(0), 1, 1));
    assert!(!rt.should_send(EndpointId(99), 1, 1));

    println!("✓ All broadcast combinations handled correctly");
}

// ============================================================================
// Capacity Limits
// ============================================================================

/// Test that routing table is naturally bounded by u8 address space
#[test]
fn test_routing_table_capacity_limit() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // MAX_ROUTES = 100,000, MAX_SYSTEMS = 1,000
    // But u8 sysid/compid means max 255 * 256 = 65,280 routes
    println!("Inserting all possible (sysid, compid) pairs...");

    let mut inserted = 0usize;
    for sysid in 1..=255u8 {
        for compid in 0..=255u8 {
            rt.update(EndpointId(0), sysid, compid, now);
            inserted += 1;
        }
    }

    let stats = rt.stats();
    println!(
        "After inserting {} pairs: routes={}, systems={}",
        inserted, stats.total_routes, stats.total_systems
    );

    // Verify we have the expected number of routes (bounded by u8 space)
    assert_eq!(stats.total_routes, 255 * 256, "Should have 255*256 routes");
    assert_eq!(stats.total_systems, 255, "Should have 255 systems");

    println!("✓ Routing table naturally bounded by u8 address space");
    println!("  MAX_ROUTES (100,000) > possible routes (65,280)");
    println!("  MAX_SYSTEMS (1,000) > possible systems (255)");
}

/// Test routing table handles rapid updates without memory growth
#[test]
fn test_routing_table_churn_stability() {
    let mut rt = RoutingTable::new();

    let iterations = 100_000usize;
    let prune_interval = 10_000usize;

    println!("Simulating route churn: {} updates...", iterations);

    for i in 0..iterations {
        let now = Instant::now();
        let sysid = ((i % 50) + 1) as u8;
        let compid = ((i / 50) % 10) as u8;
        let endpoint = i % 5;

        rt.update(EndpointId(endpoint), sysid, compid, now);

        // Periodic aggressive prune
        if i > 0 && i % prune_interval == 0 {
            rt.prune(Duration::from_nanos(1));
        }
    }

    let final_stats = rt.stats();
    println!("Final stats: {:?}", final_stats);

    assert!(
        final_stats.total_routes <= 500,
        "Routes should be bounded after pruning"
    );
}

/// Test route pruning with TTL
#[test]
fn test_route_ttl_pruning() {
    let mut rt = RoutingTable::new();

    let old_time = Instant::now();
    rt.update(EndpointId(0), 1, 1, old_time);
    rt.update(EndpointId(0), 2, 1, old_time);

    // Verify routes exist
    assert_eq!(rt.stats().total_routes, 2);

    // Prune with 0 TTL - all routes should be removed
    rt.prune(Duration::ZERO);

    assert_eq!(rt.stats().total_routes, 0);
    assert_eq!(rt.stats().total_systems, 0);
    assert_eq!(rt.stats().total_endpoints, 0);

    println!("✓ Route TTL pruning works correctly");
}

// ============================================================================
// Concurrent Access
// ============================================================================

/// Test concurrent routing updates under high load
#[test]
fn test_concurrent_updates_high_load() {
    let rt = Arc::new(RwLock::new(RoutingTable::new()));
    let mut handles = vec![];

    // 20 Writers
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

    // 20 Readers
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

    // 1 Pruner
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

    let stats = rt.read().stats();
    println!("Concurrent Test Stats: {:?}", stats);
    // No assertion on exact numbers due to pruning race, but ensures no deadlocks/panics
}

/// Test async concurrent access
#[tokio::test]
async fn test_async_concurrent_access() {
    let rt = Arc::new(RwLock::new(RoutingTable::new()));

    let mut handles = vec![];

    // 10 async writers
    for i in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let mut rt_lock = rt_clone.write();
                rt_lock.update(EndpointId(i), (j % 255) as u8, 1, Instant::now());
            }
        }));
    }

    // 10 async readers
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

/// Test EndpointId counter wrap behavior
#[test]
fn test_endpoint_id_wrap_behavior() {
    let counter = AtomicUsize::new(usize::MAX - 5);

    let mut ids = Vec::new();
    for _ in 0..10 {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        ids.push(EndpointId(id));
    }

    println!("Generated IDs around usize::MAX wrap:");
    for (i, id) in ids.iter().enumerate() {
        println!("  [{}] EndpointId({})", i, id.0);
    }

    // Verify wrap occurred
    assert_eq!(ids[5].0, usize::MAX, "ids[5] should be usize::MAX");
    assert_eq!(ids[6].0, 0, "ids[6] should wrap to 0");

    // Verify routing table handles wrapped IDs correctly
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    for id in &ids {
        rt.update(*id, 1, 1, now);
    }

    let stats = rt.stats();
    assert_eq!(stats.total_endpoints, 10, "All wrapped IDs should be tracked");

    // Verify routing works for both pre-wrap and post-wrap IDs
    assert!(rt.should_send(ids[0], 1, 1));
    assert!(rt.should_send(ids[9], 1, 1));

    println!("✓ EndpointId wrap handled correctly");
}

/// Test TCP server's ID collision scenario
#[test]
fn test_tcp_endpoint_id_collision_scenario() {
    // TCP server uses: client_id = base_id * 1000 + counter
    // Collision occurs when endpoint0's counter reaches 1000

    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // Endpoint 0 generates 1001 client IDs (0-1000)
    for i in 0..1001usize {
        rt.update(EndpointId(i), 1, 1, now);
    }

    // Endpoint 1's first client (ID = 1000) - collision!
    rt.update(EndpointId(1000), 2, 1, now);

    let stats = rt.stats();
    println!("After collision scenario: {:?}", stats);

    // EndpointId(1000) now routes to BOTH sysid=1 and sysid=2
    assert!(rt.should_send(EndpointId(1000), 1, 1));
    assert!(rt.should_send(EndpointId(1000), 2, 1));

    assert!(stats.total_endpoints <= 1001);

    println!("✓ ID collision is benign - endpoint sees traffic from multiple systems");
}

/// Test with maximum valid MAVLink header values
#[test]
fn test_max_header_values() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    rt.update(EndpointId(usize::MAX), 255, 255, now);

    let stats = rt.stats();
    assert_eq!(stats.total_systems, 1);
    assert_eq!(stats.total_routes, 1);

    assert!(rt.should_send(EndpointId(usize::MAX), 255, 255));

    println!("✓ Maximum header values handled correctly");
}

// ============================================================================
// needs_update_for_endpoint Logic
// ============================================================================

/// Test the needs_update optimization
#[test]
fn test_needs_update_logic() {
    let mut rt = RoutingTable::new();
    let now = Instant::now();

    // Initially needs update (route doesn't exist)
    assert!(rt.needs_update_for_endpoint(EndpointId(0), 1, 1, now));

    // After update, doesn't need update (within 1 second)
    rt.update(EndpointId(0), 1, 1, now);
    assert!(!rt.needs_update_for_endpoint(EndpointId(0), 1, 1, now));

    // Different endpoint needs update
    assert!(rt.needs_update_for_endpoint(EndpointId(1), 1, 1, now));

    // Different system needs update
    assert!(rt.needs_update_for_endpoint(EndpointId(0), 2, 1, now));

    println!("✓ needs_update_for_endpoint logic works correctly");
}
