//! Unix-domain stats socket: accepts line-oriented queries (`stats`,
//! `endpoint_stats`, `help`) and responds with single-shot JSON.
//!
//! Gated behind `#[cfg(unix)]` at the module declaration in
//! `orchestration.rs`. Windows builds skip this module entirely.

// Used only by the binary (`main.rs`); the library never spawns this.
#![allow(dead_code)]

use super::{supervise, NamedTask};
use crate::endpoint_core::EndpointStats;
use crate::router::EndpointId;
use crate::routing::RoutingTable;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Spawn the stats Unix-socket query server, wrapped in the standard
/// supervisor so crashes get restarted with exponential backoff. Returns
/// `None` when `socket_path` is empty (caller treats that as "not
/// configured"); otherwise a named task the caller pushes into its
/// shutdown-join list.
pub fn spawn_stats_socket(
    socket_path: String,
    routing_table: Arc<RoutingTable>,
    ep_stats: Vec<(EndpointId, String, Arc<EndpointStats>)>,
    cancel_token: CancellationToken,
) -> Option<NamedTask> {
    if socket_path.is_empty() {
        return None;
    }
    let name = format!("Stats Socket {}", socket_path);
    let supervisor_name = name.clone();
    let task_token = cancel_token;
    let handle = tokio::spawn(supervise(supervisor_name, task_token.clone(), move || {
        let rt = routing_table.clone();
        let ep_st = ep_stats.clone();
        let token = task_token.clone();
        let path = socket_path.clone();
        async move { run_stats_server(path, rt, ep_st, token).await }
    }));
    Some(NamedTask::new(name, handle))
}

async fn run_stats_server(
    socket_path: String,
    routing_table: Arc<RoutingTable>,
    ep_stats: Vec<(EndpointId, String, Arc<EndpointStats>)>,
    token: CancellationToken,
) -> crate::error::Result<()> {
    let path = Path::new(&socket_path);
    if path.exists() {
        tokio::fs::remove_file(path)
            .await
            .map_err(|e| crate::error::RouterError::filesystem(&socket_path, e))?;
    }

    let listener = UnixListener::bind(path)
        .map_err(|e| crate::error::RouterError::network(&socket_path, e))?;
    info!("Stats Query Interface listening on {}", socket_path);

    let metadata = std::fs::metadata(path)
        .map_err(|e| crate::error::RouterError::filesystem(&socket_path, e))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o660); // Restrict to owner/group read/write
    std::fs::set_permissions(path, permissions)
        .map_err(|e| crate::error::RouterError::filesystem(&socket_path, e))?;

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                info!("Stats Server shutting down.");
                if path.exists() {
                    if let Err(e) = tokio::fs::remove_file(path).await {
                        warn!(
                            "Failed to remove stats socket '{}' on shutdown: {}",
                            socket_path, e
                        );
                    }
                }
                break;
            }
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((mut stream, _addr)) => {
                        let rt = routing_table.clone();
                        let ep_stats_clone = ep_stats.clone();
                        tokio::spawn(async move {
                            let mut buf = [0u8; 1024];
                            let n = match stream.read(&mut buf).await {
                                Ok(n) if n > 0 => n,
                                _ => return,
                            };

                            // n comes from stream.read(), always <= buf.len()
                            #[allow(clippy::indexing_slicing)]
                            let command = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                            let response = match command.as_str() {
                                "stats" => {
                                    let stats = rt.stats();
                                    format!(
                                        r#"{{"total_systems":{},"total_routes":{},"total_endpoints":{},"timestamp":{}}}"#,
                                        stats.total_systems,
                                        stats.total_routes,
                                        stats.total_endpoints,
                                        stats.timestamp
                                    )
                                }
                                "endpoint_stats" => {
                                    let mut entries = Vec::new();
                                    for (ep_id, ep_name, ep_st) in &ep_stats_clone {
                                        let snap = ep_st.snapshot();
                                        entries.push(format!(
                                            r#"{{"id":{},"name":"{}","msgs_in":{},"msgs_out":{},"bytes_in":{},"bytes_out":{},"errors":{}}}"#,
                                            ep_id.0,
                                            ep_name,
                                            snap.msgs_in,
                                            snap.msgs_out,
                                            snap.bytes_in,
                                            snap.bytes_out,
                                            snap.errors
                                        ));
                                    }
                                    format!("[{}]", entries.join(","))
                                }
                                "help" => "Available commands: stats, endpoint_stats, help".to_string(),
                                _ => "Unknown command. Try 'help'".to_string(),
                            };

                            if let Err(e) = stream.write_all(response.as_bytes()).await {
                                // Clients routinely disconnect mid-response, so this
                                // is expected noise — keep it at debug.
                                tracing::debug!(
                                    "Stats Server write to client failed: {}", e
                                );
                            }
                        });
                    }
                    Err(e) => {
                        warn!("Stats Server accept error: {}", e);
                    }
                }
            }
        }
    }
    Ok(())
}
