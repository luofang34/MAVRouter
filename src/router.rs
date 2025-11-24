use mavlink::{common::MavMessage, MavHeader, MavlinkVersion};
use std::sync::Arc;
use tokio::sync::broadcast;

/// A routed MAVLink message with source information.
#[derive(Clone, Debug)]
pub struct RoutedMessage {
    /// Identifier of the source endpoint that received this message.
    pub source_id: usize,
    /// MAVLink message header containing system_id, component_id, and sequence number.
    pub header: MavHeader,
    /// The actual MAVLink message payload.
    pub message: Arc<MavMessage>,
    /// The MAVLink protocol version used for this message (V1 or V2).
    pub version: MavlinkVersion,
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
    use mavlink::common::HEARTBEAT_DATA;

    #[tokio::test]
    async fn test_bus_filtering() {
        let bus = create_bus(1000); // Use a default capacity for test
        let mut rx = bus.subscribe();

        let msg = RoutedMessage {
            source_id: 1,
            header: MavHeader::default(),
            message: Arc::new(MavMessage::HEARTBEAT(HEARTBEAT_DATA::default())),
            version: MavlinkVersion::V2,
        };

        bus.send(msg.clone()).expect("Failed to send test message");

        let received = rx.recv().await.expect("Failed to receive test message");
        assert_eq!(received.source_id, 1);
    }
}
