mod capture;
mod config;
mod proxy;
mod storage;
mod tunnel;
mod web_server;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::FmtSubscriber;

use config::Config;
use storage::{RequestStorage, WebSocketMessageStorage};
use tunnel::TunnelManager;
use web_server::WebServer;

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

    // Create storage instances
    let request_storage = Arc::new(RequestStorage::new(config.capture.max_stored_requests));
    let websocket_storage = Arc::new(WebSocketMessageStorage::new(
        config.capture.max_stored_requests,
    ));

    // Create and start web server
    let web_server = WebServer::new(
        config.clone(),
        request_storage.clone(),
        websocket_storage.clone(),
    );
    let web_server_handle = tokio::spawn(async move {
        if let Err(e) = web_server.start().await {
            tracing::error!("Web server error: {}", e);
        }
    });

    // Create and start tunnel manager
    let tunnel_manager =
        TunnelManager::new(config, request_storage.clone(), websocket_storage.clone());
    tunnel_manager.start_tunnels();

    // Wait for shutdown signal
    tunnel_manager.wait_for_shutdown_signal().await;

    // Shutdown all tunnels
    tunnel_manager.shutdown().await;

    // Abort web server
    web_server_handle.abort();

    Ok(())
}
