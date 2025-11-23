//! Basic MAVLink Router Example
//!
//! This example demonstrates how to run a simple MAVLink router with:
//! - TCP server on port 5760
//! - UDP client to ground control station on port 14550
//!
//! Usage:
//!     cargo run --example basic_router

use anyhow::Result;
use std::path::PathBuf;
use tokio::fs;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    println!("=== MAVLink Router Basic Example ===\n");
    println!("This example creates a minimal MAVLink router configuration.");
    println!("The router will:");
    println!("  - Listen on TCP port 5760 for incoming connections");
    println!("  - Forward MAVLink messages to UDP 127.0.0.1:14550");
    println!("\nConfiguration file will be created at: ./config/example_basic.toml\n");

    // Create example configuration
    let config_content = r#"[general]
# Message bus capacity (number of messages that can be queued)
bus_capacity = 1000

# Deduplication period in milliseconds (0 = disabled)
dedup_period_ms = 100

# Routing table pruning interval in seconds
routing_table_prune_interval_secs = 30

# Routing table entry time-to-live in seconds
routing_table_ttl_secs = 60

# Implicit TCP server port (optional - creates a TCP server automatically)
tcp_port = 5760

# Telemetry logging (optional)
log = "logs"
log_telemetry = true

# Example TCP endpoint (client mode)
# [[endpoint]]
# type = "tcp"
# address = "127.0.0.1:5760"
# mode = "client"

# Example UDP endpoint (client mode to GCS)
[[endpoint]]
type = "udp"
address = "127.0.0.1:14550"
mode = "client"

# Example Serial endpoint (uncomment to use)
# [[endpoint]]
# type = "serial"
# device = "/dev/ttyACM0"
# baud = 57600
"#;

    // Ensure config directory exists
    let config_dir = PathBuf::from("config");
    fs::create_dir_all(&config_dir).await?;

    // Write example config
    let config_path = config_dir.join("example_basic.toml");
    fs::write(&config_path, config_content).await?;

    println!("✓ Configuration file created: {}", config_path.display());
    println!("\nTo run the router with this configuration:");
    println!("    cargo run -- --config {}", config_path.display());
    println!("\nOr use the default configuration:");
    println!("    cargo run");

    Ok(())
}
