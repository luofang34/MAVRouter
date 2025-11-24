//! Core logic for MAVLink endpoints.
//!
//! This module defines the shared operational logic for various MAVLink endpoint types
//! (e.g., TCP, UDP, Serial). It includes the `EndpointCore` struct, which encapsulates
//! common resources and filtering mechanisms, and the `run_stream_loop` function,
//! responsible for processing MAVLink messages over asynchronous read/write streams.

use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use crate::framing::{MavlinkFrame, StreamParser};
use crate::mavlink_utils::extract_target;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use anyhow::Result;
use mavlink::{MavlinkVersion, Message};
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

/// Represents the shared core logic and resources for a MAVLink endpoint.
///
/// Each concrete endpoint type (TCP, UDP, Serial) will create an instance
/// of `EndpointCore` to handle common tasks like message filtering,
/// deduplication, and routing table updates.
#[derive(Clone)]
pub struct EndpointCore {
    /// Unique identifier for this endpoint.
    pub id: EndpointId,
    /// Sender half of the global message bus for publishing incoming messages.
    pub bus_tx: broadcast::Sender<RoutedMessage>,
    /// Shared routing table for learning and querying MAVLink network topology.
    pub routing_table: Arc<RwLock<RoutingTable>>,
    /// Shared deduplication instance to prevent processing duplicate messages.
    pub dedup: Arc<Mutex<Dedup>>,
    /// Filters applied to messages for this specific endpoint.
    pub filters: EndpointFilters,
}

impl EndpointCore {
    /// Handles an incoming MAVLink frame, processing it through filters,
    /// deduplication, and routing table updates, then forwarding it to the message bus.
    ///
    /// # Arguments
    ///
    /// * `frame` - The parsed `MavlinkFrame` containing header, message, and version.
    pub fn handle_incoming_frame(&self, frame: MavlinkFrame) {
        let mut temp_buf = Vec::new();
        if let Err(e) = match frame.version {
            MavlinkVersion::V2 => {
                mavlink::write_v2_msg(&mut temp_buf, frame.header, &frame.message)
            }
            MavlinkVersion::V1 => {
                mavlink::write_v1_msg(&mut temp_buf, frame.header, &frame.message)
            }
        } {
            warn!("Inbound Serialize Error: {}", e);
            return;
        }

        let is_dup = {
            let mut dd = self.dedup.lock();
            dd.is_duplicate(&temp_buf)
        };

        if !is_dup
            && self
                .filters
                .check_incoming(&frame.header, frame.message.message_id())
        {
            {
                let mut rt = self.routing_table.write();
                rt.update(self.id, frame.header.system_id, frame.header.component_id);
            }
            if let Err(e) = self.bus_tx.send(RoutedMessage {
                source_id: self.id,
                header: frame.header,
                message: Arc::new(frame.message),
                version: frame.version,
            }) {
                debug!("Bus send error: {}", e);
            }
        }
    }

    /// Checks if an outgoing `RoutedMessage` should be sent to this endpoint.
    ///
    /// This method applies two main criteria:
    /// 1. **Source ID Check**: Prevents sending a message back to its original sender endpoint.
    /// 2. **Outgoing Filters**: Applies any endpoint-specific filters defined.
    /// 3. **Routing Table**: Queries the `RoutingTable` to determine if this endpoint is a
    ///    valid destination for the message's target system/component.
    ///
    /// # Arguments
    ///
    /// * `msg` - The `RoutedMessage` to check.
    ///
    /// # Returns
    ///
    /// `true` if the message should be sent to this endpoint, `false` otherwise.
    pub fn check_outgoing(&self, msg: &RoutedMessage) -> bool {
        // Don't send a message back to its source endpoint
        if msg.source_id == self.id {
            return false;
        }

        if !self
            .filters
            .check_outgoing(&msg.header, msg.message.message_id())
        {
            return false;
        }

        let target = extract_target(&msg.message);
        let rt = self.routing_table.read();
        let should_send = rt.should_send(self.id, target.system_id, target.component_id);

        if !should_send && target.system_id != 0 {
            // Don't log dropped broadcast as "no route"
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

/// Runs the main stream processing loop for an endpoint.
///
/// This function sets up two concurrent tasks: one for reading incoming bytes
/// from an `AsyncRead` stream and parsing MAVLink messages, and another for
/// writing outgoing MAVLink messages to an `AsyncWrite` stream.
///
/// # Type Parameters
///
/// * `R` - Type that implements `tokio::io::AsyncRead`, `Unpin`, `Send`, and `'static`.
/// * `W` - Type that implements `tokio::io::AsyncWrite`, `Unpin`, `Send`, and `'static`.
///
/// # Arguments
///
/// * `reader` - An asynchronous reader for incoming bytes.
/// * `writer` - An asynchronous writer for outgoing bytes.
/// * `bus_rx` - Receiver half of the message bus for messages to send out.
/// * `core` - The `EndpointCore` instance providing shared logic and resources.
/// * `cancel_token` - `CancellationToken` to signal graceful shutdown.
/// * `name` - A descriptive name for the endpoint (used in logging).
///
/// # Returns
///
/// A `Result` indicating success or failure. The function runs indefinitely
/// until a read/write error occurs or the `cancel_token` is cancelled.
///
/// # Errors
///
/// Returns an `anyhow::Error` if a critical I/O error occurs on either the read
/// or write stream.
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

    // Clone CancellationToken for each independent async block/select branch
    let cancel_token_for_reader_loop = cancel_token.clone();
    let cancel_token_for_writer_loop = cancel_token.clone();
    let cancel_token_for_final_select = cancel_token.clone();

    let reader_loop = async move {
        let mut parser = StreamParser::new();
        let mut buf = [0u8; 4096];

        loop {
            tokio::select! {
                _ = cancel_token_for_reader_loop.cancelled() => break,
                read_res = reader.read(&mut buf) => {
                    match read_res {
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
            }
        }
    };

    let writer_loop = async move {
        loop {
            tokio::select! {
                _ = cancel_token_for_writer_loop.cancelled() => break,
                msg_res = bus_rx.recv() => {
                    match msg_res {
                        Ok(msg) => {
                            if !core.check_outgoing(&msg) {
                                continue;
                            }

                    let mut buf = Vec::new();
                    if let Err(e) = match msg.version {
                        MavlinkVersion::V2 => mavlink::write_v2_msg(&mut buf, msg.header, &*msg.message),
                        MavlinkVersion::V1 => mavlink::write_v1_msg(&mut buf, msg.header, &*msg.message),
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
            }
        }
    };

    tokio::select! {
        _ = reader_loop => Ok(()),
        _ = writer_loop => Ok(()),
        _ = cancel_token_for_final_select.cancelled() => Ok(()),
    }
}
