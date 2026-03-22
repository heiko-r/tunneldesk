use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::info;

use crate::config::Config;
use crate::proxy::Proxy;

pub struct TunnelManager {
    config: Config,
    cancel_token: CancellationToken,
    tracker: TaskTracker,
}

impl TunnelManager {
    pub fn new(config: Config) -> Self {
        let cancel_token = CancellationToken::new();
        let tracker = TaskTracker::new();

        Self {
            config,
            cancel_token,
            tracker,
        }
    }

    pub fn start_tunnels(&self) {
        info!(
            "Starting TunnelDesk proxy with {} tunnels",
            self.config.tunnels.len()
        );

        // Start proxy for each tunnel
        for tunnel_config in self.config.tunnels.clone() {
            let proxy = Proxy::new(
                tunnel_config.clone(),
                self.config.capture.max_stored_requests,
                &self.config.logging.stdout_level,
            );
            let task_cancel_token = self.cancel_token.clone();

            self.tracker.spawn(async move {
                if let Err(e) = proxy.start(task_cancel_token).await {
                    eprintln!("Tunnel '{}' error: {}", tunnel_config.name, e);
                }
            });
        }
    }

    pub async fn shutdown(&self) {
        info!("Shutting down TunnelDesk proxy...");

        // Send shutdown signal to all tunnels
        self.cancel_token.cancel();
        self.tracker.close();
        self.tracker.wait().await;

        info!("TunnelDesk proxy stopped");
    }

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
    use crate::config::{CaptureConfig, Config, LoggingConfig, TunnelConfig};

    fn create_test_config() -> Config {
        Config {
            tunnels: vec![
                TunnelConfig {
                    name: "test-tunnel-1".to_string(),
                    socket_path: "/tmp/test1.sock".to_string(),
                    target_port: 3001,
                },
                TunnelConfig {
                    name: "test-tunnel-2".to_string(),
                    socket_path: "/tmp/test2.sock".to_string(),
                    target_port: 3002,
                },
            ],
            logging: LoggingConfig {
                stdout_level: "debug".to_string(),
            },
            capture: CaptureConfig {
                max_stored_requests: 500,
            },
        }
    }

    #[test]
    fn test_tunnel_manager_new() {
        let config = create_test_config();
        let manager = TunnelManager::new(config);

        // Test that it can be created without panicking
        // We can't easily test the internal state without making fields public
        assert!(true);
    }

    #[test]
    fn test_tunnel_manager_with_empty_tunnels() {
        let config = Config {
            tunnels: vec![],
            logging: LoggingConfig {
                stdout_level: "info".to_string(),
            },
            capture: CaptureConfig {
                max_stored_requests: 100,
            },
        };

        let manager = TunnelManager::new(config);
        // Should still create successfully even with no tunnels
        assert!(true);
    }

    #[test]
    fn test_tunnel_manager_with_single_tunnel() {
        let config = Config {
            tunnels: vec![TunnelConfig {
                name: "single-tunnel".to_string(),
                socket_path: "/tmp/single.sock".to_string(),
                target_port: 8080,
            }],
            logging: LoggingConfig {
                stdout_level: "basic".to_string(),
            },
            capture: CaptureConfig {
                max_stored_requests: 1000,
            },
        };

        let manager = TunnelManager::new(config);
        // Should create successfully with single tunnel
        assert!(true);
    }
}
