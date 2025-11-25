use bytes::Bytes;
use mavlink::{MavHeader, MavlinkVersion};
use std::fmt;
use tokio::sync::broadcast;

use crate::mavlink_utils::MessageTarget;

/// Unique identifier for a routing endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointId(pub usize);

impl fmt::Display for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Endpoint({})", self.0)
    }
}

/// A routed MAVLink message with source information.
///
/// Optimized for zero-copy forwarding: contains raw bytes and cached metadata
/// instead of the parsed message to avoid heap allocation and Arc overhead.
#[derive(Clone, Debug)]
pub struct RoutedMessage {
    /// Identifier of the source endpoint that received this message.
    pub source_id: EndpointId,
    /// MAVLink message header containing system_id, component_id, and sequence number.
    pub header: MavHeader,
    /// Cached MAVLink message ID (avoids calling message.message_id() per endpoint).
    pub message_id: u32,
    /// The MAVLink protocol version used for this message (V1 or V2).
    #[allow(dead_code)]
    pub version: MavlinkVersion,
    /// Arrival timestamp in microseconds since UNIX EPOCH.
    pub timestamp_us: u64,
    /// Cached serialized message bytes (zero-copy from parser).
    pub serialized_bytes: Bytes,
    /// Cached target information to avoid repeated extract_target calls across endpoints.
    pub target: MessageTarget,
}

/// Type alias for the message bus sender.
///
/// This is a Tokio `broadcast::Sender` used to distribute `RoutedMessage`s
/// to all active endpoints and internal router components.
pub type MessageBus = broadcast::Sender<RoutedMessage>;

/// Creates a new message bus with the specified capacity.
///
/// The message bus is a Tokio `broadcast` channel, which allows multiple
/// receivers to subscribe and receive copies of messages sent through it.
///
/// # Arguments
///
/// * `capacity` - The maximum number of messages that can be buffered
///   if receivers are slow. If `capacity` is exceeded, older messages
///   will be dropped for slower receivers.
///
/// # Returns
///
/// A `MessageBus` (Tokio `broadcast::Sender`) instance.
///
/// # Example
///
/// ```
/// use mavrouter_rs::router;
///
/// let bus = router::create_bus(1000);
/// // Endpoints can now subscribe to this bus
/// ```
pub fn create_bus(capacity: usize) -> MessageBus {
    let (tx, _) = broadcast::channel(capacity);
    tx
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bus_filtering() {
        let bus = create_bus(1000); // Use a default capacity for test
        let mut rx = bus.subscribe();

        let msg = RoutedMessage {
            source_id: EndpointId(1),
            header: MavHeader::default(),
            message_id: 0, // HEARTBEAT message ID
            version: MavlinkVersion::V2,
            timestamp_us: 0,
            serialized_bytes: Bytes::new(),
            target: MessageTarget {
                system_id: 0,
                component_id: 0,
            },
        };

        bus.send(msg.clone()).expect("Failed to send test message");

        let received = rx.recv().await.expect("Failed to receive test message");
        assert_eq!(received.source_id, EndpointId(1));
    }
}
