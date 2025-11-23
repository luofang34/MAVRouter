use mavlink::{common::MavMessage, MavHeader, MavlinkVersion};
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct RoutedMessage {
    pub source_id: usize,
    pub header: MavHeader,
    pub message: MavMessage,
    pub version: MavlinkVersion,
}

pub type MessageBus = broadcast::Sender<RoutedMessage>;

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
            message: MavMessage::HEARTBEAT(HEARTBEAT_DATA::default()),
            version: MavlinkVersion::V2,
        };

        bus.send(msg.clone()).expect("Failed to send test message");

        let received = rx.recv().await.expect("Failed to receive test message");
        assert_eq!(received.source_id, 1);
    }
}
