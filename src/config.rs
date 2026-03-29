use serde::{Deserialize, Serialize};

/// Top-level application configuration, loaded from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// One entry per Cloudflare Tunnel to proxy.
    pub tunnels: Vec<TunnelConfig>,
    /// Controls what is written to stdout.
    pub logging: LoggingConfig,
    /// Controls how captured traffic is stored in memory.
    pub capture: CaptureConfig,
    /// Configuration for the local web UI server.
    pub gui: GuiConfig,
}

/// Controls what traffic information is written to stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Verbosity of stdout logging.  Accepted values: `"off"`, `"basic"`, `"full"`.
    pub stdout_level: String,
    /// Maximum bytes of a request/response/WebSocket body to log to stdout when
    /// `stdout_level` is `full`.  Independent of the stored body size limit.
    /// Defaults to 1 KiB.
    #[serde(default = "LoggingConfig::default_max_request_body_size")]
    pub max_request_body_size: usize,
}

impl LoggingConfig {
    fn default_max_request_body_size() -> usize {
        1024
    }
}

/// Controls how captured traffic is retained in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    /// Maximum number of request–response exchanges kept in memory.
    /// When the limit is reached the oldest entry is evicted.
    pub max_stored_requests: usize,
    /// Maximum bytes of a body to store.  Bodies that exceed this limit are
    /// truncated in storage; proxying always forwards the full body.
    pub max_request_body_size: usize,
}

/// Configuration for the built-in web UI server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    /// TCP port the web UI listens on (e.g. `8081`).
    pub port: u16,
}

/// Configuration for a single Cloudflare Tunnel proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    /// Human-readable name used to identify this tunnel in logs and the UI.
    pub name: String,
    /// Public hostname of the Cloudflare Tunnel (used for display only).
    pub domain: String,
    /// Filesystem path to the Unix domain socket that `cloudflared` writes to.
    pub socket_path: String,
    /// Local TCP port to forward tunnel traffic to.
    pub target_port: u16,
}

impl Config {
    /// Loads configuration from a TOML file at `path`.
    pub fn from_file<P: AsRef<std::path::Path>>(path: P) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Returns a built-in default configuration suitable for development.
    pub fn default_config() -> Self {
        Config {
            tunnels: vec![TunnelConfig {
                name: "webapp".to_string(),
                domain: "webapp.tunnel.example.com".to_string(),
                socket_path: "/tmp/webapp.sock".to_string(),
                target_port: 8080,
            }],
            logging: LoggingConfig {
                stdout_level: "basic".to_string(),
                max_request_body_size: LoggingConfig::default_max_request_body_size(),
            },
            capture: CaptureConfig {
                max_stored_requests: 1000,
                max_request_body_size: 10485760, // 10MB
            },
            gui: GuiConfig { port: 8081 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = Config::default_config();

        assert_eq!(config.tunnels.len(), 1);
        assert_eq!(config.tunnels[0].name, "webapp");
        assert_eq!(config.tunnels[0].socket_path, "/tmp/webapp.sock");
        assert_eq!(config.tunnels[0].target_port, 8080);
        assert_eq!(config.logging.stdout_level, "basic");
        assert_eq!(config.logging.max_request_body_size, 1024);
        assert_eq!(config.capture.max_stored_requests, 1000);
    }

    #[test]
    fn test_logging_max_request_body_size_default_when_absent() {
        // A TOML config without max_request_body_size in [logging] should
        // deserialise to the serde default (1024).
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config_no_log_limit.toml");
        let config_content = r#"
[[tunnels]]
name = "t"
domain = "t.example.com"
socket_path = "/tmp/t.sock"
target_port = 3000

[logging]
stdout_level = "basic"

[capture]
max_stored_requests = 100
max_request_body_size = 1048576

[gui]
port = 8081
"#;
        fs::File::create(&file_path)
            .unwrap()
            .write_all(config_content.as_bytes())
            .unwrap();

        let config = Config::from_file(&file_path).unwrap();
        assert_eq!(config.logging.max_request_body_size, 1024);
    }

    #[test]
    fn test_logging_max_request_body_size_explicit() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config_log_limit.toml");
        let config_content = r#"
[[tunnels]]
name = "t"
domain = "t.example.com"
socket_path = "/tmp/t.sock"
target_port = 3000

[logging]
stdout_level = "full"
max_request_body_size = 4096

[capture]
max_stored_requests = 100
max_request_body_size = 1048576

[gui]
port = 8081
"#;
        fs::File::create(&file_path)
            .unwrap()
            .write_all(config_content.as_bytes())
            .unwrap();

        let config = Config::from_file(&file_path).unwrap();
        assert_eq!(config.logging.max_request_body_size, 4096);
    }

    #[test]
    fn test_config_from_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_config.toml");
        let config_content = r#"
[[tunnels]]
name = "test-tunnel"
domain = "test.tunnel.example.com"
socket_path = "/tmp/test.sock"
target_port = 3000
 
[logging]
stdout_level = "debug"
 
[capture]
max_stored_requests = 500
max_request_body_size = 10485760

[gui]
port = 8081
"#;
        let mut file = fs::File::create(&file_path).unwrap();
        file.write_all(config_content.as_bytes()).unwrap();

        let config = Config::from_file(&file_path).unwrap();

        assert_eq!(config.tunnels.len(), 1);
        assert_eq!(config.tunnels[0].name, "test-tunnel");
        assert_eq!(config.tunnels[0].socket_path, "/tmp/test.sock");
        assert_eq!(config.tunnels[0].target_port, 3000);
        assert_eq!(config.logging.stdout_level, "debug");
        assert_eq!(config.capture.max_stored_requests, 500);
    }

    #[test]
    fn test_config_from_file_invalid() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("invalid_config.toml");
        let mut file = fs::File::create(&file_path).unwrap();
        file.write_all(b"invalid toml content").unwrap();

        let result = Config::from_file(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_from_file_not_found() {
        let result = Config::from_file("/nonexistent/path/config.toml");
        assert!(result.is_err());
    }
}
