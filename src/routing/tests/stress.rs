//! CPU-scaled stress / churn tests for the routing table.

//! - capacity limits and TTL pruning
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::print_stdout)]

use crate::router::EndpointId;
use crate::routing::*;
use std::sync::Arc;
use std::time::Instant;
// ============================================================================
// Stress (scaled by CPU count via env var)
// ============================================================================

fn stress_iterations() -> usize {
    if let Ok(s) = std::env::var("CI_STRESS_ITERATIONS") {
        return s.parse().expect("CI_STRESS_ITERATIONS must be a number");
    }

    let cpus = num_cpus::get();
    if cpus >= 64 {
        10_000_000
    } else if cpus >= 8 {
        1_000_000
    } else if cpus >= 4 {
        500_000
    } else {
        100_000
    }
}

#[test]
fn test_routing_table_stress_functional() {
    let rt = RoutingTable::new();

    for endpoint in 0..10 {
        for sys in 1..=100 {
            for comp in 1..=10 {
                rt.update(EndpointId(endpoint), sys, comp, Instant::now());
            }
        }
    }

    let iterations = stress_iterations();
    for i in 0..iterations {
        let sys = ((i % 100) + 1) as u8;
        let comp = ((i % 10) + 1) as u8;
        let endpoint = i % 10;
        assert!(
            rt.should_send(EndpointId(endpoint), sys, comp),
            "Route should exist for endpoint {} sys {} comp {}",
            endpoint,
            sys,
            comp
        );
    }
}

#[tokio::test]
async fn test_routing_table_concurrent_access_async() {
    let rt = Arc::new(RoutingTable::new());

    let mut handles = vec![];

    for i in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                rt_clone.update(EndpointId(i), (j % 255) as u8, 1, Instant::now());
            }
        }));
    }

    for _ in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let _ = rt_clone.should_send(EndpointId(0), (j % 255) as u8, 1);
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}
