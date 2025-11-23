use anyhow::{Result, Context};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tracing::{error, info, warn, debug};
use mavlink::{MavlinkVersion, Message};
use crate::router::RoutedMessage;
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::net::SocketAddr;
use crate::routing::RoutingTable;
use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use crate::framing::StreamParser;
use tokio_util::sync::CancellationToken;

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
    
    let clients: Arc<Mutex<HashMap<SocketAddr, tokio::sync::mpsc::Sender<Vec<u8>>>>> = Arc::new(Mutex::new(HashMap::new()));
    
    let clients_sender_loop = clients.clone();
    let filters_tx = filters.clone();

    let sender_task = async move {
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
                    // Log serialization error
                    if let Err(e) = match msg.version {
                         MavlinkVersion::V2 => mavlink::write_v2_msg(&mut buf, msg.header, &msg.message),
                         MavlinkVersion::V1 => mavlink::write_v1_msg(&mut buf, msg.header, &msg.message),
                    } {
                        warn!("TCP Serialize error: {}", e);
                        continue;
                    }
                    
                    let mut targets_to_remove = Vec::new();
                    {
                        #[allow(clippy::expect_used)]
                        let mut guard = clients_sender_loop.lock();
                        for (addr, tx) in guard.iter() {
                            if let Err(e) = tx.try_send(buf.clone()) {
                                if tx.is_closed() {
                                    targets_to_remove.push(*addr);
                                } else {
                                    debug!("TCP send to {} dropped (Full): {}", addr, e);
                                }
                            }
                        }
                        for addr in targets_to_remove {
                            guard.remove(&addr);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                     warn!("TCP Sender lagged: missed {} messages", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    // Listener/Connector Task
    let listener_task = async move {
        match mode {
            crate::config::EndpointMode::Server => {
                let listener = TcpListener::bind(&address).await
                    .with_context(|| format!("Failed to bind TCP listener to {}", address))?;
                info!("TCP Server listening on {}", address);
                
                let mut join_set = JoinSet::new();

                loop {
                    tokio::select! {
                        accept_res = listener.accept() => {
                            match accept_res {
                                Ok((stream, addr)) => {
                                    info!("Accepted TCP connection from {}", addr);
                                    join_set.spawn(handle_connection(id, stream, addr, bus_tx.clone(), clients.clone(), routing_table.clone(), dedup.clone(), filters.clone()));
                                }
                                Err(e) => error!("TCP Accept error: {}", e),
                            }
                        }
                        _ = join_set.join_next(), if !join_set.is_empty() => {}
                    }
                }
            }
            crate::config::EndpointMode::Client => {
                info!("Connecting to TCP server at {}", address);
                loop {
                    match TcpStream::connect(&address).await {
                        Ok(stream) => {
                            info!("Connected to {}", address);
                            let addr = stream.peer_addr().unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
                            
                            handle_connection(id, stream, addr, bus_tx.clone(), clients.clone(), routing_table.clone(), dedup.clone(), filters.clone()).await;
                            warn!("Connection to {} lost, retrying in 1s...", address);
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                        Err(e) => {
                            error!("Failed to connect to {}: {}. Retrying in 5s...", address, e);
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            }
        }
    };

    tokio::select! {
        _ = sender_task => Ok(()),
        res = listener_task => res,
        _ = token.cancelled() => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    id: usize,
    stream: TcpStream,
    addr: SocketAddr,
    bus_tx: broadcast::Sender<RoutedMessage>,
    clients: Arc<Mutex<HashMap<SocketAddr, tokio::sync::mpsc::Sender<Vec<u8>>>>>,
    routing_table: Arc<RwLock<RoutingTable>>,
    dedup: Arc<Mutex<Dedup>>,
    filters: EndpointFilters,
) {
    let (mut read_stream, mut write_stream) = stream.into_split();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(100);
    
    {
        #[allow(clippy::expect_used)]
        let mut guard = clients.lock();
        guard.insert(addr, tx);
    }
    
    let clients_writer = clients.clone();
    let writer_loop = async move {
        while let Some(packet) = rx.recv().await {
            if let Err(e) = write_stream.write_all(&packet).await {
                debug!("TCP write error to {}: {}", addr, e);
                break;
            }
        }
        #[allow(clippy::expect_used)]
        let mut guard = clients_writer.lock();
        guard.remove(&addr);
    };
    
    let reader_loop = async move {
        let mut parser = StreamParser::new();
        let mut buf = [0u8; 4096];
        
        loop {
            match read_stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    parser.push(&buf[..n]);
                    
                    while let Some(frame) = parser.next() {
                         let mut temp_buf = Vec::new();
                         if let Err(e) = match frame.version {
                            MavlinkVersion::V2 => mavlink::write_v2_msg(&mut temp_buf, frame.header, &frame.message),
                            MavlinkVersion::V1 => mavlink::write_v1_msg(&mut temp_buf, frame.header, &frame.message),
                        } {
                            warn!("TCP Inbound Serialize Error: {}", e);
                            continue;
                        }
                        
                        let is_dup = {
                            #[allow(clippy::expect_used)]
                            let mut dd = dedup.lock();
                            dd.is_duplicate(&temp_buf)
                        };
                        
                        if !is_dup && filters.check_incoming(&frame.header, frame.message.message_id()) {
                            {
                                 #[allow(clippy::expect_used)]
                                 let mut rt = routing_table.write();
                                 rt.update(id, frame.header.system_id, frame.header.component_id);
                            }
                            if let Err(e) = bus_tx.send(RoutedMessage {
                                source_id: id,
                                header: frame.header,
                                message: frame.message,
                                version: frame.version,
                            }) {
                                debug!("TCP Bus send error (no receivers?): {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("TCP read error from {}: {}", addr, e);
                    break;
                }
            }
        }
    };
    
    tokio::select! {
        _ = writer_loop => {},
        _ = reader_loop => {},
    }
    
    info!("Connection from {} closed", addr);
}
