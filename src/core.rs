//! The core: one process per config file that owns the tunnels, captured
//! traffic, the `cloudflared` connector and the HTTP endpoint all frontends
//! (native window, browser, MCP) connect to.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::app_service::AppService;
use crate::cloudflare::CloudflareClient;
use crate::cloudflared::{CloudflaredProcess, ConnectorState};
use crate::config::Config;
use crate::storage::{RequestStorage, WebSocketMessageStorage};
use crate::sync::{SyncReport, TunnelSync};
use crate::tunnel::TunnelManager;
use crate::web_server::{ServerContext, WebServer};

const WEB_SERVER_STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// How a core was started.
#[derive(Debug, Clone, Copy)]
pub struct CoreOptions {
    /// Exit automatically once no client has been attached for
    /// `core.idle_timeout_secs`. Used for background cores started on demand.
    pub idle_shutdown: bool,
}

/// A running core for one canonical config path.
pub struct Core {
    app_service: Arc<AppService>,
    config_path: PathBuf,
    bound_port: u16,
    shutdown: CancellationToken,
    web_task: JoinHandle<()>,
    cloudflare_task: JoinHandle<()>,
    idle_task: Option<JoinHandle<()>>,
    cloudflared: Arc<Mutex<Option<CloudflaredProcess>>>,
}

impl Core {
    /// Loads (or creates) the config, binds the HTTP endpoint, starts all
    /// tunnels and kicks off the Cloudflare setup in the background.
    ///
    /// `config_path` should be canonical so that it matches the instance key.
    pub async fn start(config_path: &Path, options: CoreOptions) -> anyhow::Result<Self> {
        let mut config = Config::load_or_create(config_path)?;
        if config.ensure_access_token() {
            config.save_to_file(config_path)?;
            info!("Generated access token in {}", config_path.display());
        }

        let access_token = config.gui.access_token.clone().unwrap_or_default();
        let allowed_origins = config.gui.allowed_origins.clone();
        let idle_timeout = Duration::from_secs(config.core.idle_timeout_secs);

        let request_storage = Arc::new(RequestStorage::new(config.capture.max_stored_requests));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(
            config.capture.max_stored_requests,
        ));
        let tunnel_manager = Arc::new(TunnelManager::new(
            &config,
            request_storage.clone(),
            websocket_storage.clone(),
        ));
        let (listener, bound_port) = WebServer::bind(config.gui.port).await?;

        let shared_config = Arc::new(RwLock::new(config));
        let app_service = Arc::new(AppService::new(
            shared_config.clone(),
            tunnel_manager.clone(),
            request_storage,
            websocket_storage,
        ));

        let shutdown = CancellationToken::new();
        let clients = ClientRegistry::new();
        let context = ServerContext {
            bound_port,
            access_token,
            allowed_origins,
            config_path: config_path.to_path_buf(),
            config_hash: crate::instance::config_hash(config_path),
            shutdown: shutdown.clone(),
            clients: clients.clone(),
            idle_timeout: options.idle_shutdown.then_some(idle_timeout),
        };
        let web_server = WebServer::with_context(app_service.clone(), context);
        let web_task = tokio::spawn(async move {
            if let Err(e) = web_server.serve(listener).await {
                tracing::error!("Web server error: {e}");
            }
        });
        info!("TunnelDesk core listening on http://127.0.0.1:{bound_port}");

        {
            let cfg = shared_config.read().await;
            tunnel_manager.start_tunnels(&cfg).await;
        }

        let cloudflared = Arc::new(Mutex::new(None));
        let cloudflare_task = tokio::spawn(setup_cloudflare(
            app_service.clone(),
            config_path.to_path_buf(),
            cloudflared.clone(),
        ));

        let idle_task = options
            .idle_shutdown
            .then(|| tokio::spawn(watch_idle(clients.clone(), idle_timeout, shutdown.clone())));

        Ok(Self {
            app_service,
            config_path: config_path.to_path_buf(),
            bound_port,
            shutdown,
            web_task,
            cloudflare_task,
            idle_task,
            cloudflared,
        })
    }

    /// The TCP port the HTTP endpoint is actually bound to.
    pub fn port(&self) -> u16 {
        self.bound_port
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Cancelled when a shutdown is requested (via API, UI or idle timeout).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Removes the Cloudflare routes, stops `cloudflared`, the tunnels and the
    /// HTTP endpoint, in that order.
    pub async fn shutdown(self) {
        info!("Shutting down TunnelDesk core...");
        self.shutdown.cancel();
        self.cloudflare_task.abort();
        let _ = self.cloudflare_task.await;
        if let Some(idle) = self.idle_task {
            idle.abort();
        }

        if let Some(sync) = self.app_service.tunnel_sync() {
            let cfg = self.app_service.config.read().await;
            match sync.remove_all_configured_tunnels(&cfg).await {
                Ok(_) => info!("Removed all configured tunnels from Cloudflare"),
                Err(e) => {
                    tracing::warn!("Failed to remove tunnels from Cloudflare during shutdown: {e}")
                }
            }
        }

        if let Some(process) = self.cloudflared.lock().await.take() {
            process.shutdown().await;
        }

        self.app_service.tunnel_manager.shutdown().await;

        let mut web_task = self.web_task;
        if tokio::time::timeout(WEB_SERVER_STOP_TIMEOUT, &mut web_task)
            .await
            .is_err()
        {
            web_task.abort();
        }
        info!("TunnelDesk core stopped");
    }
}

/// Performs Cloudflare setup if `[cloudflare]` is configured: creates the
/// tunnel on first run, starts the connector and performs an initial sync.
async fn setup_cloudflare(
    app_service: Arc<AppService>,
    config_path: PathBuf,
    cloudflared: Arc<Mutex<Option<CloudflaredProcess>>>,
) {
    let Some(cf_cfg) = app_service.config.read().await.cloudflare.clone() else {
        return;
    };

    let client = match CloudflareClient::new(&cf_cfg.api_token, &cf_cfg.account_id, &cf_cfg.zone_id)
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create Cloudflare client: {e}");
            return;
        }
    };

    let (tunnel_id, tunnel_token) = match (cf_cfg.tunnel_id, cf_cfg.tunnel_token) {
        (Some(id), token) => (id, token.unwrap_or_default()),
        (None, _) => {
            info!("No tunnel_id configured; creating a new Cloudflare tunnel...");
            let secret = generate_tunnel_secret();
            let tunnel_id = match client.create_tunnel(&cf_cfg.tunnel_name, &secret).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!("Failed to create Cloudflare tunnel: {e}");
                    return;
                }
            };
            info!("Created Cloudflare tunnel: {tunnel_id}");

            let token = match client.get_tunnel_token(&tunnel_id).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("Failed to get tunnel token: {e}");
                    return;
                }
            };

            let mut cfg = app_service.config.write().await;
            if let Err(e) = TunnelSync::save_tunnel_credentials(
                &mut cfg,
                &config_path,
                tunnel_id.clone(),
                token.clone(),
            ) {
                tracing::error!("Failed to save tunnel credentials to config: {e}");
                return;
            }
            info!("Saved tunnel credentials to {}", config_path.display());
            (tunnel_id, token)
        }
    };

    // Install the sync handle before touching Cloudflare so that a shutdown
    // during the initial sync still removes the routes.
    let sync = Arc::new(TunnelSync::new(client, &tunnel_id));
    app_service.set_tunnel_sync(sync.clone());

    let connector = app_service.connector_sender();
    if !cf_cfg.manage_cloudflared {
        connector.send_replace(ConnectorState::External);
    } else if tunnel_token.is_empty() {
        tracing::warn!("No tunnel_token configured; cannot start cloudflared");
    } else {
        let process = CloudflaredProcess::spawn(tunnel_token, connector);
        cloudflared.lock().await.replace(process);
    }

    let report: SyncReport = {
        let cfg = app_service.config.read().await;
        sync.sync_to_cloudflare(&cfg).await
    };
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
}

/// Generates a base64-encoded random 32-byte tunnel secret.
fn generate_tunnel_secret() -> String {
    use base64::Engine as _;
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    base64::engine::general_purpose::STANDARD.encode(secret)
}

/// Blocks until Ctrl-C or SIGTERM is received.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => info!("Received Ctrl+C, shutting down..."),
            _ = sigterm.recv() => info!("Received SIGTERM, shutting down..."),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        info!("Received Ctrl+C, shutting down...");
    }
}

// ── Client tracking ───────────────────────────────────────────────────────────

/// Tracks attached frontends and recent activity for the idle shutdown.
pub struct ClientRegistry {
    count: watch::Sender<usize>,
    last_activity: std::sync::Mutex<Instant>,
}

impl ClientRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            count: watch::channel(0).0,
            last_activity: std::sync::Mutex::new(Instant::now()),
        })
    }

    /// Registers an attached client until the returned guard is dropped.
    pub fn attach(self: &Arc<Self>) -> ClientGuard {
        self.count.send_modify(|c| *c += 1);
        self.touch();
        ClientGuard {
            registry: self.clone(),
        }
    }

    /// Records activity, e.g. an authenticated HTTP request.
    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    pub fn count(&self) -> usize {
        *self.count.borrow()
    }

    /// Notifies on every change of the attached-client count.
    pub fn subscribe(&self) -> watch::Receiver<usize> {
        self.count.subscribe()
    }

    /// Time since the last activity; zero while any client is attached.
    pub fn idle_for(&self) -> Duration {
        if self.count() > 0 {
            return Duration::ZERO;
        }
        self.last_activity.lock().unwrap().elapsed()
    }
}

/// Keeps a client counted as attached for its lifetime.
pub struct ClientGuard {
    registry: Arc<ClientRegistry>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.registry
            .count
            .send_modify(|c| *c = c.saturating_sub(1));
        self.registry.touch();
    }
}

/// Cancels `shutdown` once no client has been attached or active for `timeout`.
pub async fn watch_idle(
    registry: Arc<ClientRegistry>,
    timeout: Duration,
    shutdown: CancellationToken,
) {
    let check_every = (timeout / 4).clamp(Duration::from_millis(50), Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tokio::time::sleep(check_every) => {}
            _ = shutdown.cancelled() => return,
        }
        if registry.idle_for() >= timeout {
            info!(
                "No clients for {}s; shutting down idle core",
                timeout.as_secs()
            );
            shutdown.cancel();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_track_attached_clients() {
        let registry = ClientRegistry::new();
        let rx = registry.subscribe();
        assert_eq!(registry.count(), 0);
        let a = registry.attach();
        let b = registry.attach();
        assert_eq!(registry.count(), 2);
        assert_eq!(*rx.borrow(), 2);
        drop(a);
        assert_eq!(registry.count(), 1);
        drop(b);
        assert_eq!(registry.count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_for_is_zero_while_attached() {
        let registry = ClientRegistry::new();
        let guard = registry.attach();
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(registry.idle_for(), Duration::ZERO);
        drop(guard);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(registry.idle_for(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_watcher_shuts_down_after_timeout() {
        let registry = ClientRegistry::new();
        let shutdown = CancellationToken::new();
        let watcher = tokio::spawn(watch_idle(
            registry.clone(),
            Duration::from_secs(30),
            shutdown.clone(),
        ));

        tokio::time::sleep(Duration::from_secs(20)).await;
        assert!(!shutdown.is_cancelled());
        tokio::time::sleep(Duration::from_secs(15)).await;
        assert!(shutdown.is_cancelled());
        watcher.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_watcher_waits_while_clients_attached() {
        let registry = ClientRegistry::new();
        let shutdown = CancellationToken::new();
        let guard = registry.attach();
        let watcher = tokio::spawn(watch_idle(
            registry.clone(),
            Duration::from_secs(30),
            shutdown.clone(),
        ));

        tokio::time::sleep(Duration::from_secs(120)).await;
        assert!(!shutdown.is_cancelled());

        drop(guard);
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert!(
            !shutdown.is_cancelled(),
            "timeout restarts when the last client leaves"
        );
        tokio::time::sleep(Duration::from_secs(15)).await;
        assert!(shutdown.is_cancelled());
        watcher.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn activity_postpones_idle_shutdown() {
        let registry = ClientRegistry::new();
        let shutdown = CancellationToken::new();
        let watcher = tokio::spawn(watch_idle(
            registry.clone(),
            Duration::from_secs(30),
            shutdown.clone(),
        ));

        for _ in 0..4 {
            tokio::time::sleep(Duration::from_secs(20)).await;
            registry.touch();
        }
        assert!(!shutdown.is_cancelled());
        shutdown.cancel();
        watcher.await.unwrap();
    }

    #[tokio::test]
    async fn core_starts_serves_and_shuts_down() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gui]\nport = 0\n").unwrap();

        let core = Core::start(
            &path,
            CoreOptions {
                idle_shutdown: false,
            },
        )
        .await
        .unwrap();
        assert_ne!(core.port(), 0, "port 0 must resolve to a real port");

        let config = Config::from_file(&path).unwrap();
        assert!(
            config.gui.access_token.is_some(),
            "access token must be persisted on first start"
        );

        let health = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://127.0.0.1:{}/api/health", core.port()))
            .send()
            .await
            .unwrap();
        assert!(health.status().is_success());

        let port = core.port();
        tokio::time::timeout(Duration::from_secs(10), core.shutdown())
            .await
            .expect("shutdown hung");
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "listener must be closed after shutdown"
        );
    }

    #[tokio::test]
    async fn core_with_idle_shutdown_cancels_token_when_unused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gui]\nport = 0\n[core]\nidle_timeout_secs = 1\n").unwrap();

        let core = Core::start(
            &path,
            CoreOptions {
                idle_shutdown: true,
            },
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), core.shutdown_token().cancelled())
            .await
            .expect("idle core did not request shutdown");
        core.shutdown().await;
    }
}
