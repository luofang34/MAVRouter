#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]

mod config;
mod router;
mod endpoint_core;
mod endpoints {
    pub mod udp;
    pub mod tcp;
    pub mod serial;
    pub mod tlog;
}
mod dedup;
mod routing;
mod filter;
mod framing;
mod mavlink_utils;
pub mod lock_helpers;

use anyhow::Result;
use clap::Parser;
use tracing::{info, error, warn};
use crate::config::{Config, EndpointConfig};
use crate::router::create_bus;
use crate::routing::RoutingTable;
use crate::dedup::Dedup;
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "mavrouter.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    
    info!("Starting mavrouter-rs with config: {}", args.config);
    
    let config = match Config::load(&args.config).await {
        Ok(c) => c,
        Err(e) => {
            error!("Error loading config: {:#}", e);
            return Err(e);
        }
    };
    
    info!("Loaded configuration with {} endpoints", config.endpoint.len());

    let bus = create_bus(config.general.bus_capacity);
    let mut handles = vec![];
    
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    
    let dedup_period = config.general.dedup_period_ms.unwrap_or(0);
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(dedup_period))));

    let cancel_token = CancellationToken::new();

    // Pruning Task
    let rt_prune = routing_table.clone();
    let prune_token = cancel_token.child_token();
    let prune_interval = config.general.routing_table_prune_interval_secs;
    let prune_ttl = config.general.routing_table_ttl_secs;
    handles.push(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = prune_token.cancelled() => {
                    info!("RoutingTable Pruner shutting down.");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(prune_interval)) => {
                    let mut rt = lock_write!(rt_prune);
                    rt.prune(Duration::from_secs(prune_ttl));
                }
            }
        }
    }));


    // Implicit TCP Server
    if let Some(port) = config.general.tcp_port {
        let name = format!("Implicit TCP Server :{}", port);
        let bus_tx = bus.clone();
        let bus_rx = bus.subscribe();
        let rt = routing_table.clone();
        let dd = dedup.clone();
        let id = config.endpoint.len();
        let filters = crate::filter::EndpointFilters::default(); 
        let addr = format!("0.0.0.0:{}", port);
        let task_token = cancel_token.child_token();
        
        handles.push(tokio::spawn(supervise(name, task_token.clone(), move || {
            let bus_tx = bus_tx.clone();
            let bus_rx = bus_rx.resubscribe();
            let rt = rt.clone();
            let dd = dd.clone();
            let filters = filters.clone();
            let addr = addr.clone();
            let m = crate::config::EndpointMode::Server; 
            let token = task_token.clone();
            async move {
                crate::endpoints::tcp::run(id, addr, m, bus_tx, bus_rx, rt, dd, filters, token).await
            }
        })));
    }
    
    // Logging
    if let Some(log_dir) = &config.general.log {
        if config.general.log_telemetry {
            let name = format!("TLog Logger {}", log_dir);
            let bus_rx = bus.subscribe();
            let dir = log_dir.clone();
            let task_token = cancel_token.child_token();
            
            handles.push(tokio::spawn(supervise(name, task_token.clone(), move || {
                let bus_rx = bus_rx.resubscribe();
                let dir = dir.clone();
                let token = task_token.clone();
                async move {
                    crate::endpoints::tlog::run(dir, bus_rx, token).await
                }
            })));
        }
    }

    for (i, endpoint_config) in config.endpoint.iter().enumerate() {
        let bus_tx = bus.clone();
        let bus_rx = bus.subscribe();
        let rt = routing_table.clone();
        let dd = dedup.clone();
        let task_token = cancel_token.child_token();
        
        match endpoint_config {
            EndpointConfig::Udp { address, mode, filters } => {
                let name = format!("UDP Endpoint {} ({})", i, address);
                let addr = address.clone();
                let m = mode.clone();
                let f = filters.clone();
                                
                handles.push(tokio::spawn(supervise(name, task_token.clone(), move || {
                    let bus_tx = bus_tx.clone();
                    let bus_rx = bus_rx.resubscribe();
                    let rt = rt.clone();
                    let dd = dd.clone();
                    let f = f.clone();
                    let addr = addr.clone();
                    let m = m.clone();
                    let token = task_token.clone();
                    async move {
                        crate::endpoints::udp::run(i, addr, m, bus_tx, bus_rx, rt, dd, f, token).await
                    }
                })));
            }
            EndpointConfig::Tcp { address, mode, filters } => {
                let name = format!("TCP Endpoint {} ({})", i, address);
                let addr = address.clone();
                let m = mode.clone();
                let f = filters.clone();
                
                handles.push(tokio::spawn(supervise(name, task_token.clone(), move || {
                    let bus_tx = bus_tx.clone();
                    let bus_rx = bus_rx.resubscribe();
                    let rt = rt.clone();
                    let dd = dd.clone();
                    let f = f.clone();
                    let addr = addr.clone();
                    let m = m.clone();
                    let token = task_token.clone();
                    async move {
                        crate::endpoints::tcp::run(i, addr, m, bus_tx, bus_rx, rt, dd, f, token).await
                    }
                })));
            }
            EndpointConfig::Serial { device, baud, filters } => {
                let name = format!("Serial Endpoint {} ({})", i, device);
                let dev = device.clone();
                let b = *baud;
                let f = filters.clone();
                
                handles.push(tokio::spawn(supervise(name, task_token.clone(), move || {
                    let bus_tx = bus_tx.clone();
                    let bus_rx = bus_rx.resubscribe();
                    let rt = rt.clone();
                    let dd = dd.clone();
                    let f = f.clone();
                    let dev = dev.clone();
                    let token = task_token.clone();
                    async move {
                        crate::endpoints::serial::run(i, dev, b, bus_tx, bus_rx, rt, dd, f, token).await
                    }
                })));
            }
        }
    }

    if handles.is_empty() {
        info!("No endpoints configured. Exiting.");
        return Ok(());
    }

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl+C received. Initiating graceful shutdown...");
        }
        _ = futures::future::join_all(handles) => {
            info!("All supervised tasks completed/failed. Shutting down.");
        }
    }
    cancel_token.cancel();

    // Give tasks time to flush (e.g. tlog)
    tokio::time::sleep(Duration::from_secs(2)).await;
    info!("Shutdown complete.");

    Ok(())
}

async fn supervise<F, Fut>(name: String, cancel_token: CancellationToken, task_factory: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} received cancellation signal. Exiting.", name);
                break;
            }
            result = task_factory() => {
                match result {
                    Ok(_) => {
                        warn!("Supervisor: Task {} finished cleanly (unexpected). Restarting in 1s...", name);
                    }
                    Err(e) => {
                        error!("Supervisor: Task {} failed: {:#}. Restarting in 1s...", name, e);
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
