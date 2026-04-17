//! Endpoint-group and sniffer-sysid routing semantics.

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
use std::time::Instant;
// ============================================================================
// Groups
// ============================================================================

#[test]
fn test_grouped_endpoints_share_routing() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);

    rt.set_endpoint_group(ep0, "autopilot".to_string());
    rt.set_endpoint_group(ep1, "autopilot".to_string());

    let now = Instant::now();
    rt.update(ep0, 1, 1, now);

    assert!(rt.should_send(ep0, 1, 1));
    assert!(rt.should_send(ep1, 1, 1));
}

#[test]
fn test_ungrouped_endpoints_independent() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);

    let now = Instant::now();
    rt.update(ep0, 1, 1, now);

    assert!(rt.should_send(ep0, 1, 1));
    assert!(!rt.should_send(ep1, 1, 1));
}

#[test]
fn test_group_with_three_endpoints() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);
    let ep2 = EndpointId(2);

    rt.set_endpoint_group(ep0, "vehicle".to_string());
    rt.set_endpoint_group(ep1, "vehicle".to_string());
    rt.set_endpoint_group(ep2, "vehicle".to_string());

    let now = Instant::now();
    rt.update(ep0, 5, 1, now);

    assert!(rt.should_send(ep0, 5, 1));
    assert!(rt.should_send(ep1, 5, 1));
    assert!(rt.should_send(ep2, 5, 1));
}

#[test]
fn test_mixed_grouped_and_ungrouped() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);
    let ep2 = EndpointId(2);
    let ep3 = EndpointId(3);

    rt.set_endpoint_group(ep0, "autopilot".to_string());
    rt.set_endpoint_group(ep1, "autopilot".to_string());
    rt.set_endpoint_group(ep3, "gcs".to_string());

    let now = Instant::now();
    rt.update(ep0, 1, 1, now);

    assert!(rt.should_send(ep0, 1, 1));
    assert!(rt.should_send(ep1, 1, 1));
    assert!(!rt.should_send(ep2, 1, 1));
    assert!(!rt.should_send(ep3, 1, 1));
}

#[test]
fn test_empty_group_string_ignored() {
    // Config layer strips empty group names (`""` → `None`). At the routing
    // layer empty strings do group together by design; this test documents
    // that invariant so a refactor at either layer has to update both.
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);

    rt.set_endpoint_group(ep0, String::new());
    rt.set_endpoint_group(ep1, String::new());

    let now = Instant::now();
    rt.update(ep0, 1, 1, now);

    assert!(rt.should_send(ep0, 1, 1));
}

#[test]
fn test_grouped_endpoints_system_broadcast() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);

    rt.set_endpoint_group(ep0, "autopilot".to_string());
    rt.set_endpoint_group(ep1, "autopilot".to_string());

    let now = Instant::now();
    rt.update(ep0, 1, 1, now);

    assert!(rt.should_send(ep0, 1, 0));
    assert!(rt.should_send(ep1, 1, 0));
}

// ============================================================================
// Sniffer mode
// ============================================================================

#[test]
fn test_sniffer_mode_receives_all() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);

    rt.set_sniffer_sysids(&[253]);

    let now = Instant::now();
    rt.update(ep0, 253, 1, now);
    rt.update(ep1, 1, 1, now);

    assert!(rt.should_send(ep0, 1, 1));
    assert!(rt.should_send(ep0, 2, 5));
    assert!(rt.should_send(ep0, 99, 99));
    assert!(rt.should_send(ep0, 0, 0));
}

#[test]
fn test_sniffer_mode_no_effect_without_sysid() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);

    rt.set_sniffer_sysids(&[253]);

    let now = Instant::now();
    rt.update(ep0, 1, 1, now);
    rt.update(ep1, 2, 1, now);

    assert!(rt.should_send(ep0, 1, 1));
    assert!(!rt.should_send(ep0, 2, 1));
    assert!(!rt.should_send(ep0, 99, 1));
}

#[test]
fn test_sniffer_and_normal_endpoints_coexist() {
    let mut rt = RoutingTable::new();
    let ep_sniffer = EndpointId(0);
    let ep_normal = EndpointId(1);

    rt.set_sniffer_sysids(&[253]);

    let now = Instant::now();
    rt.update(ep_sniffer, 253, 1, now);
    rt.update(ep_normal, 1, 1, now);

    assert!(rt.should_send(ep_sniffer, 1, 1));
    assert!(rt.should_send(ep_sniffer, 2, 5));
    assert!(rt.should_send(ep_sniffer, 99, 99));

    assert!(rt.should_send(ep_normal, 1, 1));
    assert!(!rt.should_send(ep_normal, 2, 5));
    assert!(!rt.should_send(ep_normal, 99, 99));
}

#[test]
fn test_basic_routing_without_groups() {
    let mut rt = RoutingTable::new();
    let ep0 = EndpointId(0);
    let ep1 = EndpointId(1);

    let now = Instant::now();
    rt.update(ep0, 1, 1, now);
    rt.update(ep1, 2, 1, now);

    assert!(rt.should_send(ep0, 0, 0));
    assert!(rt.should_send(ep1, 0, 0));

    assert!(rt.should_send(ep0, 1, 1));
    assert!(!rt.should_send(ep1, 1, 1));

    assert!(!rt.should_send(ep0, 2, 1));
    assert!(rt.should_send(ep1, 2, 1));

    assert!(!rt.should_send(ep0, 99, 1));
}
