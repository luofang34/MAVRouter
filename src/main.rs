mod config;
mod endpoint_core;
mod error;
mod orchestration;
mod router;
mod endpoints {
    pub mod serial;
    pub mod tcp;
    pub mod tlog;
    pub mod udp;
}
mod dedup;
mod filter;
mod framing;
mod mavlink_utils;
mod routing;
mod run_loop;

use crate::error::Result;
use crate::run_loop::{run_with_signals, ReloadSignal, ShutdownSignal};
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to configuration file
    #[arg(
        short,
        long,
        default_value = "mavrouter.toml",
        env = "MAVROUTER_CONF_FILE"
    )]
    config: String,
    /// Path to configuration directory (*.toml files merged alphabetically)
    #[arg(long, env = "MAVROUTER_CONF_DIR")]
    config_dir: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("mavrouter-rs v{} - AGPL-3.0", env!("CARGO_PKG_VERSION"));
    info!("Source code: {}", env!("CARGO_PKG_REPOSITORY"));
    info!("This program comes with ABSOLUTELY NO WARRANTY.");

    let args = Args::parse();

    info!("Starting mavrouter-rs with config: {}", args.config);
    if let Some(ref dir) = args.config_dir {
        info!("Config directory: {}", dir);
    }

    let sig_hup = ReloadSignal::new()?;
    let shutdown = ShutdownSignal::new()?;

    run_with_signals(args.config, args.config_dir, sig_hup, shutdown, None).await
}
