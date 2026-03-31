use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

const CF_API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Client for the Cloudflare v4 API.
#[derive(Clone)]
pub struct CloudflareClient {
    http: reqwest::Client,
    api_token: String,
    pub account_id: String,
    pub zone_id: String,
}

/// A single ingress rule in a Cloudflare tunnel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngressRule {
    /// Public hostname for this rule. `None` for the catch-all rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Backend service (e.g. `"unix:/tmp/tunnel.sock"` or `"http_status:404"`).
    pub service: String,
    /// Optional per-rule origin request settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_request: Option<serde_json::Value>,
}

impl IngressRule {
    /// Creates a catch-all rule that returns HTTP 404.
    pub fn catch_all() -> Self {
        IngressRule {
            hostname: None,
            service: "http_status:404".to_string(),
            origin_request: None,
        }
    }

    /// Creates a rule forwarding `hostname` to a Unix socket at `socket_path`.
    pub fn unix_socket(hostname: impl Into<String>, socket_path: impl Into<String>) -> Self {
        IngressRule {
            hostname: Some(hostname.into()),
            service: format!("unix:{}", socket_path.into()),
            origin_request: None,
        }
    }
}

/// The ingress configuration of a Cloudflare tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfiguration {
    pub ingress: Vec<IngressRule>,
}

/// A single rule in a Cloudflare Ruleset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRulesetRuleActionParameters {
    /// Whether the request's response from the origin is eligible for caching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<bool>,
}

/// A single rule in a Cloudflare Ruleset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRulesetRule {
    /// Cloudflare-assigned rule ID. Omitted when creating a new rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Rule action (e.g. `"bypass_cache"`).
    pub action: String,
    /// Action parameters (e.g. `{"cache": false}` for set_cache_settings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_parameters: Option<CacheRulesetRuleActionParameters>,
    /// Wirefilter expression (e.g. `http.host eq "example.com"`).
    pub expression: String,
    /// Human-readable description used to identify the rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether this rule is active.
    pub enabled: bool,
}

/// A Cloudflare DNS record.
#[derive(Debug, Clone, Deserialize)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub record_type: String,
    #[allow(dead_code)]
    pub content: String,
}

/// Standard Cloudflare API response envelope.
#[derive(Debug, Deserialize)]
struct CfResponse<T> {
    success: bool,
    errors: Vec<CfError>,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct CfError {
    message: String,
}

/// Response body from the create-tunnel endpoint.
#[derive(Debug, Deserialize)]
struct CreateTunnelResult {
    id: String,
}

/// Response body from the get-token endpoint (plain string token).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TokenResult {
    Token(String),
}

/// Response from the get-tunnel-config endpoint.
#[derive(Debug, Deserialize)]
struct TunnelConfigResult {
    config: TunnelConfiguration,
}

/// Response from the get-cache-ruleset endpoint.
#[derive(Debug, Deserialize)]
struct RulesetResult {
    rules: Option<Vec<CacheRulesetRule>>,
}

impl CloudflareClient {
    /// Creates a new client. Builds a `reqwest::Client` with rustls TLS.
    pub fn new(
        api_token: impl Into<String>,
        account_id: impl Into<String>,
        zone_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            api_token: api_token.into(),
            account_id: account_id.into(),
            zone_id: zone_id.into(),
        })
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_token)
    }

    /// Creates a new named Cloudflare tunnel.
    ///
    /// Returns the tunnel ID.
    pub async fn create_tunnel(&self, name: &str, tunnel_secret: &str) -> anyhow::Result<String> {
        let url = format!("{}/accounts/{}/cfd_tunnel", CF_API_BASE, self.account_id);
        let body = serde_json::json!({
            "name": name,
            "tunnel_secret": tunnel_secret,
        });

        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .context("POST cfd_tunnel")?;

        let cf: CfResponse<CreateTunnelResult> =
            resp.json().await.context("parse create_tunnel response")?;

        if !cf.success {
            return Err(anyhow!(
                "create_tunnel failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        cf.result
            .map(|r| r.id)
            .ok_or_else(|| anyhow!("create_tunnel: missing result"))
    }

    /// Fetches the connector token for an existing tunnel.
    pub async fn get_tunnel_token(&self, tunnel_id: &str) -> anyhow::Result<String> {
        let url = format!(
            "{}/accounts/{}/cfd_tunnel/{}/token",
            CF_API_BASE, self.account_id, tunnel_id
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("GET cfd_tunnel token")?;

        let cf: CfResponse<TokenResult> = resp
            .json()
            .await
            .context("parse get_tunnel_token response")?;

        if !cf.success {
            return Err(anyhow!(
                "get_tunnel_token failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        match cf.result {
            Some(TokenResult::Token(t)) => Ok(t),
            None => Err(anyhow!("get_tunnel_token: missing result")),
        }
    }

    /// Returns the current ingress configuration of a tunnel.
    pub async fn get_tunnel_config(&self, tunnel_id: &str) -> anyhow::Result<TunnelConfiguration> {
        let url = format!(
            "{}/accounts/{}/cfd_tunnel/{}/configurations",
            CF_API_BASE, self.account_id, tunnel_id
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("GET cfd_tunnel configurations")?;

        let cf: CfResponse<TunnelConfigResult> = resp
            .json()
            .await
            .context("parse get_tunnel_config response")?;

        if !cf.success {
            return Err(anyhow!(
                "get_tunnel_config failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        cf.result
            .map(|r| r.config)
            .ok_or_else(|| anyhow!("get_tunnel_config: missing result"))
    }

    /// Replaces the ingress configuration of a tunnel.
    pub async fn put_tunnel_config(
        &self,
        tunnel_id: &str,
        config: &TunnelConfiguration,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/accounts/{}/cfd_tunnel/{}/configurations",
            CF_API_BASE, self.account_id, tunnel_id
        );

        let resp = self
            .http
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "config": config }))
            .send()
            .await
            .context("PUT cfd_tunnel configurations")?;

        let cf: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .context("parse put_tunnel_config response")?;

        if !cf.success {
            return Err(anyhow!(
                "put_tunnel_config failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok(())
    }

    /// Lists all CNAME DNS records in the configured zone.
    pub async fn list_dns_cnames(&self) -> anyhow::Result<Vec<DnsRecord>> {
        let url = format!(
            "{}/zones/{}/dns_records?type=CNAME&per_page=500",
            CF_API_BASE, self.zone_id
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("GET dns_records")?;

        let cf: CfResponse<Vec<DnsRecord>> = resp
            .json()
            .await
            .context("parse list_dns_cnames response")?;

        if !cf.success {
            return Err(anyhow!(
                "list_dns_cnames failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        Ok(cf.result.unwrap_or_default())
    }

    /// Creates a proxied CNAME record pointing `hostname` at the tunnel.
    ///
    /// The CNAME content is `{tunnel_id}.cfargotunnel.com`.
    pub async fn create_dns_cname(&self, hostname: &str, tunnel_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/zones/{}/dns_records", CF_API_BASE, self.zone_id);
        let body = serde_json::json!({
            "type": "CNAME",
            "name": hostname,
            "content": format!("{}.cfargotunnel.com", tunnel_id),
            "proxied": true,
        });

        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .context("POST dns_records")?;

        let cf: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .context("parse create_dns_cname response")?;

        if !cf.success {
            return Err(anyhow!(
                "create_dns_cname failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok(())
    }

    /// Returns all rules in the zone's cache-settings ruleset phase.
    ///
    /// Returns an empty `Vec` when the phase entrypoint does not exist yet
    /// (Cloudflare returns 404 for zones that have never had cache rules).
    pub async fn get_cache_ruleset_rules(&self) -> anyhow::Result<Vec<CacheRulesetRule>> {
        let url = format!(
            "{}/zones/{}/rulesets/phases/http_request_cache_settings/entrypoint",
            CF_API_BASE, self.zone_id
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("GET cache ruleset")?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }

        let cf: CfResponse<RulesetResult> = resp
            .json()
            .await
            .context("parse get_cache_ruleset response")?;

        if !cf.success {
            return Err(anyhow!(
                "get_cache_ruleset_rules failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        Ok(cf.result.and_then(|r| r.rules).unwrap_or_default())
    }

    /// Replaces all rules in the zone's cache-settings ruleset phase.
    pub async fn put_cache_ruleset_rules(&self, rules: &[CacheRulesetRule]) -> anyhow::Result<()> {
        let url = format!(
            "{}/zones/{}/rulesets/phases/http_request_cache_settings/entrypoint",
            CF_API_BASE, self.zone_id
        );

        let resp = self
            .http
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "rules": rules }))
            .send()
            .await
            .context("PUT cache ruleset")?;

        let cf: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .context("parse put_cache_ruleset response")?;

        if !cf.success {
            return Err(anyhow!(
                "put_cache_ruleset_rules failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok(())
    }

    /// Upserts a single cache-bypass rule covering all `hostnames`.
    ///
    /// Any existing rule with the TunnelDesk description is replaced.
    /// Other cache rules in the zone are preserved unchanged.
    /// If `hostnames` is empty the TunnelDesk rule is removed entirely.
    pub async fn upsert_bypass_cache_rule(&self, hostnames: &[&str]) -> anyhow::Result<()> {
        const DESCRIPTION: &str = "TunnelDesk: bypass cache for tunneled hostnames";

        let mut rules = self.get_cache_ruleset_rules().await?;
        // Remove the previous TunnelDesk rule (if any).
        rules.retain(|r| r.description.as_deref() != Some(DESCRIPTION));

        if !hostnames.is_empty() {
            rules.push(CacheRulesetRule {
                id: None,
                action: "set_cache_settings".to_string(),
                action_parameters: Some(CacheRulesetRuleActionParameters { cache: Some(false) }),
                expression: build_hostname_expression(hostnames),
                description: Some(DESCRIPTION.to_string()),
                enabled: true,
            });
        }

        self.put_cache_ruleset_rules(&rules).await
    }

    /// Deletes a DNS record by ID.
    pub async fn delete_dns_cname(&self, record_id: &str) -> anyhow::Result<()> {
        let url = format!(
            "{}/zones/{}/dns_records/{}",
            CF_API_BASE, self.zone_id, record_id
        );

        let resp = self
            .http
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("DELETE dns_records")?;

        let cf: CfResponse<serde_json::Value> = resp
            .json()
            .await
            .context("parse delete_dns_cname response")?;

        if !cf.success {
            return Err(anyhow!(
                "delete_dns_cname failed: {}",
                cf.errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok(())
    }
}

/// Builds a Wirefilter expression that matches any of the given hostnames.
///
/// - Single host  → `http.host eq "example.com"`
/// - Multiple     → `http.host in {"a.com" "b.com"}`
fn build_hostname_expression(hostnames: &[&str]) -> String {
    debug_assert!(!hostnames.is_empty());
    if hostnames.len() == 1 {
        format!(r#"http.host eq "{}""#, hostnames[0])
    } else {
        let inner = hostnames
            .iter()
            .map(|h| format!(r#""{h}""#))
            .collect::<Vec<_>>()
            .join(" ");
        format!("http.host in {{{inner}}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ingress_rule_catch_all() {
        let rule = IngressRule::catch_all();
        assert!(rule.hostname.is_none());
        assert_eq!(rule.service, "http_status:404");
    }

    #[test]
    fn test_ingress_rule_unix_socket() {
        let rule = IngressRule::unix_socket("app.example.com", "/tmp/app.sock");
        assert_eq!(rule.hostname, Some("app.example.com".to_string()));
        assert_eq!(rule.service, "unix:/tmp/app.sock");
    }

    #[test]
    fn test_ingress_rule_serialization_skips_null_hostname() {
        let rule = IngressRule::catch_all();
        let json = serde_json::to_value(&rule).unwrap();
        assert!(json.get("hostname").is_none());
        assert_eq!(json["service"], "http_status:404");
    }

    #[test]
    fn test_ingress_rule_serialization_includes_hostname() {
        let rule = IngressRule::unix_socket("app.example.com", "/tmp/app.sock");
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["hostname"], "app.example.com");
        assert_eq!(json["service"], "unix:/tmp/app.sock");
    }

    #[test]
    fn test_tunnel_configuration_serialization() {
        let config = TunnelConfiguration {
            ingress: vec![
                IngressRule::unix_socket("a.example.com", "/tmp/a.sock"),
                IngressRule::catch_all(),
            ],
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["ingress"].as_array().unwrap().len(), 2);
        assert_eq!(json["ingress"][0]["hostname"], "a.example.com");
        assert!(json["ingress"][1].get("hostname").is_none());
    }

    #[test]
    fn test_cf_response_deserialization_success() {
        let raw = r#"{"success":true,"errors":[],"result":{"id":"abc123"}}"#;
        let cf: CfResponse<CreateTunnelResult> = serde_json::from_str(raw).unwrap();
        assert!(cf.success);
        assert_eq!(cf.result.unwrap().id, "abc123");
    }

    #[test]
    fn test_cf_response_deserialization_error() {
        let raw = r#"{"success":false,"errors":[{"message":"Invalid API token"}],"result":null}"#;
        let cf: CfResponse<CreateTunnelResult> = serde_json::from_str(raw).unwrap();
        assert!(!cf.success);
        assert_eq!(cf.errors[0].message, "Invalid API token");
        assert!(cf.result.is_none());
    }

    #[test]
    fn test_dns_record_deserialization() {
        let raw = r#"{"id":"rec1","name":"app.example.com","type":"CNAME","content":"abc.cfargotunnel.com"}"#;
        let rec: DnsRecord = serde_json::from_str(raw).unwrap();
        assert_eq!(rec.id, "rec1");
        assert_eq!(rec.name, "app.example.com");
        assert_eq!(rec.record_type, "CNAME");
    }

    #[test]
    fn test_client_new_succeeds() {
        let client = CloudflareClient::new("token", "account", "zone");
        assert!(client.is_ok());
    }

    #[test]
    fn test_build_hostname_expression_single() {
        let expr = build_hostname_expression(&["app.example.com"]);
        assert_eq!(expr, r#"http.host eq "app.example.com""#);
    }

    #[test]
    fn test_build_hostname_expression_multiple() {
        let expr = build_hostname_expression(&["a.example.com", "b.example.com"]);
        assert_eq!(expr, r#"http.host in {"a.example.com" "b.example.com"}"#);
    }

    #[test]
    fn test_cache_ruleset_rule_serialization_includes_id() {
        let rule = CacheRulesetRule {
            id: Some("rule-id".to_string()),
            action: "set_cache_settings".to_string(),
            action_parameters: Some(CacheRulesetRuleActionParameters { cache: Some(false) }),
            expression: r#"http.host eq "app.example.com""#.to_string(),
            description: Some("desc".to_string()),
            enabled: true,
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["id"], "rule-id");
        assert_eq!(json["action"], "set_cache_settings");
        assert_eq!(json["enabled"], true);
    }

    #[test]
    fn test_cache_ruleset_rule_serialization_omits_null_id() {
        let rule = CacheRulesetRule {
            id: None,
            action: "set_cache_settings".to_string(),
            action_parameters: Some(CacheRulesetRuleActionParameters { cache: Some(false) }),
            expression: r#"http.host eq "app.example.com""#.to_string(),
            description: None,
            enabled: true,
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert!(json.get("id").is_none(), "id must be omitted when None");
        assert!(
            json.get("description").is_none(),
            "description must be omitted when None"
        );
    }

    #[test]
    fn test_cache_ruleset_rule_deserialization() {
        let raw = r#"{"id":"r1","action":"bypass_cache","expression":"http.host eq \"a.com\"","description":"TunnelDesk: bypass cache for tunneled hostnames","enabled":true}"#;
        let rule: CacheRulesetRule = serde_json::from_str(raw).unwrap();
        assert_eq!(rule.id, Some("r1".to_string()));
        assert_eq!(rule.action, "bypass_cache");
        assert!(rule.enabled);
    }
}
