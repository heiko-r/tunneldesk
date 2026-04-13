mod capture;
mod cloudflare;
mod cloudflared;
mod config;
#[cfg(feature = "mcp")]
mod mcp;
mod proxy;
mod storage;
mod sync;
mod tunnel;
mod web_server;

#[cfg(feature = "gui")]
mod gui;

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

#[derive(Parser, Clone)]
#[command(name = "tunneldesk")]
#[command(about = "A local HTTP proxy with Unix domain sockets")]
struct Args {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Run without a native GUI window (headless server mode)
    #[arg(long)]
    no_gui: bool,

    /// Run as an MCP server on stdio (requires the `mcp` Cargo feature).
    /// Tracing output is redirected to stderr so it does not interfere with
    /// the MCP protocol stream.
    #[arg(long)]
    mcp: bool,
}

impl Args {
    /// Resolves the config path, falling back to a platform-appropriate default.
    fn resolved_config(&self) -> PathBuf {
        if let Some(path) = &self.config {
            return path.clone();
        }
        default_config_path()
    }
}

/// Returns the default config file path.
///
/// - macOS `.app` bundle: `~/Library/Application Support/TunnelDesk/config.toml`,
///   else `config.toml` in the working directory
/// - Windows: `%APPDATA%\TunnelDesk\config.toml`
/// - Everywhere else: `~/.config/TunnelDesk/config.toml`
fn default_config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe) = std::env::current_exe() {
            if exe.to_string_lossy().contains(".app/Contents/MacOS/") {
                if let Some(home) = std::env::var_os("HOME") {
                    return PathBuf::from(home)
                        .join("Library/Application Support/TunnelDesk/config.toml");
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata)
                .join("TunnelDesk")
                .join("config.toml");
        }
    }
    #[cfg(unix)]
    {
        // Linux and other Unix-like systems
        if let Some(home_dir) = home::home_dir() {
            return home_dir.join(".config/TunnelDesk/config.toml");
        }
    }
    PathBuf::from("config.toml")
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Guard against --mcp on builds that lack the feature.
    #[cfg(not(feature = "mcp"))]
    if args.mcp {
        anyhow::bail!(
            "This binary was not compiled with the 'mcp' feature. \
             Rebuild with `--features mcp`."
        );
    }

    // When acting as an MCP server, stdout carries the JSON-RPC protocol
    // stream, so tracing must be redirected to stderr instead.
    #[cfg(feature = "mcp")]
    if args.mcp {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(std::io::stderr)
            .init();
    } else {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(subscriber)?;
    }
    // When the mcp feature is absent --mcp is already rejected above, so we
    // always take the normal tracing path here.
    #[cfg(not(feature = "mcp"))]
    {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(subscriber)?;
    }

    // --mcp implies headless: the GUI must not own stdout.
    #[cfg(feature = "gui")]
    if !args.no_gui && !args.mcp {
        gui::launch(args); // diverges: tao event loop runs forever
    }

    tokio::runtime::Runtime::new()?.block_on(run_headless(args))
}

async fn run_headless(args: Args) -> anyhow::Result<()> {
    let (tunnel_manager, web_server_handle, _port) = init_app(&args).await?;

    tunnel_manager.wait_for_shutdown_signal().await;
    tunnel_manager.shutdown().await;
    web_server_handle.abort();

    Ok(())
}

/// Initialises all app components and returns handles needed for lifecycle management.
/// Returns `(tunnel_manager, web_server_handle, gui_port)`.
pub(crate) async fn init_app(
    args: &Args,
) -> anyhow::Result<(Arc<TunnelManager>, tokio::task::JoinHandle<()>, u16)> {
    // Load configuration
    let config_path = args.resolved_config();
    let mut config = if config_path.exists() {
        Config::from_file(&config_path)?
    } else {
        info!("Config file not found, using default configuration");
        Config::default_config()
    };

    // Cloudflare setup (only when [cloudflare] section is present)
    let tunnel_sync: Option<Arc<TunnelSync>> = setup_cloudflare(&mut config, &config_path).await;

    // Wrap config in shared Arc<RwLock> for live mutation by CRUD handlers.
    let shared_config = Arc::new(RwLock::new(config));

    // Create storage instances
    let cfg = shared_config.read().await;
    let request_storage = Arc::new(RequestStorage::new(cfg.capture.max_stored_requests));
    let websocket_storage = Arc::new(WebSocketMessageStorage::new(
        cfg.capture.max_stored_requests,
    ));
    let port = cfg.gui.port;
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

    // Create and start web server.
    // Clone tunnel_sync so the MCP server (when enabled) can share the same
    // Cloudflare sync handle without requiring a second Arc.
    let web_server = WebServer::new(
        shared_config.clone(),
        tunnel_manager.clone(),
        tunnel_sync.clone(),
        request_storage.clone(),
        websocket_storage.clone(),
    );

    // Start MCP stdio server when requested (feature-gated).
    #[cfg(feature = "mcp")]
    if args.mcp {
        let mcp_server = mcp::TunnelDeskMcp::new(
            shared_config.clone(),
            tunnel_manager.clone(),
            tunnel_sync,
            request_storage.clone(),
            websocket_storage.clone(),
        );
        tokio::spawn(async move {
            use rmcp::ServiceExt as _;
            match mcp_server.serve(rmcp::transport::io::stdio()).await {
                Ok(service) => {
                    if let Err(e) = service.waiting().await {
                        tracing::error!("MCP server error: {e}");
                    }
                }
                Err(e) => tracing::error!("Failed to start MCP server: {e}"),
            }
        });
    }

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

    Ok((tunnel_manager, web_server_handle, port))
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
