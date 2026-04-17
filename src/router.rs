use bytes::Bytes;
use mavlink::{MavHeader, MavlinkVersion};
use std::fmt;
use std::sync::Arc;
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
/// Travels through the bus as `Arc<RoutedMessage>` so fan-out cost is one
/// atomic refcount bump per subscriber instead of a full struct clone
/// (which includes heap-backed `Bytes` — cheap to clone on its own, but
/// adds up across many subscribers).
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

/// Message bus handle used to distribute `Arc<RoutedMessage>`s to all active
/// endpoints and internal router components.
///
/// Uses `tokio::sync::broadcast` for lock-free message distribution. New
/// subscribers are created via `sender.subscribe()` (not receiver clone).
/// Carrying `Arc<RoutedMessage>` keeps fan-out O(1) per subscriber — each
/// receiver pays an atomic increment, not a `RoutedMessage` clone.
#[derive(Clone)]
pub struct MessageBus {
    /// Sender half of the bus. Also used to create new subscribers via `subscribe()`.
    pub tx: broadcast::Sender<Arc<RoutedMessage>>,
}

impl MessageBus {
    /// Create a new subscriber to the bus.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<RoutedMessage>> {
        self.tx.subscribe()
    }

    /// Clone the sender half of the bus.
    pub fn sender(&self) -> broadcast::Sender<Arc<RoutedMessage>> {
        self.tx.clone()
    }
}

/// Creates a new message bus with the specified capacity.
///
/// The message bus is a `tokio::sync::broadcast` channel, which allows multiple
/// receivers to subscribe and receive copies of messages sent through it.
/// Slow receivers that fall behind will automatically skip oldest messages
/// (lagged behavior).
///
/// # Arguments
///
/// * `capacity` - The maximum number of messages that can be buffered
///   if receivers are slow. If `capacity` is exceeded, older messages
///   will be dropped for slower receivers.
///
/// # Returns
///
/// A `MessageBus` instance containing the sender (subscribers are created from it).
pub fn create_bus(capacity: usize) -> MessageBus {
    let (tx, _rx) = broadcast::channel(capacity);
    MessageBus { tx }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bus_filtering() {
        let bus = create_bus(1000);
        let mut rx = bus.subscribe();

        let msg = Arc::new(RoutedMessage {
            source_id: EndpointId(1),
            header: MavHeader::default(),
            message_id: 0,
            version: MavlinkVersion::V2,
            timestamp_us: 0,
            serialized_bytes: Bytes::new(),
            target: MessageTarget {
                system_id: 0,
                component_id: 0,
            },
        });

        bus.tx
            .send(Arc::clone(&msg))
            .expect("Failed to send test message");

        let received = rx.recv().await.expect("Failed to receive test message");
        assert_eq!(received.source_id, EndpointId(1));
    }

    /// Sending with no subscribers must not panic when the channel backlog
    /// overflows its bounded capacity.
    #[tokio::test]
    async fn test_message_bus_overflow() {
        let bus = create_bus(10);
        let tx = bus.sender();

        for i in 0..100 {
            let msg = Arc::new(RoutedMessage {
                source_id: EndpointId(0),
                header: MavHeader {
                    system_id: 1,
                    component_id: 1,
                    sequence: i as u8,
                },
                message_id: 0,
                version: MavlinkVersion::V2,
                timestamp_us: 0,
                serialized_bytes: Bytes::from_static(b"test"),
                target: MessageTarget {
                    system_id: 0,
                    component_id: 0,
                },
            });
            tx.send(msg).ok();
        }
        // Reaching here without panic is the assertion.
    }
}
