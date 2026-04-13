use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level application configuration, loaded from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// One entry per Cloudflare Tunnel to proxy.
    #[serde(default)]
    pub tunnels: Vec<TunnelConfig>,
    /// Controls what is written to stdout.
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Controls how captured traffic is stored in memory.
    #[serde(default)]
    pub capture: CaptureConfig,
    /// Configuration for the local web UI server.
    #[serde(default)]
    pub gui: GuiConfig,
    /// Optional Cloudflare integration settings.
    #[serde(default)]
    pub cloudflare: Option<CloudflareConfig>,
    /// Path to the config file on disk (not serialized, set after loading).
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
}

/// Cloudflare integration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareConfig {
    /// Cloudflare API token with Zone:DNS:Edit and Account:Cloudflare Tunnel:Edit permissions.
    pub api_token: String,
    /// Cloudflare account ID.
    pub account_id: String,
    /// Cloudflare zone ID for DNS record management.
    pub zone_id: String,
    /// ID of the managed Cloudflare tunnel. Populated automatically on first run.
    #[serde(default)]
    pub tunnel_id: Option<String>,
    /// Display name for the managed Cloudflare tunnel.
    #[serde(default = "CloudflareConfig::default_tunnel_name")]
    pub tunnel_name: String,
    /// Connector token for `cloudflared service install`. Populated automatically on first run.
    #[serde(default)]
    pub tunnel_token: Option<String>,
}

impl CloudflareConfig {
    fn default_tunnel_name() -> String {
        "tunneldesk".to_string()
    }
}

/// Controls what traffic information is written to stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Verbosity of stdout logging.  Accepted values: `"off"`, `"basic"`, `"full"`.
    #[serde(default = "LoggingConfig::default_stdout_level")]
    pub stdout_level: String,
    /// Maximum bytes of a request/response/WebSocket body to log to stdout when
    /// `stdout_level` is `full`.  Independent of the stored body size limit.
    /// Defaults to 1 KiB.
    #[serde(default = "LoggingConfig::default_max_request_body_size")]
    pub max_request_body_size: usize,
}

impl LoggingConfig {
    fn default_stdout_level() -> String {
        "basic".to_string()
    }
    fn default_max_request_body_size() -> usize {
        1024
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            stdout_level: Self::default_stdout_level(),
            max_request_body_size: Self::default_max_request_body_size(),
        }
    }
}

/// Controls how captured traffic is retained in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    /// Maximum number of request–response exchanges kept in memory.
    /// When the limit is reached the oldest entry is evicted.
    #[serde(default = "CaptureConfig::default_max_stored_requests")]
    pub max_stored_requests: usize,
    /// Maximum bytes of a body to store.  Bodies that exceed this limit are
    /// truncated in storage; proxying always forwards the full body.
    #[serde(default = "CaptureConfig::default_max_request_body_size")]
    pub max_request_body_size: usize,
}

impl CaptureConfig {
    fn default_max_stored_requests() -> usize {
        1000
    }

    fn default_max_request_body_size() -> usize {
        10485760 // 10 MB
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        CaptureConfig {
            max_stored_requests: Self::default_max_stored_requests(),
            max_request_body_size: Self::default_max_request_body_size(),
        }
    }
}

/// Configuration for the built-in web UI server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    /// TCP port the web UI listens on (e.g. `3013`).
    #[serde(default = "GuiConfig::default_port")]
    pub port: u16,
}

impl GuiConfig {
    fn default_port() -> u16 {
        3013
    }
}

impl Default for GuiConfig {
    fn default() -> Self {
        GuiConfig {
            port: Self::default_port(),
        }
    }
}

/// Configuration for a single Cloudflare Tunnel proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    /// Human-readable name used to identify this tunnel in logs and the UI.
    pub name: String,
    /// Public hostname of the Cloudflare Tunnel.
    pub domain: String,
    /// Filesystem path to the Unix domain socket that `cloudflared` writes to.
    pub socket_path: String,
    /// Local TCP port to forward tunnel traffic to.
    pub target_port: u16,
    /// Whether this tunnel is active in Cloudflare. Disabled tunnels are still
    /// proxied locally but are removed from the Cloudflare ingress config.
    #[serde(default = "TunnelConfig::default_enabled")]
    pub enabled: bool,
}

impl TunnelConfig {
    fn default_enabled() -> bool {
        true
    }
}

impl Config {
    /// Loads configuration from a TOML file at `path`.
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        let mut config: Config = toml::from_str(&content)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// Saves the configuration back to the file it was loaded from.
    /// Uses `toml_edit` to preserve formatting and comments where possible.
    pub fn save_to_file(&self, path: &Path) -> anyhow::Result<()> {
        // Load existing document to preserve formatting, or start fresh.
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = existing.parse()?;

        // Remove existing [[tunnels]] and rewrite them.
        doc.remove("tunnels");
        let mut tunnels_table_array = toml_edit::ArrayOfTables::default();
        for tunnel in &self.tunnels {
            let mut t = toml_edit::Table::default();
            t["name"] = toml_edit::value(tunnel.name.clone());
            t["domain"] = toml_edit::value(tunnel.domain.clone());
            t["socket_path"] = toml_edit::value(tunnel.socket_path.clone());
            t["target_port"] = toml_edit::value(i64::from(tunnel.target_port));
            t["enabled"] = toml_edit::value(tunnel.enabled);
            tunnels_table_array.push(t);
        }
        doc["tunnels"] = toml_edit::Item::ArrayOfTables(tunnels_table_array);

        // Update [cloudflare] section if present.
        if let Some(cf) = &self.cloudflare {
            let cf_table =
                doc["cloudflare"].or_insert(toml_edit::Item::Table(toml_edit::Table::default()));
            cf_table["api_token"] = toml_edit::value(cf.api_token.clone());
            cf_table["account_id"] = toml_edit::value(cf.account_id.clone());
            cf_table["zone_id"] = toml_edit::value(cf.zone_id.clone());
            cf_table["tunnel_name"] = toml_edit::value(cf.tunnel_name.clone());
            if let Some(ref tid) = cf.tunnel_id {
                cf_table["tunnel_id"] = toml_edit::value(tid.clone());
            }
            if let Some(ref token) = cf.tunnel_token {
                cf_table["tunnel_token"] = toml_edit::value(token.clone());
            }
        }

        std::fs::write(path, doc.to_string())?;
        Ok(())
    }

    /// Returns a built-in default configuration suitable for development.
    pub fn default_config() -> Self {
        Config {
            tunnels: vec![],
            logging: LoggingConfig {
                stdout_level: "basic".to_string(),
                max_request_body_size: LoggingConfig::default_max_request_body_size(),
            },
            capture: CaptureConfig {
                max_stored_requests: 1000,
                max_request_body_size: 10485760, // 10MB
            },
            gui: GuiConfig { port: 8081 },
            cloudflare: None,
            config_path: None,
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

        assert_eq!(config.tunnels.len(), 0);
        assert_eq!(config.logging.stdout_level, "basic");
        assert_eq!(config.logging.max_request_body_size, 1024);
        assert_eq!(config.capture.max_stored_requests, 1000);
        assert!(config.cloudflare.is_none());
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
        // enabled defaults to true when absent
        assert!(config.tunnels[0].enabled);
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
        assert!(config.tunnels[0].enabled);
        assert_eq!(config.logging.stdout_level, "debug");
        assert_eq!(config.capture.max_stored_requests, 500);
        assert_eq!(config.config_path, Some(file_path.clone()));
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

    #[test]
    fn test_config_with_cloudflare_section() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config_cf.toml");
        let config_content = r#"
[[tunnels]]
name = "webapp"
domain = "webapp.example.com"
socket_path = "/tmp/webapp.sock"
target_port = 8080
enabled = true

[logging]
stdout_level = "basic"

[capture]
max_stored_requests = 1000
max_request_body_size = 10485760

[gui]
port = 8081

[cloudflare]
api_token = "test-token"
account_id = "acc123"
zone_id = "zone456"
tunnel_name = "myapp"
tunnel_id = "tid789"
tunnel_token = "tok-abc"
"#;
        fs::File::create(&file_path)
            .unwrap()
            .write_all(config_content.as_bytes())
            .unwrap();

        let config = Config::from_file(&file_path).unwrap();
        let cf = config.cloudflare.unwrap();
        assert_eq!(cf.api_token, "test-token");
        assert_eq!(cf.account_id, "acc123");
        assert_eq!(cf.zone_id, "zone456");
        assert_eq!(cf.tunnel_name, "myapp");
        assert_eq!(cf.tunnel_id, Some("tid789".to_string()));
        assert_eq!(cf.tunnel_token, Some("tok-abc".to_string()));
    }

    #[test]
    fn test_config_cloudflare_optional_fields_absent() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config_cf_minimal.toml");
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

[cloudflare]
api_token = "tok"
account_id = "acc"
zone_id = "zone"
"#;
        fs::File::create(&file_path)
            .unwrap()
            .write_all(config_content.as_bytes())
            .unwrap();

        let config = Config::from_file(&file_path).unwrap();
        let cf = config.cloudflare.unwrap();
        assert_eq!(cf.tunnel_name, "tunneldesk"); // default
        assert!(cf.tunnel_id.is_none());
        assert!(cf.tunnel_token.is_none());
    }

    #[test]
    fn test_tunnel_enabled_field_explicit_false() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("config_disabled.toml");
        let config_content = r#"
[[tunnels]]
name = "inactive"
domain = "inactive.example.com"
socket_path = "/tmp/inactive.sock"
target_port = 9000
enabled = false

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
        assert!(!config.tunnels[0].enabled);
    }

    #[test]
    fn test_save_to_file_round_trips_tunnels() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("save_test.toml");

        // Write initial config
        let initial = r#"
[logging]
stdout_level = "basic"

[capture]
max_stored_requests = 100
max_request_body_size = 1048576

[gui]
port = 8081

[[tunnels]]
name = "original"
domain = "original.example.com"
socket_path = "/tmp/original.sock"
target_port = 3000
enabled = true
"#;
        fs::write(&file_path, initial).unwrap();

        let mut config = Config::from_file(&file_path).unwrap();
        config.tunnels.push(TunnelConfig {
            name: "new".to_string(),
            domain: "new.example.com".to_string(),
            socket_path: "/tmp/new.sock".to_string(),
            target_port: 4000,
            enabled: false,
        });

        config.save_to_file(&file_path).unwrap();

        let reloaded = Config::from_file(&file_path).unwrap();
        assert_eq!(reloaded.tunnels.len(), 2);
        assert_eq!(reloaded.tunnels[0].name, "original");
        assert_eq!(reloaded.tunnels[1].name, "new");
        assert!(!reloaded.tunnels[1].enabled);
    }

    #[test]
    fn test_save_to_file_writes_cloudflare_tunnel_id() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("cf_save_test.toml");

        let initial = r#"
[logging]
stdout_level = "basic"

[capture]
max_stored_requests = 100
max_request_body_size = 1048576

[gui]
port = 8081

[cloudflare]
api_token = "tok"
account_id = "acc"
zone_id = "zone"
tunnel_name = "tunneldesk"

[[tunnels]]
name = "t"
domain = "t.example.com"
socket_path = "/tmp/t.sock"
target_port = 3000
enabled = true
"#;
        fs::write(&file_path, initial).unwrap();

        let mut config = Config::from_file(&file_path).unwrap();
        let cf = config.cloudflare.as_mut().unwrap();
        cf.tunnel_id = Some("new-tunnel-id".to_string());
        cf.tunnel_token = Some("new-token".to_string());

        config.save_to_file(&file_path).unwrap();

        let reloaded = Config::from_file(&file_path).unwrap();
        let cf = reloaded.cloudflare.unwrap();
        assert_eq!(cf.tunnel_id, Some("new-tunnel-id".to_string()));
        assert_eq!(cf.tunnel_token, Some("new-token".to_string()));
        // Existing fields preserved
        assert_eq!(cf.api_token, "tok");
        assert_eq!(cf.account_id, "acc");
    }
}
