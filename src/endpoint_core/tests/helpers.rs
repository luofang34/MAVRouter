#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use crate::endpoint_core::{EndpointCore, EndpointStats};
use crate::filter::EndpointFilters;
use crate::framing::{MavlinkFrame, StreamParser};
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::{RouteUpdate, RoutingTable};
use mavlink::{common::MavMessage, MavHeader};
use std::sync::Arc;
use std::time::Duration;

/// Build a valid MAVLink v2 HEARTBEAT frame and return it as a `MavlinkFrame`,
/// constructed via the real `StreamParser` so `raw_bytes` is populated
/// consistently with the production ingress path.
pub(super) fn make_heartbeat_frame(system_id: u8, component_id: u8, sequence: u8) -> MavlinkFrame {
    let header = MavHeader {
        system_id,
        component_id,
        sequence,
    };
    let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).expect("write v2");

    let mut parser = StreamParser::new();
    parser.push(&buf);
    parser.parse_next().expect("should parse heartbeat frame")
}

/// Build an `EndpointCore` with an isolated mpsc route-update channel whose
/// receiver is dropped — tests exercising `handle_incoming_frame` don't care
/// whether the route actually lands in a live updater, only that the
/// non-blocking submission path works.
pub(super) fn make_core(
    id: usize,
    bus_tx: tokio::sync::broadcast::Sender<Arc<RoutedMessage>>,
    routing_table: Arc<RoutingTable>,
    dedup_period: Duration,
    filters: EndpointFilters,
) -> EndpointCore {
    use crate::dedup::ConcurrentDedup;
    let (route_update_tx, _route_update_rx) = tokio::sync::mpsc::channel::<RouteUpdate>(16);
    EndpointCore {
        id: EndpointId(id),
        bus_tx,
        routing_table,
        route_update_tx,
        dedup: ConcurrentDedup::new(dedup_period),
        filters,
        update_routing: true,
        stats: Arc::new(EndpointStats::new()),
    }
}
