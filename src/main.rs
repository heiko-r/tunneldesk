mod capture;
mod config;
mod proxy;
// mod query;
mod storage;
mod tunnel;

use clap::Parser;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::FmtSubscriber;

use config::Config;
use tunnel::TunnelManager;

#[derive(Parser)]
#[command(name = "tunneldesk")]
#[command(about = "A local HTTP proxy with Unix domain sockets")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Load configuration
    let config = if args.config.exists() {
        Config::from_file(&args.config)?
    } else {
        info!("Config file not found, using default configuration");
        Config::default_config()
    };

    // Create and start tunnel manager
    let tunnel_manager = TunnelManager::new(config);
    tunnel_manager.start_tunnels();

    // Wait for shutdown signal
    tunnel_manager.wait_for_shutdown_signal().await;

    // Shutdown all tunnels
    tunnel_manager.shutdown().await;

    Ok(())
}
