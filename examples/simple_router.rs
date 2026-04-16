//! Simple MAVRouter example
//!
//! Starts a router from an inline TOML config with a single UDP server
//! endpoint on port 14550, then blocks until Ctrl+C. This is the
//! recommended way to embed `mavrouter` in a binary — nothing more than
//! [`Router::from_str`] and [`Router::stop`] is required.
//!
//! Run with: `cargo run --example simple_router`

use mavrouter::Router;

#[tokio::main]
async fn main() -> Result<(), mavrouter::error::RouterError> {
    tracing_subscriber::fmt::init();

    let router = Router::from_str(
        r#"
[general]
bus_capacity = 1000

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
"#,
    )
    .await?;

    tracing::info!("Router started — UDP GCS listening on 0.0.0.0:14550");

    // Wait for Ctrl+C, then gracefully shut down.
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!("Failed to install Ctrl+C handler: {}", e);
    }
    tracing::info!("Shutting down...");
    router.stop().await;

    Ok(())
}
