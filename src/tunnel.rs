use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::info;

use crate::config::{Config, LoggingConfig};
use crate::proxy::Proxy;
use crate::storage::{RequestStorage, WebSocketMessageStorage};

/// Manages the lifecycle of all configured proxy tunnels.
///
/// Supports dynamic per-tunnel start, stop, and restart in addition to the
/// bulk operations used at startup and shutdown.
pub struct TunnelManager {
    logging: LoggingConfig,
    capture_max_body: usize,
    /// Global cancellation token — cancelling this stops all tunnels.
    cancel_token: CancellationToken,
    /// Task tracker used for bulk shutdown.
    tracker: TaskTracker,
    request_storage: Arc<RequestStorage>,
    websocket_storage: Arc<WebSocketMessageStorage>,
    /// Per-tunnel handles: name → (individual cancel token, join handle).
    handles: Mutex<HashMap<String, (CancellationToken, JoinHandle<()>)>>,
}

impl TunnelManager {
    /// Creates a new `TunnelManager`. Tunnels are not started until
    /// [`start_tunnels`](Self::start_tunnels) or
    /// [`start_tunnel`](Self::start_tunnel) is called.
    pub fn new(
        config: &Config,
        request_storage: Arc<RequestStorage>,
        websocket_storage: Arc<WebSocketMessageStorage>,
    ) -> Self {
        let cancel_token = CancellationToken::new();
        let tracker = TaskTracker::new();

        Self {
            logging: config.logging.clone(),
            capture_max_body: config.capture.max_request_body_size,
            cancel_token,
            tracker,
            request_storage,
            websocket_storage,
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// Spawns a proxy task for a single tunnel.
    pub async fn start_tunnel(&self, tunnel_config: crate::config::TunnelConfig) {
        let name = tunnel_config.name.clone();
        let proxy = Proxy::new(
            tunnel_config,
            self.request_storage.clone(),
            self.websocket_storage.clone(),
            &self.logging.stdout_level,
            self.capture_max_body,
            self.logging.max_request_body_size,
        );

        // Each tunnel gets a child token so it can be cancelled individually.
        let tunnel_cancel = self.cancel_token.child_token();
        let tunnel_cancel_clone = tunnel_cancel.clone();
        let name_for_log = name.clone();

        let handle = self.tracker.spawn(async move {
            if let Err(e) = proxy.start(tunnel_cancel_clone).await {
                tracing::error!("Tunnel '{}' error: {}", name_for_log, e);
            }
        });

        self.handles
            .lock()
            .await
            .insert(name.clone(), (tunnel_cancel, handle));

        info!("Started tunnel '{}'", name);
    }

    /// Stops a single tunnel by name.
    pub async fn stop_tunnel(&self, name: &str) {
        let entry = self.handles.lock().await.remove(name);
        if let Some((token, handle)) = entry {
            token.cancel();
            handle.await.ok();
            info!("Stopped tunnel '{}'", name);
        }
    }

    /// Restarts a single tunnel with (potentially updated) config.
    pub async fn restart_tunnel(&self, name: &str, new_config: crate::config::TunnelConfig) {
        self.stop_tunnel(name).await;
        self.start_tunnel(new_config).await;
    }

    /// Spawns proxy tasks for every tunnel in `config`.
    pub async fn start_tunnels(&self, config: &Config) {
        info!(
            "Starting TunnelDesk proxy with {} tunnels",
            config.tunnels.len()
        );
        for tunnel_config in config.tunnels.clone() {
            self.start_tunnel(tunnel_config).await;
        }
    }

    /// Signals all tunnel tasks to stop and waits for them to finish.
    pub async fn shutdown(&self) {
        info!("Shutting down TunnelDesk proxy...");
        self.cancel_token.cancel();
        self.tracker.close();
        self.tracker.wait().await;
        info!("TunnelDesk proxy stopped");
    }

    /// Returns `true` if a handle for `name` exists (test-only).
    #[cfg(test)]
    pub async fn is_tunnel_running(&self, name: &str) -> bool {
        self.handles.lock().await.contains_key(name)
    }

    /// Blocks until Ctrl-C or SIGTERM is received.
    pub async fn wait_for_shutdown_signal(&self) {
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Received Ctrl+C, shutting down...");
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, shutting down...");
                }
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok();
            info!("Received Ctrl+C, shutting down...");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CaptureConfig, Config, GuiConfig, LoggingConfig, TunnelConfig};

    fn create_test_config() -> Config {
        Config {
            tunnels: vec![
                TunnelConfig {
                    name: "test-tunnel-1".to_string(),
                    domain: "test-1.tunnel.example.com".to_string(),
                    socket_path: "/tmp/test1.sock".to_string(),
                    target_port: 3001,
                    enabled: true,
                },
                TunnelConfig {
                    name: "test-tunnel-2".to_string(),
                    domain: "test-2.tunnel.example.com".to_string(),
                    socket_path: "/tmp/test2.sock".to_string(),
                    target_port: 3002,
                    enabled: true,
                },
            ],
            logging: LoggingConfig {
                stdout_level: "debug".to_string(),
                max_request_body_size: 1024,
            },
            capture: CaptureConfig {
                max_stored_requests: 500,
                max_request_body_size: 10 * 1024 * 1024, // 10MB
            },
            gui: GuiConfig { port: 8081 },
            cloudflare: None,
            config_path: None,
        }
    }

    #[test]
    fn test_tunnel_manager_new() {
        let config = create_test_config();
        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        // Just check it constructs without panic.
        let _ = TunnelManager::new(&config, request_storage, websocket_storage);
    }

    #[test]
    fn test_tunnel_manager_with_empty_tunnels() {
        let config = Config {
            tunnels: vec![],
            logging: LoggingConfig {
                stdout_level: "info".to_string(),
                max_request_body_size: 1024,
            },
            capture: CaptureConfig {
                max_stored_requests: 100,
                max_request_body_size: 10 * 1024 * 1024,
            },
            gui: GuiConfig { port: 8081 },
            cloudflare: None,
            config_path: None,
        };

        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let _ = TunnelManager::new(&config, request_storage, websocket_storage);
    }

    #[test]
    fn test_tunnel_manager_with_single_tunnel() {
        let config = Config {
            tunnels: vec![TunnelConfig {
                name: "single-tunnel".to_string(),
                domain: "single.tunnel.example.com".to_string(),
                socket_path: "/tmp/single.sock".to_string(),
                target_port: 8080,
                enabled: true,
            }],
            logging: LoggingConfig {
                stdout_level: "basic".to_string(),
                max_request_body_size: 1024,
            },
            capture: CaptureConfig {
                max_stored_requests: 1000,
                max_request_body_size: 10 * 1024 * 1024,
            },
            gui: GuiConfig { port: 8081 },
            cloudflare: None,
            config_path: None,
        };

        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let _ = TunnelManager::new(&config, request_storage, websocket_storage);
    }

    #[tokio::test]
    async fn test_shutdown_without_start() {
        let config = create_test_config();
        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let manager = TunnelManager::new(&config, request_storage, websocket_storage);
        // Calling shutdown before start_tunnels must complete without panic or hang.
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn test_stop_nonexistent_tunnel_is_noop() {
        let config = create_test_config();
        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let manager = TunnelManager::new(&config, request_storage, websocket_storage);
        // Should not panic.
        manager.stop_tunnel("no-such-tunnel").await;
    }
}
