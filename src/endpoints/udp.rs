use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use crate::router::RoutedMessage;
use crate::routing::RoutingTable;
use anyhow::{Context, Result};
use mavlink::{MavlinkVersion, Message};
use parking_lot::{Mutex, RwLock};
use std::collections::HashSet;
use std::io::Cursor;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: usize,
    address: String,
    mode: crate::config::EndpointMode,
    bus_tx: broadcast::Sender<RoutedMessage>,
    mut bus_rx: broadcast::Receiver<RoutedMessage>,
    routing_table: Arc<RwLock<RoutingTable>>,
    dedup: Arc<Mutex<Dedup>>,
    filters: EndpointFilters,
    token: CancellationToken,
) -> Result<()> {
    let (bind_addr, target_addr) = if mode == crate::config::EndpointMode::Server {
        (address.clone(), None)
    } else {
        let mut addrs = address
            .to_socket_addrs()
            .context("Invalid remote address")?;
        let target = addrs.next().context("Could not resolve remote address")?;
        ("0.0.0.0:0".to_string(), Some(target))
    };

    let socket = UdpSocket::bind(&bind_addr)
        .await
        .with_context(|| format!("Failed to bind UDP socket to {}", bind_addr))?;

    let r = Arc::new(socket);
    let s = r.clone();

    let clients = Arc::new(Mutex::new(HashSet::new()));

    let clients_recv = clients.clone();
    let tx_inner = bus_tx.clone();
    let r_socket = r.clone();
    let rt_recv = routing_table.clone();
    let filters_rx = filters.clone();

    // Receiver Task
    let recv_loop = async move {
        let mut buf = [0u8; 65535];
        loop {
            match r_socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    {
                        #[allow(clippy::expect_used)]
                        let mut guard = clients_recv.lock();
                        guard.insert(addr);
                    }

                    {
                        #[allow(clippy::expect_used)]
                        let mut dd = dedup.lock();
                        if dd.is_duplicate(&buf[..len]) {
                            continue;
                        }
                    }

                    let mut cursor = Cursor::new(&buf[..len]);
                    let res = mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut cursor)
                        .map(|(h, m)| (h, m, MavlinkVersion::V2))
                        .or_else(|_| {
                            cursor.set_position(0);
                            mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut cursor)
                                .map(|(h, m)| (h, m, MavlinkVersion::V1))
                        });

                    if let Ok((header, message, version)) = res {
                        if !filters_rx.check_incoming(&header, message.message_id()) {
                            continue;
                        }

                        {
                            #[allow(clippy::expect_used)]
                            let mut rt = rt_recv.write();
                            rt.update(id, header.system_id, header.component_id);
                        }

                        if let Err(e) = tx_inner.send(RoutedMessage {
                            source_id: id,
                            header,
                            message,
                            version,
                        }) {
                            debug!("UDP Bus Send Error: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("UDP recv error: {}", e);
                }
            }
        }
    };

    // Sender Task
    let clients_send = clients.clone();
    let s_socket = s.clone();
    let _rt_send = routing_table.clone();
    let filters_tx = filters.clone();

    let send_loop = async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if msg.source_id == id {
                        continue;
                    }

                    if !filters_tx.check_outgoing(&msg.header, msg.message.message_id()) {
                        continue;
                    }

                    let mut buf = Vec::new();
                    if let Err(e) = match msg.version {
                        MavlinkVersion::V2 => {
                            mavlink::write_v2_msg(&mut buf, msg.header, &msg.message)
                        }
                        MavlinkVersion::V1 => {
                            mavlink::write_v1_msg(&mut buf, msg.header, &msg.message)
                        }
                    } {
                        warn!("UDP Serialize Error: {}", e);
                        continue;
                    }

                    if let Some(target) = target_addr {
                        if let Err(e) = s_socket.send_to(&buf, target).await {
                            debug!("UDP send error to target: {}", e);
                        }
                    } else {
                        let targets: Vec<SocketAddr> = {
                            #[allow(clippy::expect_used)]
                            let guard = clients_send.lock();
                            guard.iter().cloned().collect()
                        };
                        for client in targets {
                            if let Err(e) = s_socket.send_to(&buf, client).await {
                                debug!("UDP broadcast error to {}: {}", client, e);
                            }
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("UDP Sender lagged: missed {} messages", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    tokio::select! {
        _ = recv_loop => Ok(()),
        _ = send_loop => Ok(()),
        _ = token.cancelled() => Ok(()),
    }
}
