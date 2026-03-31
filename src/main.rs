mod capture;
mod cloudflare;
mod cloudflared;
mod config;
mod proxy;
mod storage;
mod sync;
mod tunnel;
mod web_server;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::sync::RwLock;
use tracing::info;
use tracing_subscriber::FmtSubscriber;

use cloudflare::CloudflareClient;
use cloudflared::CloudflaredService;
use config::Config;
use storage::{RequestStorage, WebSocketMessageStorage};
use sync::{SyncReport, TunnelSync};
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
    let mut config = if args.config.exists() {
        Config::from_file(&args.config)?
    } else {
        info!("Config file not found, using default configuration");
        Config::default_config()
    };

    // Cloudflare setup (only when [cloudflare] section is present)
    let tunnel_sync: Option<Arc<TunnelSync>> = setup_cloudflare(&mut config, &args.config).await;

    // Wrap config in shared Arc<RwLock> for live mutation by CRUD handlers.
    let shared_config = Arc::new(RwLock::new(config));

    // Create storage instances
    let cfg = shared_config.read().await;
    let request_storage = Arc::new(RequestStorage::new(cfg.capture.max_stored_requests));
    let websocket_storage = Arc::new(WebSocketMessageStorage::new(
        cfg.capture.max_stored_requests,
    ));
    drop(cfg);

    // Create tunnel manager
    let tunnel_manager = {
        let cfg = shared_config.read().await;
        Arc::new(TunnelManager::new(
            &cfg,
            request_storage.clone(),
            websocket_storage.clone(),
        ))
    };

    // Create and start web server
    let web_server = WebServer::new(
        shared_config.clone(),
        tunnel_manager.clone(),
        tunnel_sync,
        request_storage.clone(),
        websocket_storage.clone(),
    );
    let web_server_handle = tokio::spawn(async move {
        if let Err(e) = web_server.start().await {
            tracing::error!("Web server error: {}", e);
        }
    });

    // Start all tunnels
    {
        let cfg = shared_config.read().await;
        tunnel_manager.start_tunnels(&cfg).await;
    }

    // Wait for shutdown signal
    tunnel_manager.wait_for_shutdown_signal().await;

    // Shutdown all tunnels
    tunnel_manager.shutdown().await;

    // Abort web server
    web_server_handle.abort();

    Ok(())
}

/// Performs Cloudflare setup if `[cloudflare]` is configured.
///
/// Creates the tunnel on first run, installs cloudflared if needed, and
/// performs an initial sync. Returns `None` when Cloudflare is not configured.
async fn setup_cloudflare(
    config: &mut Config,
    config_path: &std::path::Path,
) -> Option<Arc<TunnelSync>> {
    let cf_cfg = config.cloudflare.as_ref()?;

    let client = match CloudflareClient::new(&cf_cfg.api_token, &cf_cfg.account_id, &cf_cfg.zone_id)
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create Cloudflare client: {e}");
            return None;
        }
    };

    // Create tunnel if not already configured.
    if cf_cfg.tunnel_id.is_none() {
        info!("No tunnel_id configured; creating a new Cloudflare tunnel...");
        let tunnel_name = cf_cfg.tunnel_name.clone();

        let secret = generate_tunnel_secret();
        let tunnel_id = match client.create_tunnel(&tunnel_name, &secret).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to create Cloudflare tunnel: {e}");
                return None;
            }
        };
        info!("Created Cloudflare tunnel: {tunnel_id}");

        let token = match client.get_tunnel_token(&tunnel_id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("Failed to get tunnel token: {e}");
                return None;
            }
        };

        if let Err(e) = TunnelSync::save_tunnel_credentials(config, config_path, tunnel_id, token) {
            tracing::error!("Failed to save tunnel credentials to config: {e}");
            return None;
        }
        info!("Saved tunnel credentials to {}", config_path.display());
    }

    let cf_cfg = config.cloudflare.as_ref().unwrap();
    let tunnel_id = cf_cfg.tunnel_id.as_ref().unwrap().clone();
    let tunnel_token = cf_cfg.tunnel_token.as_deref().unwrap_or("").to_string();

    // Install and start cloudflared service if needed.
    if !CloudflaredService::is_installed().await {
        tracing::warn!(
            "cloudflared binary not found on PATH. \
             Install it and run `cloudflared service install {tunnel_token}` manually."
        );
    } else if !CloudflaredService::is_running().await {
        info!("cloudflared service not running; installing...");
        if let Err(e) = CloudflaredService::install_and_start(&tunnel_token).await {
            tracing::warn!("Failed to install cloudflared service: {e}");
        }
    }

    let sync = Arc::new(TunnelSync::new(client, &tunnel_id));

    // Initial sync — report unknown hosts as warnings but don't auto-remove.
    let report: SyncReport = sync.sync_to_cloudflare(config).await;
    if !report.added.is_empty() {
        info!(
            "Sync: added {} host(s): {:?}",
            report.added.len(),
            report.added
        );
    }
    if !report.unknown_hosts.is_empty() {
        tracing::warn!(
            "Cloudflare has {} unknown host(s) not in config.toml: {:?}. \
             Use the web UI to confirm removal.",
            report.unknown_hosts.len(),
            report.unknown_hosts
        );
    }
    for err in &report.errors {
        tracing::warn!("Sync error: {err}");
    }

    Some(sync)
}

/// Generates a base64-encoded random 32-byte tunnel secret.
fn generate_tunnel_secret() -> String {
    use base64::Engine as _;
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    base64::engine::general_purpose::STANDARD.encode(secret)
}
