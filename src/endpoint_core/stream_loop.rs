//! `run_stream_loop`: the shared reader/writer select-loop implementation
//! used by every stream-based endpoint (TCP and Serial).

use super::EndpointCore;
use crate::error::{Result, RouterError};
use crate::framing::StreamParser;
use crate::router::RoutedMessage;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
use tokio_util::sync::CancellationToken;
use tracing::error;

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
/// Returns a [`crate::error::RouterError`] if a critical I/O error occurs on either the
/// read or write stream.
pub async fn run_stream_loop<R, W>(
    mut reader: R,
    writer: W,
    mut bus_rx: broadcast::Receiver<Arc<RoutedMessage>>,
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
            if cancel_token_for_reader_loop.is_cancelled() {
                return Ok(());
            }
            match reader.read(&mut buf).await {
                Ok(0) => return Ok(()), // EOF
                Ok(n) => {
                    // n comes from reader.read(), always <= buf.len()
                    #[allow(clippy::indexing_slicing)]
                    parser.push(&buf[..n]);
                    while let Some(frame) = parser.parse_next() {
                        core_read.handle_incoming_frame(frame);
                    }
                }
                Err(e) => {
                    return Err(RouterError::network(&name_read, e));
                }
            }
        }
    };

    let writer_loop = async move {
        let mut writer = BufWriter::with_capacity(65536, writer);

        loop {
            let msg = match bus_rx.recv().await {
                Ok(msg) => msg,
                Err(RecvError::Lagged(n)) => {
                    // Dropping bus messages is a correctness signal, not
                    // noise — surface it at error! and count it so the
                    // stats pipeline picks it up.
                    core.stats.bus_lagged.fetch_add(n, Ordering::Relaxed);
                    error!(
                        "{} bus receiver lagged: dropped {} messages (bus_lagged now {})",
                        name,
                        n,
                        core.stats.bus_lagged.load(Ordering::Relaxed)
                    );
                    continue;
                }
                Err(RecvError::Closed) => break,
            };
            if cancel_token_for_writer_loop.is_cancelled() {
                break;
            }

            if !core.check_outgoing(&msg) {
                continue;
            }

            #[allow(clippy::cast_possible_truncation)] // MAVLink frame fits u64
            let len = msg.serialized_bytes.len() as u64;
            if let Err(e) = writer.write_all(&msg.serialized_bytes).await {
                core.stats.errors.fetch_add(1, Ordering::Relaxed);
                return Err(RouterError::network(&name, e));
            }
            core.stats.record_outgoing(len);

            // Optimistic batching - process a fixed batch to reduce wakeups
            const BATCH_SIZE: usize = 1024;
            for _ in 0..BATCH_SIZE {
                match bus_rx.try_recv() {
                    Ok(m) => {
                        if core.check_outgoing(&m) {
                            #[allow(clippy::cast_possible_truncation)]
                            let batch_len = m.serialized_bytes.len() as u64;
                            if let Err(e) = writer.write_all(&m.serialized_bytes).await {
                                core.stats.errors.fetch_add(1, Ordering::Relaxed);
                                return Err(RouterError::network(&name, e));
                            }
                            core.stats.record_outgoing(batch_len);
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Lagged(n)) => {
                        core.stats.bus_lagged.fetch_add(n, Ordering::Relaxed);
                        error!(
                            "{} bus receiver lagged (batch): dropped {} messages (bus_lagged now {})",
                            name,
                            n,
                            core.stats.bus_lagged.load(Ordering::Relaxed)
                        );
                    }
                    Err(TryRecvError::Closed) => return Ok(()),
                }
            }

            // Flush buffered writes; keep simple to avoid deadlocks in low traffic tests
            if let Err(e) = writer.flush().await {
                core.stats.errors.fetch_add(1, Ordering::Relaxed);
                return Err(RouterError::network(&name, e));
            }
        }
        Ok(())
    };

    // Propagate errors from reader/writer loops (issue #3)
    tokio::select! {
        result = reader_loop => result,
        result = writer_loop => result,
        _ = cancel_token_for_final_select.cancelled() => Ok(()),
    }
}
