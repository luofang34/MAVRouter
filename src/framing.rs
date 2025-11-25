//! MAVLink framing and parsing utilities.
//!
//! This module provides tools for extracting complete MAVLink messages
//! from a raw byte stream, handling both MAVLink v1 and v2 formats.
//! It includes a `StreamParser` to manage incoming byte buffers and
//! reconstruct messages, and a `MavlinkFrame` to represent a parsed message
//! with its header and protocol version.

use bytes::{Buf, BytesMut};
use mavlink::{MavHeader, MavlinkVersion};
use memchr::memchr2; // Added memchr2 import
use std::io::Cursor;
use tracing::warn;

// Maximum buffer size to prevent OOM from malformed streams
const MAX_BUFFER_SIZE: usize = 1024 * 1024; // 1MB

/// Represents a completely parsed MAVLink message, including its header and protocol version.
pub struct MavlinkFrame {
    /// The MAVLink message header.
    pub header: MavHeader,
    /// The decoded MAVLink message payload.
    pub message: mavlink::common::MavMessage,
    /// The MAVLink protocol version (v1 or v2).
    pub version: MavlinkVersion,
}

/// A stateful parser for extracting MAVLink frames from an asynchronous byte stream.
///
/// This parser accumulates incoming bytes and attempts to reconstruct
/// valid MAVLink v1 or v2 messages. It handles partial packets and
/// gracefully deals with malformed data by skipping invalid bytes.
pub struct StreamParser {
    /// Internal buffer to store incoming bytes.
    buffer: BytesMut,
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamParser {
    /// Creates a new `StreamParser` with an empty internal buffer.
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(4096),
        }
    }

    /// Appends new data to the internal buffer.
    ///
    /// If adding the new data would exceed `MAX_BUFFER_SIZE`, the buffer
    /// is cleared to prevent excessive memory usage from potentially
    /// malformed or continuously streaming data.
    ///
    /// # Arguments
    ///
    /// * `data` - The byte slice to append.
    pub fn push(&mut self, data: &[u8]) {
        let new_len = self.buffer.len() + data.len();
        if new_len > MAX_BUFFER_SIZE {
            let overflow = new_len - MAX_BUFFER_SIZE;
            warn!(
                "StreamParser buffer full, dropping {} oldest bytes to make room",
                overflow
            );

            if overflow <= self.buffer.len() {
                self.buffer.advance(overflow);
            } else {
                self.buffer.clear();
            }
        }
        self.buffer.extend_from_slice(data);
    }

    /// Attempts to parse the next complete MAVLink frame from the internal buffer.
    ///
    /// This method searches for MAVLink start-of-frame bytes (0xFE for v1, 0xFD for v2)
    /// and tries to decode a full message. If a partial message is found, it returns `None`
    /// and waits for more data. If malformed data is encountered, it skips invalid bytes
    /// until a potential start-of-frame is found.
    ///
    /// # Returns
    ///
    /// An `Option` containing a `MavlinkFrame` if a complete message is successfully parsed,
    /// or `None` if no complete message is currently available or an EOF condition is met.
    pub fn parse_next(&mut self) -> Option<MavlinkFrame> {
        loop {
            if self.buffer.is_empty() {
                return None;
            }

            // 1. Search for STX (Magic Byte) using memchr2 for performance
            let start_idx = memchr2(0xFD, 0xFE, &self.buffer);

            if let Some(idx) = start_idx {
                if idx > 0 {
                    self.buffer.advance(idx);
                }
            } else {
                // No STX found, clear buffer to prevent overflow if no valid MAVLink is seen
                self.buffer.clear();
                return None;
            }

            // 2. Try to parse message at current position (0)
            let mut cursor = Cursor::new(&self.buffer[..]);
            let start_pos = cursor.position();

            // Try V2
            let res_v2 = mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut cursor);

            match res_v2 {
                Ok((header, message)) => {
                    let len = cursor.position() as usize;
                    self.buffer.advance(len);
                    return Some(MavlinkFrame {
                        header,
                        message,
                        version: MavlinkVersion::V2,
                    });
                }
                Err(e) => {
                    // Try V1
                    cursor.set_position(start_pos);
                    let res_v1 =
                        mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut cursor);

                    match res_v1 {
                        Ok((header, message)) => {
                            let len = cursor.position() as usize;
                            self.buffer.advance(len);
                            return Some(MavlinkFrame {
                                header,
                                message,
                                version: MavlinkVersion::V1,
                            });
                        }
                        Err(e_v1) => {
                            // If either parser returned UnexpectedEof, it means we need more data.
                            // Otherwise, it's a parse error, so we discard the current byte
                            // and continue searching for the next STX.
                            if is_eof(&e) || is_eof(&e_v1) {
                                return None;
                            }
                            // Invalid packet, skip STX and continue
                            self.buffer.advance(1);
                            continue;
                        }
                    }
                }
            }
        }
    }
}

/// Checks if a `mavlink::error::MessageReadError` indicates an UnexpectedEof.
///
/// This is used internally by `StreamParser` to differentiate between needing
/// more data and encountering a malformed message.
fn is_eof(e: &mavlink::error::MessageReadError) -> bool {
    match e {
        mavlink::error::MessageReadError::Io(io_err) => {
            io_err.kind() == std::io::ErrorKind::UnexpectedEof
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use mavlink::common::MavMessage;
    use mavlink::Message;

    #[test]
    fn test_partial_packet() {
        let mut parser = StreamParser::new();
        let header = MavHeader::default();
        let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());

        let mut buf = Vec::new();
        mavlink::write_v2_msg(&mut buf, header, &msg).expect("Failed to write test message");

        let split_idx = buf.len() / 2;
        parser.push(&buf[..split_idx]);
        assert!(parser.parse_next().is_none());

        parser.push(&buf[split_idx..]);
        let res = parser.parse_next();
        assert!(res.is_some());
        assert_eq!(
            res.expect("Should have parsed packet").message.message_id(),
            0
        );
    }

    #[test]
    fn test_v1_packet_parsing() {
        let mut parser = StreamParser::new();
        let header = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());

        let mut buf = Vec::new();
        mavlink::write_v1_msg(&mut buf, header, &msg).expect("Failed to write V1 message");

        parser.push(&buf);
        let res = parser.parse_next();
        assert!(res.is_some());
        let frame = res.expect("Should parse V1 packet");
        assert_eq!(frame.version, MavlinkVersion::V1);
        assert_eq!(frame.header.system_id, 1);
    }

    #[test]
    fn test_v2_packet_parsing() {
        let mut parser = StreamParser::new();
        let header = MavHeader {
            system_id: 255,
            component_id: 190,
            sequence: 42,
        };
        let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());

        let mut buf = Vec::new();
        mavlink::write_v2_msg(&mut buf, header, &msg).expect("Failed to write V2 message");

        parser.push(&buf);
        let res = parser.parse_next();
        assert!(res.is_some());
        let frame = res.expect("Should parse V2 packet");
        assert_eq!(frame.version, MavlinkVersion::V2);
        assert_eq!(frame.header.system_id, 255);
        assert_eq!(frame.header.component_id, 190);
    }

    #[test]
    fn test_garbage_before_packet() {
        let mut parser = StreamParser::new();
        let header = MavHeader::default();
        let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());

        let mut buf = Vec::new();
        mavlink::write_v2_msg(&mut buf, header, &msg).expect("Failed to write message");

        // Prepend garbage bytes
        let mut garbage = vec![0x00, 0x11, 0x22, 0x33, 0x44];
        garbage.extend_from_slice(&buf);

        parser.push(&garbage);
        let res = parser.parse_next();
        assert!(res.is_some(), "Should skip garbage and find packet");
    }

    #[test]
    fn test_empty_buffer_returns_none() {
        let mut parser = StreamParser::new();
        assert!(parser.parse_next().is_none());
    }

    #[test]
    fn test_no_stx_clears_buffer() {
        let mut parser = StreamParser::new();
        // Push data with no valid STX (0xFD or 0xFE)
        parser.push(&[0x00, 0x11, 0x22, 0x33]);
        assert!(parser.parse_next().is_none());
        // Buffer should be cleared since no STX found
    }

    #[test]
    fn test_multiple_packets_in_sequence() {
        let mut parser = StreamParser::new();
        let header = MavHeader::default();
        let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());

        let mut buf = Vec::new();
        mavlink::write_v2_msg(&mut buf, header, &msg).expect("write msg 1");
        mavlink::write_v2_msg(&mut buf, header, &msg).expect("write msg 2");
        mavlink::write_v2_msg(&mut buf, header, &msg).expect("write msg 3");

        parser.push(&buf);

        assert!(parser.parse_next().is_some());
        assert!(parser.parse_next().is_some());
        assert!(parser.parse_next().is_some());
        assert!(parser.parse_next().is_none());
    }

    #[test]
    fn test_default_trait() {
        let parser = StreamParser::default();
        assert!(parser.buffer.is_empty());
    }
}
