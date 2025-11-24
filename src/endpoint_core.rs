use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use crate::framing::{MavlinkFrame, StreamParser};
use crate::mavlink_utils::extract_target;
use crate::router::RoutedMessage;
use crate::routing::RoutingTable;
use crate::{lock_mutex, lock_read, lock_write}; // Import macros
use anyhow::Result;
use mavlink::{MavlinkVersion, Message};
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn, trace};

#[derive(Clone)]
pub struct EndpointCore {
    pub id: usize,
    pub bus_tx: broadcast::Sender<RoutedMessage>,
    pub routing_table: Arc<RwLock<RoutingTable>>,
    pub dedup: Arc<Mutex<Dedup>>,
    pub filters: EndpointFilters,
}

impl EndpointCore {
    pub fn handle_incoming_frame(&self, frame: MavlinkFrame) {
        let mut temp_buf = Vec::new();
        if let Err(e) = match frame.version {
            MavlinkVersion::V2 => mavlink::write_v2_msg(&mut temp_buf, frame.header, &frame.message),
            MavlinkVersion::V1 => mavlink::write_v1_msg(&mut temp_buf, frame.header, &frame.message),
        } {
            warn!("Inbound Serialize Error: {}", e);
            return;
        }

        let is_dup = {
            let mut dd = lock_mutex!(self.dedup);
            dd.is_duplicate(&temp_buf)
        };

        if !is_dup && self.filters.check_incoming(&frame.header, frame.message.message_id()) {
            {
                let mut rt = lock_write!(self.routing_table);
                rt.update(self.id, frame.header.system_id, frame.header.component_id);
            }
            if let Err(e) = self.bus_tx.send(RoutedMessage {
                source_id: self.id,
                header: frame.header,
                message: frame.message,
                version: frame.version,
            }) {
                debug!("Bus send error: {}", e);
            }
        }
    }

    pub fn check_outgoing(&self, msg: &RoutedMessage) -> bool {
        if msg.source_id == self.id {
            return false;
        }

        if !self.filters.check_outgoing(&msg.header, msg.message.message_id()) {
            return false;
        }

        let target = extract_target(&msg.message);
        let rt = lock_read!(self.routing_table);
        let should_send = rt.should_send(self.id, target.system_id, target.component_id);

        if !should_send && target.system_id != 0 {
            trace!(
                endpoint_id = self.id,
                target_sys = target.system_id,
                target_comp = target.component_id,
                msg_id = msg.message.message_id(),
                "Routing decision: DROP (no route)"
            );
        }

        should_send
    }
}

pub async fn run_stream_loop<R, W>(
    mut reader: R,
    mut writer: W,
    mut bus_rx: broadcast::Receiver<RoutedMessage>,
    core: EndpointCore,
    cancel_token: CancellationToken,
    name: String,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let core_read = core.clone();
    let name_read = name.clone();
    
    let reader_loop = async move {
        let mut parser = StreamParser::new();
        let mut buf = [0u8; 4096];

        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    parser.push(&buf[..n]);
                    while let Some(frame) = parser.parse_next() {
                        core_read.handle_incoming_frame(frame);
                    }
                }
                Err(e) => {
                    error!("{} read error: {}", name_read, e);
                    break;
                }
            }
        }
    };

    let writer_loop = async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if !core.check_outgoing(&msg) {
                        continue;
                    }

                    let mut buf = Vec::new();
                    if let Err(e) = match msg.version {
                        MavlinkVersion::V2 => mavlink::write_v2_msg(&mut buf, msg.header, &msg.message),
                        MavlinkVersion::V1 => mavlink::write_v1_msg(&mut buf, msg.header, &msg.message),
                    } {
                        warn!("{} Serialize Error: {}", name, e);
                        continue;
                    }

                    if let Err(e) = writer.write_all(&buf).await {
                        debug!("{} write error: {}", name, e);
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("{} Sender lagged: missed {} messages", name, n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    tokio::select! {
        _ = reader_loop => Ok(()),
        _ = writer_loop => Ok(()),
        _ = cancel_token.cancelled() => Ok(()),
    }
}
