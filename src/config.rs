use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub tunnels: Vec<TunnelConfig>,
    pub logging: LoggingConfig,
    pub capture: CaptureConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub stdout_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub max_stored_requests: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub name: String,
    pub socket_path: String,
    pub target_port: u16,
}

impl Config {
    pub fn from_file<P: AsRef<std::path::Path>>(path: P) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn default_config() -> Self {
        Config {
            tunnels: vec![TunnelConfig {
                name: "webapp".to_string(),
                socket_path: "/tmp/webapp.sock".to_string(),
                target_port: 8080,
            }],
            logging: LoggingConfig {
                stdout_level: "basic".to_string(),
            },
            capture: CaptureConfig {
                max_stored_requests: 1000,
            },
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
        assert_eq!(config.capture.max_stored_requests, 1000);
    }

    #[test]
    fn test_config_from_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_config.toml");
        let config_content = r#"
[[tunnels]]
name = "test-tunnel"
socket_path = "/tmp/test.sock"
target_port = 3000
 
[logging]
stdout_level = "debug"
 
[capture]
max_stored_requests = 500
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
