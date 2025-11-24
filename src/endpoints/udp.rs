use anyhow::{Result, Context};
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{error, warn, debug};
use mavlink::MavlinkVersion;
use crate::router::RoutedMessage;
use std::io::Cursor;
use std::net::{SocketAddr, ToSocketAddrs};
use std::collections::HashSet;
use crate::routing::RoutingTable;
use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use tokio_util::sync::CancellationToken;
use crate::endpoint_core::EndpointCore;
use crate::framing::MavlinkFrame;
use crate::lock_mutex;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: usize,
    address: String,
    mode: crate::config::EndpointMode,
    bus_tx: broadcast::Sender<RoutedMessage>,
    bus_rx: broadcast::Receiver<RoutedMessage>,
    routing_table: Arc<RwLock<RoutingTable>>,
    dedup: Arc<Mutex<Dedup>>,
    filters: EndpointFilters,
    token: CancellationToken,
) -> Result<()> {
    
    let core = EndpointCore {
        id,
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
    };

    let (bind_addr, target_addr) = if mode == crate::config::EndpointMode::Server {
        (address.clone(), None)
    } else {
        let mut addrs = address.to_socket_addrs().context("Invalid remote address")?;
        let target = addrs.next().context("Could not resolve remote address")?;
        ("0.0.0.0:0".to_string(), Some(target))
    };

    let socket = UdpSocket::bind(&bind_addr).await
        .with_context(|| format!("Failed to bind UDP socket to {}", bind_addr))?;
        
    let r = Arc::new(socket);
    let s = r.clone();
    
    let clients = Arc::new(Mutex::new(HashSet::new()));

    let clients_recv = clients.clone();
    let r_socket = r.clone();
    let core_rx = core.clone();

    // Receiver Task
    let recv_loop = async move {
        let mut buf = [0u8; 65535];
        loop {
            match r_socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    {
                        let mut guard = lock_mutex!(clients_recv);
                        guard.insert(addr);
                    }
                    
                    let mut cursor = Cursor::new(&buf[..len]);
                    let res = mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut cursor).map(|(h, m)| (h, m, MavlinkVersion::V2))
                        .or_else(|_| {
                             cursor.set_position(0);
                             mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut cursor).map(|(h, m)| (h, m, MavlinkVersion::V1))
                        });

                    if let Ok((header, message, version)) = res {
                        // Use Core logic
                        core_rx.handle_incoming_frame(MavlinkFrame { header, message, version });
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
    let mut bus_rx_loop = bus_rx;
    let core_tx = core.clone();
    
    let send_loop = async move {
        loop {
            match bus_rx_loop.recv().await {
                Ok(msg) => {
                    if !core_tx.check_outgoing(&msg) {
                        continue;
                    }

                    let mut buf = Vec::new(); 
                    if let Err(e) = match msg.version {
                         MavlinkVersion::V2 => mavlink::write_v2_msg(&mut buf, msg.header, &msg.message),
                         MavlinkVersion::V1 => mavlink::write_v1_msg(&mut buf, msg.header, &msg.message),
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
                             let guard = lock_mutex!(clients_send);
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
