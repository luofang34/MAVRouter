#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use super::helpers::{make_core, make_heartbeat_frame};
use crate::filter::EndpointFilters;
use crate::router::{create_bus, EndpointId};
use crate::routing::RoutingTable;
use ahash::AHashSet as HashSet;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn test_handle_incoming_happy_path() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(RoutingTable::new());

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));

    let msg = rx.try_recv().expect("Expected a message on the bus");
    assert_eq!(msg.source_id, EndpointId(1));
    assert_eq!(msg.header.system_id, 1);
    assert_eq!(msg.header.component_id, 1);
    assert_eq!(msg.message_id, 0);
}

#[tokio::test]
async fn test_handle_incoming_sysid_zero_rejected() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(RoutingTable::new());

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    core.handle_incoming_frame(make_heartbeat_frame(0, 1, 0));

    assert!(
        rx.try_recv().is_err(),
        "SysID 0 frame should not appear on bus"
    );
}

#[tokio::test]
async fn test_handle_incoming_filter_rejection() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(RoutingTable::new());

    let filters = EndpointFilters {
        block_msg_id_in: HashSet::from([0]),
        ..Default::default()
    };

    let core = make_core(1, bus.sender(), routing_table, Duration::ZERO, filters);

    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));

    assert!(
        rx.try_recv().is_err(),
        "Filtered message should not appear on bus"
    );
}

#[tokio::test]
async fn test_handle_incoming_dedup_rejection() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(RoutingTable::new());

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::from_secs(1),
        EndpointFilters::default(),
    );

    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));
    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));

    assert!(rx.try_recv().is_ok(), "First frame should appear");
    assert!(
        rx.try_recv().is_err(),
        "Duplicate frame should be suppressed"
    );
}
