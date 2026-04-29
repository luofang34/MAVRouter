#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use super::helpers::make_core;
use crate::filter::EndpointFilters;
use crate::mavlink_utils::MessageTarget;
use crate::router::{create_bus, EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use ahash::AHashSet as HashSet;
use bytes::Bytes;
use mavlink::{MavHeader, MavlinkVersion};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn test_check_outgoing_self_origin_rejected() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RoutingTable::new());
    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    let msg = RoutedMessage {
        source_id: EndpointId(1),
        header: MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        },
        message_id: 0,
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from_static(b"test"),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    assert!(!core.check_outgoing(&msg));
}

#[test]
fn test_check_outgoing_filter_rejection() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RoutingTable::new());

    let filters = EndpointFilters {
        block_msg_id_out: HashSet::from([30]),
        ..Default::default()
    };

    let core = make_core(1, bus.sender(), routing_table, Duration::ZERO, filters);

    let msg = RoutedMessage {
        source_id: EndpointId(2),
        header: MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        },
        message_id: 30,
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from_static(b"test"),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    assert!(!core.check_outgoing(&msg));
}

#[test]
fn test_check_outgoing_pass_through() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RoutingTable::new());

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    let msg = RoutedMessage {
        source_id: EndpointId(2),
        header: MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        },
        message_id: 0,
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from_static(b"test"),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    assert!(core.check_outgoing(&msg));
}
