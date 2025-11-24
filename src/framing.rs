use mavlink::{MavHeader, MavlinkVersion};
use bytes::{BytesMut, Buf};
use std::io::Cursor;
use tracing::warn;

// Maximum buffer size to prevent OOM from malformed streams
const MAX_BUFFER_SIZE: usize = 1024 * 1024; // 1MB

pub struct MavlinkFrame {
    pub header: MavHeader,
    pub message: mavlink::common::MavMessage,
    pub version: MavlinkVersion,
}

pub struct StreamParser {
    buffer: BytesMut,
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamParser {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(4096),
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        // Clear buffer if adding new data would exceed the limit
        if self.buffer.len() + data.len() > MAX_BUFFER_SIZE {
            warn!("StreamParser buffer exceeded MAX_BUFFER_SIZE. Clearing buffer to prevent OOM.");
            self.buffer.clear();
        }
        self.buffer.extend_from_slice(data);
    }

    pub fn parse_next(&mut self) -> Option<MavlinkFrame> {
        loop {
            if self.buffer.is_empty() {
                return None;
            }

            // 1. Search for STX (Magic Byte)
            let mut start_idx = None;
            for (i, &b) in self.buffer.iter().enumerate() {
                // MAVLink 1: 0xFE, MAVLink 2: 0xFD
                if b == 0xFD || b == 0xFE {
                    start_idx = Some(i);
                    break;
                }
            }

            if let Some(idx) = start_idx {
                if idx > 0 {
                    self.buffer.advance(idx);
                }
            } else {
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
                    let res_v1 = mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut cursor);
                    
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
                            if is_eof(&e) || is_eof(&e_v1) {
                                return None;
                            }
                            // Invalid packet, skip STX
                            self.buffer.advance(1);
                            continue;
                        }
                    }
                }
            }
        }
    }
}

fn is_eof(e: &mavlink::error::MessageReadError) -> bool {
    match e {
        mavlink::error::MessageReadError::Io(io_err) => {
            io_err.kind() == std::io::ErrorKind::UnexpectedEof
        },
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
        assert_eq!(res.expect("Should have parsed packet").message.message_id(), 0);
    }
}
