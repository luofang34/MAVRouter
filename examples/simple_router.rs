//! Simple MAVRouter example
//!
//! This example creates a router with:
//! - UDP server on port 14550 (for GCS)
//! - TCP server on port 5760 (for companion computer)
//!
//! Run with: cargo run --example simple_router

use mavrouter::*;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create message bus
    let bus = router::create_bus(1000);

    // Create routing table and deduplication
    let routing_table = std::sync::Arc::new(parking_lot::RwLock::new(routing::RoutingTable::new()));
    let dedup = dedup::ConcurrentDedup::new(Duration::from_millis(100));

    let token = tokio_util::sync::CancellationToken::new();

    // Start UDP endpoint (GCS)
    let bus_tx = bus.sender();
    let bus_rx = bus.subscribe();
    let rt = routing_table.clone();
    let dd = dedup.clone();
    let t = token.clone();
    // This example does not spawn a routing updater task, so drop the
    // receiver — ingress route submissions will silently fail via try_send,
    // which is fine for a demo (the routing table just stays empty).
    let (route_tx, _route_rx) = tokio::sync::mpsc::channel::<routing::RouteUpdate>(16);

    tokio::spawn(async move {
        endpoints::udp::run(
            1,
            "0.0.0.0:14550".to_string(),
            config::EndpointMode::Server,
            bus_tx,
            bus_rx,
            rt,
            route_tx,
            dd,
            filter::EndpointFilters::default(),
            t,
            300,
            5,
            std::sync::Arc::new(mavrouter::endpoint_core::EndpointStats::new()),
        )
        .await
    });

    tracing::info!("Router started");
    tracing::info!("UDP GCS: 0.0.0.0:14550");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down...");
    token.cancel();

    Ok(())
}
