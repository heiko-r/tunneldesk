use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::info;

use crate::config::Config;
use crate::proxy::Proxy;
use crate::storage::{RequestStorage, WebSocketMessageStorage};
use std::sync::Arc;

/// Manages the lifecycle of all configured proxy tunnels.
///
/// Creates one [`Proxy`] task per tunnel and provides cooperative shutdown via
/// a shared [`CancellationToken`].
pub struct TunnelManager {
    config: Config,
    cancel_token: CancellationToken,
    tracker: TaskTracker,
    request_storage: Arc<RequestStorage>,
    websocket_storage: Arc<WebSocketMessageStorage>,
}

impl TunnelManager {
    /// Creates a new `TunnelManager` from `config`.  Tunnels are not started
    /// until [`start_tunnels`](Self::start_tunnels) is called.
    pub fn new(
        config: Config,
        request_storage: Arc<RequestStorage>,
        websocket_storage: Arc<WebSocketMessageStorage>,
    ) -> Self {
        let cancel_token = CancellationToken::new();
        let tracker = TaskTracker::new();

        Self {
            config,
            cancel_token,
            tracker,
            request_storage,
            websocket_storage,
        }
    }

    /// Spawns a proxy task for each tunnel in the configuration.
    pub fn start_tunnels(&self) {
        info!(
            "Starting TunnelDesk proxy with {} tunnels",
            self.config.tunnels.len()
        );

        // Start proxy for each tunnel
        for tunnel_config in self.config.tunnels.clone() {
            let proxy = Proxy::new(
                tunnel_config.clone(),
                self.request_storage.clone(),
                self.websocket_storage.clone(),
                &self.config.logging.stdout_level,
                self.config.capture.max_request_body_size,
                self.config.logging.max_request_body_size,
            );
            let task_cancel_token = self.cancel_token.clone();

            self.tracker.spawn(async move {
                if let Err(e) = proxy.start(task_cancel_token).await {
                    tracing::error!("Tunnel '{}' error: {}", tunnel_config.name, e);
                }
            });
        }
    }

    /// Signals all tunnel tasks to stop and waits for them to finish.
    pub async fn shutdown(&self) {
        info!("Shutting down TunnelDesk proxy...");

        // Send shutdown signal to all tunnels
        self.cancel_token.cancel();
        self.tracker.close();
        self.tracker.wait().await;

        info!("TunnelDesk proxy stopped");
    }

    #[cfg(test)]
    fn tunnel_count(&self) -> usize {
        self.config.tunnels.len()
    }

    /// Blocks until Ctrl-C or SIGTERM is received, then returns so the caller
    /// can invoke [`shutdown`](Self::shutdown).
    pub async fn wait_for_shutdown_signal(&self) {
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
                },
                TunnelConfig {
                    name: "test-tunnel-2".to_string(),
                    domain: "test-2.tunnel.example.com".to_string(),
                    socket_path: "/tmp/test2.sock".to_string(),
                    target_port: 3002,
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
        }
    }

    #[test]
    fn test_tunnel_manager_new() {
        let config = create_test_config();
        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let manager = TunnelManager::new(config, request_storage, websocket_storage);
        assert_eq!(manager.tunnel_count(), 2);
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
        };

        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let manager = TunnelManager::new(config, request_storage, websocket_storage);
        assert_eq!(manager.tunnel_count(), 0);
    }

    #[test]
    fn test_tunnel_manager_with_single_tunnel() {
        let config = Config {
            tunnels: vec![TunnelConfig {
                name: "single-tunnel".to_string(),
                domain: "single.tunnel.example.com".to_string(),
                socket_path: "/tmp/single.sock".to_string(),
                target_port: 8080,
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
        };

        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let manager = TunnelManager::new(config, request_storage, websocket_storage);
        assert_eq!(manager.tunnel_count(), 1);
    }

    #[tokio::test]
    async fn test_shutdown_without_start() {
        let config = create_test_config();
        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let manager = TunnelManager::new(config, request_storage, websocket_storage);
        // Calling shutdown before start_tunnels must complete without panic or hang.
        manager.shutdown().await;
    }
}
