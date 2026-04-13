use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use tracing::{info, warn};

use crate::cloudflare::{CloudflareClient, IngressRule, TunnelConfiguration};
use crate::config::{Config, TunnelConfig};

/// Summary of a sync operation.
#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    /// Domains added to Cloudflare (new ingress rules + DNS records created).
    pub added: Vec<String>,
    /// Domains removed from Cloudflare (ingress rules + DNS records deleted).
    pub removed: Vec<String>,
    /// Domains found on Cloudflare but absent from the local config.
    /// These are returned for user confirmation before removal.
    pub unknown_hosts: Vec<String>,
    /// Non-fatal errors encountered during sync.
    pub errors: Vec<String>,
}

/// Orchestrates synchronisation between `config.toml` and Cloudflare.
pub struct TunnelSync {
    client: CloudflareClient,
    tunnel_id: String,
}

impl TunnelSync {
    pub fn new(client: CloudflareClient, tunnel_id: impl Into<String>) -> Self {
        Self {
            client,
            tunnel_id: tunnel_id.into(),
        }
    }

    /// Full sync: brings Cloudflare ingress rules and DNS records into line
    /// with the enabled tunnels in `config`.
    ///
    /// Unknown hosts (present in Cloudflare but absent from local config) are
    /// reported in `SyncReport::unknown_hosts` but **not** automatically
    /// removed — call [`remove_hosts`](Self::remove_hosts) after user
    /// confirmation.
    pub async fn sync_to_cloudflare(&self, config: &Config) -> SyncReport {
        let mut report = SyncReport::default();

        // Fetch current Cloudflare ingress rules.
        let current_config = match self.client.get_tunnel_config(&self.tunnel_id).await {
            Ok(c) => c,
            Err(e) => {
                report.errors.push(format!("get_tunnel_config: {e}"));
                return report;
            }
        };

        let current_hosts: HashMap<String, ()> = current_config
            .ingress
            .iter()
            .filter_map(|r| r.hostname.clone())
            .map(|h| (h, ()))
            .collect();

        // Desired state: all enabled tunnels.
        let desired: Vec<&TunnelConfig> = config.tunnels.iter().filter(|t| t.enabled).collect();
        let desired_hosts: HashMap<&str, &TunnelConfig> =
            desired.iter().map(|t| (t.domain.as_str(), *t)).collect();

        // Identify unknown hosts: on Cloudflare but not in local config at all.
        let all_local_domains: std::collections::HashSet<&str> =
            config.tunnels.iter().map(|t| t.domain.as_str()).collect();
        for host in current_hosts.keys() {
            if !all_local_domains.contains(host.as_str()) {
                report.unknown_hosts.push(host.clone());
            }
        }

        // Add missing DNS records and ingress rules.
        for (host, tunnel) in &desired_hosts {
            if !current_hosts.contains_key(*host) {
                info!("Sync: adding DNS CNAME for {host}");
                if let Err(e) = self.client.create_dns_cname(host, &self.tunnel_id).await {
                    report.errors.push(format!("create_dns_cname({host}): {e}"));
                } else {
                    report.added.push(host.to_string());
                    let _ = tunnel; // suppress lint
                }
            }
        }

        // Build and push the new ingress config (desired + unknown + catch-all).
        // Unknown hosts are preserved so they are not silently removed before
        // the user confirms removal via the frontend.
        let ingress = build_sync_ingress(&desired, &current_config.ingress, &report.unknown_hosts);

        let new_config = TunnelConfiguration { ingress };
        if let Err(e) = self
            .client
            .put_tunnel_config(&self.tunnel_id, &new_config)
            .await
        {
            report.errors.push(format!("put_tunnel_config: {e}"));
            return report;
        }

        // Keep cache bypass rule in sync with the new ingress.
        if let Some(e) = self.apply_cache_rule(&new_config.ingress).await {
            report.errors.push(e);
        }

        report
    }

    /// Removes the given `hostnames` from Cloudflare ingress config and
    /// deletes their DNS records.  Call this after user confirmation.
    pub async fn remove_hosts(&self, hostnames: &[String]) -> anyhow::Result<Vec<String>> {
        if hostnames.is_empty() {
            return Ok(vec![]);
        }

        // Fetch current state.
        let current_config = self
            .client
            .get_tunnel_config(&self.tunnel_id)
            .await
            .context("get_tunnel_config")?;

        let dns_records = self
            .client
            .list_dns_cnames()
            .await
            .context("list_dns_cnames")?;

        // Build a map hostname -> DNS record ID.
        let dns_map: HashMap<&str, &str> = dns_records
            .iter()
            .map(|r| (r.name.as_str(), r.id.as_str()))
            .collect();

        let mut errors = vec![];
        let remove_set: std::collections::HashSet<&str> =
            hostnames.iter().map(String::as_str).collect();

        // Delete DNS records.
        for hostname in hostnames {
            if let Some(record_id) = dns_map.get(hostname.as_str())
                && let Err(e) = self.client.delete_dns_cname(record_id).await
            {
                errors.push(format!("delete_dns_cname({hostname}): {e}"));
            }
        }

        // Push ingress without the removed hosts.
        let mut ingress: Vec<IngressRule> = current_config
            .ingress
            .into_iter()
            .filter(|r| {
                r.hostname
                    .as_deref()
                    .map(|h| !remove_set.contains(h))
                    .unwrap_or(true) // keep catch-all
            })
            .collect();

        // Ensure catch-all is present.
        if !ingress.iter().any(|r| r.hostname.is_none()) {
            ingress.push(IngressRule::catch_all());
        }

        let new_config = TunnelConfiguration { ingress };
        self.client
            .put_tunnel_config(&self.tunnel_id, &new_config)
            .await
            .context("put_tunnel_config")?;

        if let Some(e) = self.apply_cache_rule(&new_config.ingress).await {
            errors.push(e);
        }

        if !errors.is_empty() {
            anyhow::bail!("partial failure removing hosts: {}", errors.join("; "));
        }

        Ok(hostnames.to_vec())
    }

    /// Adds a single tunnel's ingress rule and DNS CNAME.
    pub async fn add_single_tunnel(&self, tunnel: &TunnelConfig) -> anyhow::Result<()> {
        // Create DNS record.
        self.client
            .create_dns_cname(&tunnel.domain, &self.tunnel_id)
            .await
            .with_context(|| format!("create_dns_cname({})", tunnel.domain))?;

        // Fetch current config and append.
        let mut current = self
            .client
            .get_tunnel_config(&self.tunnel_id)
            .await
            .context("get_tunnel_config")?;

        // Remove old catch-all, append new rule, re-append catch-all.
        current.ingress.retain(|r| r.hostname.is_some());
        current.ingress.push(IngressRule::unix_socket(
            tunnel.domain.clone(),
            tunnel.socket_path.clone(),
        ));
        current.ingress.push(IngressRule::catch_all());

        self.client
            .put_tunnel_config(&self.tunnel_id, &current)
            .await
            .context("put_tunnel_config")?;

        self.apply_cache_rule(&current.ingress).await;
        Ok(())
    }

    /// Removes a single tunnel's ingress rule and DNS CNAME.
    pub async fn remove_single_tunnel(&self, domain: &str) -> anyhow::Result<()> {
        // Find and delete DNS record.
        let dns_records = self
            .client
            .list_dns_cnames()
            .await
            .context("list_dns_cnames")?;

        for record in &dns_records {
            if record.name == domain {
                self.client
                    .delete_dns_cname(&record.id)
                    .await
                    .with_context(|| format!("delete_dns_cname({})", record.id))?;
                break;
            }
        }

        // Remove from ingress config.
        let mut current = self
            .client
            .get_tunnel_config(&self.tunnel_id)
            .await
            .context("get_tunnel_config")?;

        current
            .ingress
            .retain(|r| r.hostname.as_deref() != Some(domain));

        // Ensure catch-all is present.
        if !current.ingress.iter().any(|r| r.hostname.is_none()) {
            current.ingress.push(IngressRule::catch_all());
        }

        self.client
            .put_tunnel_config(&self.tunnel_id, &current)
            .await
            .context("put_tunnel_config")?;

        self.apply_cache_rule(&current.ingress).await;
        Ok(())
    }

    /// Updates a single tunnel in Cloudflare (handles domain rename and
    /// service path changes).
    pub async fn update_single_tunnel(
        &self,
        old_domain: &str,
        tunnel: &TunnelConfig,
    ) -> anyhow::Result<()> {
        let domain_changed = old_domain != tunnel.domain;

        if domain_changed {
            // Remove old DNS record.
            let dns_records = self
                .client
                .list_dns_cnames()
                .await
                .context("list_dns_cnames")?;

            for record in &dns_records {
                if record.name == old_domain {
                    self.client
                        .delete_dns_cname(&record.id)
                        .await
                        .with_context(|| format!("delete_dns_cname old({})", record.id))?;
                    break;
                }
            }

            // Create new DNS record.
            self.client
                .create_dns_cname(&tunnel.domain, &self.tunnel_id)
                .await
                .with_context(|| format!("create_dns_cname({})", tunnel.domain))?;
        }

        // Fetch and update ingress config.
        let mut current = self
            .client
            .get_tunnel_config(&self.tunnel_id)
            .await
            .context("get_tunnel_config")?;

        // Replace the matching rule (by old hostname or socket service).
        let new_rule = IngressRule::unix_socket(tunnel.domain.clone(), tunnel.socket_path.clone());
        let mut replaced = false;
        for rule in current.ingress.iter_mut() {
            if rule.hostname.as_deref() == Some(old_domain) {
                *rule = new_rule.clone();
                replaced = true;
                break;
            }
        }
        if !replaced {
            // Rule didn't exist yet — append before catch-all.
            current.ingress.retain(|r| r.hostname.is_some());
            current.ingress.push(new_rule);
            current.ingress.push(IngressRule::catch_all());
        }

        self.client
            .put_tunnel_config(&self.tunnel_id, &current)
            .await
            .context("put_tunnel_config")?;

        self.apply_cache_rule(&current.ingress).await;
        Ok(())
    }

    /// Updates the Cloudflare cache bypass rule to match the active hostnames
    /// in `ingress`.  Non-fatal: failures are logged as warnings.
    ///
    /// Returns any error message so callers can surface it in a `SyncReport`.
    async fn apply_cache_rule(&self, ingress: &[IngressRule]) -> Option<String> {
        let hostnames: Vec<&str> = ingress
            .iter()
            .filter_map(|r| r.hostname.as_deref())
            .collect();
        if let Err(e) = self.client.upsert_bypass_cache_rule(&hostnames).await {
            let msg = format!("upsert_bypass_cache_rule: {e}");
            warn!("{msg}");
            return Some(msg);
        }
        None
    }

    /// Saves `tunnel_id` and `tunnel_token` into `config` and writes to disk.
    pub fn save_tunnel_credentials(
        config: &mut Config,
        path: &Path,
        tunnel_id: String,
        tunnel_token: String,
    ) -> anyhow::Result<()> {
        if let Some(cf) = config.cloudflare.as_mut() {
            cf.tunnel_id = Some(tunnel_id);
            cf.tunnel_token = Some(tunnel_token);
        }
        config
            .save_to_file(path)
            .context("save config after tunnel creation")
    }
}

/// Builds the ingress rule list to push to Cloudflare during a sync.
///
/// The result contains:
/// - one rule per enabled desired tunnel (from local config)
/// - the existing rules for unknown hosts (present in Cloudflare but absent
///   from local config), so they are not silently deleted before user confirmation
/// - a catch-all rule at the end
fn build_sync_ingress(
    desired: &[&TunnelConfig],
    current_ingress: &[IngressRule],
    unknown_hosts: &[String],
) -> Vec<IngressRule> {
    let unknown_set: std::collections::HashSet<&str> =
        unknown_hosts.iter().map(String::as_str).collect();
    let mut ingress: Vec<IngressRule> = desired
        .iter()
        .map(|t| IngressRule::unix_socket(t.domain.clone(), t.socket_path.clone()))
        .collect();
    ingress.extend(
        current_ingress
            .iter()
            .filter(|r| {
                r.hostname
                    .as_deref()
                    .map(|h| unknown_set.contains(h))
                    .unwrap_or(false)
            })
            .cloned(),
    );
    ingress.push(IngressRule::catch_all());
    ingress
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::IngressRule;

    #[test]
    fn test_sync_report_default() {
        let report = SyncReport::default();
        assert!(report.added.is_empty());
        assert!(report.removed.is_empty());
        assert!(report.unknown_hosts.is_empty());
        assert!(report.errors.is_empty());
    }

    #[test]
    fn test_ingress_rule_catch_all_service() {
        let rule = IngressRule::catch_all();
        assert_eq!(rule.service, "http_status:404");
        assert!(rule.hostname.is_none());
    }

    #[test]
    fn test_ingress_rule_unix_socket_service() {
        let rule = IngressRule::unix_socket("app.example.com", "/tmp/app.sock");
        assert_eq!(rule.service, "unix:/tmp/app.sock");
        assert_eq!(rule.hostname.as_deref(), Some("app.example.com"));
    }

    #[test]
    fn test_save_tunnel_credentials_updates_config() {
        use std::io::Write;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let content = r#"
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
[[tunnels]]
name = "t"
domain = "t.example.com"
socket_path = "/tmp/t.sock"
target_port = 3000
enabled = true
"#;
        std::fs::File::create(&path)
            .unwrap()
            .write_all(content.as_bytes())
            .unwrap();

        let mut config = crate::config::Config::from_file(&path).unwrap();
        TunnelSync::save_tunnel_credentials(
            &mut config,
            &path,
            "tid-xyz".to_string(),
            "tok-abc".to_string(),
        )
        .unwrap();

        let reloaded = crate::config::Config::from_file(&path).unwrap();
        let cf = reloaded.cloudflare.unwrap();
        assert_eq!(cf.tunnel_id, Some("tid-xyz".to_string()));
        assert_eq!(cf.tunnel_token, Some("tok-abc".to_string()));
    }

    fn make_tunnel(domain: &str, socket: &str) -> TunnelConfig {
        TunnelConfig {
            name: domain.to_string(),
            domain: domain.to_string(),
            socket_path: socket.to_string(),
            target_port: 3000,
            enabled: true,
        }
    }

    /// Unknown hosts must be preserved in the pushed ingress so they are not
    /// silently removed before the user confirms deletion in the frontend.
    #[test]
    fn test_build_sync_ingress_preserves_unknown_hosts() {
        let desired_tunnel = make_tunnel("keep.example.com", "/tmp/keep.sock");
        let desired = vec![&desired_tunnel];

        let current_ingress = vec![
            IngressRule::unix_socket("keep.example.com", "/tmp/keep.sock"),
            IngressRule::unix_socket("ghost.example.com", "/tmp/ghost.sock"),
            IngressRule::catch_all(),
        ];

        let unknown_hosts = vec!["ghost.example.com".to_string()];

        let ingress = build_sync_ingress(&desired, &current_ingress, &unknown_hosts);

        let hostnames: Vec<Option<&str>> = ingress.iter().map(|r| r.hostname.as_deref()).collect();

        assert!(
            hostnames.contains(&Some("keep.example.com")),
            "desired host must be present"
        );
        assert!(
            hostnames.contains(&Some("ghost.example.com")),
            "unknown host must be preserved, not silently removed"
        );
        assert!(hostnames.contains(&None), "catch-all must be present");
    }

    /// Disabled tunnels must not appear in the pushed ingress.
    #[test]
    fn test_build_sync_ingress_excludes_disabled_tunnels() {
        // Caller already filters to enabled tunnels before calling build_sync_ingress,
        // so `desired` only contains enabled ones — verify that is respected.
        let enabled = make_tunnel("active.example.com", "/tmp/active.sock");
        let desired = vec![&enabled]; // disabled tunnel intentionally omitted

        let current_ingress = vec![
            IngressRule::unix_socket("active.example.com", "/tmp/active.sock"),
            IngressRule::catch_all(),
        ];

        let ingress = build_sync_ingress(&desired, &current_ingress, &[]);

        let hostnames: Vec<Option<&str>> = ingress.iter().map(|r| r.hostname.as_deref()).collect();

        assert_eq!(
            hostnames,
            vec![Some("active.example.com"), None],
            "only the enabled tunnel and catch-all should be present"
        );
    }

    /// When there are no unknown hosts, the ingress is exactly the desired
    /// tunnels plus the catch-all.
    #[test]
    fn test_build_sync_ingress_no_unknowns() {
        let t1 = make_tunnel("a.example.com", "/tmp/a.sock");
        let t2 = make_tunnel("b.example.com", "/tmp/b.sock");
        let desired = vec![&t1, &t2];

        let current_ingress = vec![
            IngressRule::unix_socket("a.example.com", "/tmp/a.sock"),
            IngressRule::catch_all(),
        ];

        let ingress = build_sync_ingress(&desired, &current_ingress, &[]);

        assert_eq!(ingress.len(), 3, "two desired + catch-all");
        assert!(
            ingress.last().unwrap().hostname.is_none(),
            "last rule is catch-all"
        );
    }

    /// The catch-all must always be the final rule.
    #[test]
    fn test_build_sync_ingress_catch_all_is_last() {
        let t = make_tunnel("x.example.com", "/tmp/x.sock");
        let desired = vec![&t];

        let current_ingress = vec![
            IngressRule::unix_socket("x.example.com", "/tmp/x.sock"),
            IngressRule::unix_socket("orphan.example.com", "/tmp/orphan.sock"),
            IngressRule::catch_all(),
        ];

        let unknown_hosts = vec!["orphan.example.com".to_string()];
        let ingress = build_sync_ingress(&desired, &current_ingress, &unknown_hosts);

        assert!(
            ingress.last().unwrap().hostname.is_none(),
            "catch-all must be the last rule"
        );
    }
}
