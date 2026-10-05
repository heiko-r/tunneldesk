use base64::Engine as _;
use rand::{Rng, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use std::env;
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock, broadcast, watch};

use crate::cloudflared::ConnectorState;
use crate::config::{Config, TunnelConfig};
use crate::storage::{RequestStorage, WebSocketMessageStorage};
use crate::sync::TunnelSync;
use crate::tunnel::TunnelManager;

/// A [`RequestExchange`](crate::storage::RequestExchange) with binary fields
/// base64-encoded for safe JSON transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestExchangeWithBase64 {
    pub request: StoredRequestWithBase64,
    pub response: Option<StoredResponseWithBase64>,
}

/// A [`StoredRequest`](crate::storage::StoredRequest) with `body` and
/// `raw_request` base64-encoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRequestWithBase64 {
    pub id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tunnel_name: String,
    pub method: String,
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    /// Base64-encoded body bytes.
    pub body: String,
    /// Base64-encoded raw request bytes.
    pub raw_request: String,
    /// `true` when this request was created by the replay feature.
    pub replayed: bool,
}

/// A [`StoredResponse`](crate::storage::StoredResponse) with `body` and
/// `raw_response` base64-encoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponseWithBase64 {
    pub request_id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    /// Base64-encoded body bytes.
    pub body: String,
    /// Base64-encoded raw response bytes.
    pub raw_response: String,
    pub response_time_ms: Option<f64>,
}

/// A [`StoredWebSocketMessage`](crate::storage::StoredWebSocketMessage) with
/// `payload` base64-encoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredWebSocketMessageWithBase64 {
    pub id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tunnel_name: String,
    pub upgrade_request_id: String,
    /// Traffic direction: `"→"` for client→server, `"←"` for server→client.
    pub direction: String,
    pub message_type: crate::storage::WebSocketMessageType,
    /// Base64-encoded payload bytes.
    pub payload: String,
}

/// Payload for a replay request sent by the browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayRequestPayload {
    pub tunnel_name: String,
    pub method: String,
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    /// Base64-encoded request body.
    pub body: String,
}

/// Payload for a replay response.
///
/// On success `id` is the ID of the stored replayed exchange; on error `error` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResponsePayload {
    /// ID of the stored replayed exchange (`None` when the request failed before storage).
    pub id: Option<String>,
    /// Error message when `id` is `None`.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTunnelRequest {
    pub name: String,
    pub domain: String,
    pub socket_path: Option<String>,
    pub target_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateTunnelRequest {
    pub name: String,
    pub domain: Option<String>,
    pub socket_path: Option<String>,
    pub target_port: Option<u16>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteTunnelRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmRemoveHostsRequest {
    pub hosts: Vec<String>,
}

/// Metadata about a configured tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub name: String,
    pub domain: String,
    pub socket_path: String,
    /// Local TCP port the tunnel forwards to.
    pub destination: u16,
    /// Whether this tunnel is enabled in Cloudflare.
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelDeletedResponse {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReportResponse {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub unknown_hosts: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownHostsFoundResponse {
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareStatusResponse {
    pub configured: bool,
    pub tunnel_id: Option<String>,
    pub tunnel_name: Option<String>,
    pub connector: ConnectorState,
}

/// Change notifications broadcast to every connected client, regardless of
/// which frontend (GUI, browser, MCP) triggered the change.
#[derive(Debug, Clone)]
pub enum TunnelEvent {
    Created(TunnelInfo),
    Updated(TunnelInfo),
    Deleted(TunnelDeletedResponse),
}

#[derive(Clone)]
pub struct AppService {
    pub config: Arc<RwLock<Config>>,
    pub tunnel_manager: Arc<TunnelManager>,
    /// Set once the background Cloudflare setup has finished.
    tunnel_sync: Arc<OnceCell<Arc<TunnelSync>>>,
    pub request_storage: Arc<RequestStorage>,
    pub websocket_storage: Arc<WebSocketMessageStorage>,
    connector: watch::Sender<ConnectorState>,
    events: broadcast::Sender<TunnelEvent>,
}

impl AppService {
    pub fn new(
        config: Arc<RwLock<Config>>,
        tunnel_manager: Arc<TunnelManager>,
        request_storage: Arc<RequestStorage>,
        websocket_storage: Arc<WebSocketMessageStorage>,
    ) -> Self {
        let (connector, _) = watch::channel(ConnectorState::Stopped);
        let (events, _) = broadcast::channel(64);
        Self {
            config,
            tunnel_manager,
            tunnel_sync: Arc::new(OnceCell::new()),
            request_storage,
            websocket_storage,
            connector,
            events,
        }
    }

    /// Returns the Cloudflare sync handle once Cloudflare setup has completed.
    pub fn tunnel_sync(&self) -> Option<Arc<TunnelSync>> {
        self.tunnel_sync.get().cloned()
    }

    /// Installs the Cloudflare sync handle. Returns `false` if one was already set.
    pub fn set_tunnel_sync(&self, sync: Arc<TunnelSync>) -> bool {
        self.tunnel_sync.set(sync).is_ok()
    }

    /// Sender through which the `cloudflared` supervisor publishes its state.
    pub fn connector_sender(&self) -> watch::Sender<ConnectorState> {
        self.connector.clone()
    }

    pub fn subscribe_connector(&self) -> watch::Receiver<ConnectorState> {
        self.connector.subscribe()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<TunnelEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event: TunnelEvent) {
        // No receivers is fine: nobody is connected.
        let _ = self.events.send(event);
    }

    pub async fn create_tunnel(&self, req: CreateTunnelRequest) -> Result<TunnelInfo, String> {
        // Validate uniqueness.
        {
            let cfg = self.config.read().await;
            if cfg.tunnels.iter().any(|t| t.name == req.name) {
                return Err(format!("A tunnel named '{}' already exists", req.name));
            }
        }

        let socket_path = req
            .socket_path
            .unwrap_or_else(|| get_default_socket_path(&req.name));

        let new_tunnel = TunnelConfig {
            name: req.name.clone(),
            domain: req.domain,
            socket_path,
            target_port: req.target_port,
            enabled: true,
        };

        let info = tunnel_info_from_config(&new_tunnel);

        // Persist to config.
        let config_path = {
            let mut cfg = self.config.write().await;
            cfg.tunnels.push(new_tunnel.clone());
            cfg.config_path.clone()
        };

        if let Some(path) = &config_path {
            let cfg = self.config.read().await;
            if let Err(e) = cfg.save_to_file(path) {
                return Err(format!("Failed to save config: {e}"));
            }
        }

        // Cloudflare: add ingress rule + DNS.
        if new_tunnel.enabled
            && let Some(sync) = self.tunnel_sync()
            && let Err(e) = sync.add_single_tunnel(&new_tunnel).await
        {
            tracing::warn!("Cloudflare add_single_tunnel failed: {e}");
        }

        // Start local proxy.
        self.tunnel_manager.start_tunnel(new_tunnel).await;

        self.emit(TunnelEvent::Created(info.clone()));
        Ok(info)
    }

    pub async fn update_tunnel(&self, req: UpdateTunnelRequest) -> Result<TunnelInfo, String> {
        let old_tunnel = {
            let cfg = self.config.read().await;
            match cfg.tunnels.iter().find(|t| t.name == req.name) {
                Some(t) => t.clone(),
                None => {
                    return Err(format!("Tunnel '{}' not found", req.name));
                }
            }
        };

        let old_domain = old_tunnel.domain.clone();
        let old_enabled = old_tunnel.enabled;

        let updated = TunnelConfig {
            name: old_tunnel.name.clone(),
            domain: req.domain.unwrap_or(old_tunnel.domain),
            socket_path: req.socket_path.unwrap_or(old_tunnel.socket_path),
            target_port: req.target_port.unwrap_or(old_tunnel.target_port),
            enabled: req.enabled.unwrap_or(old_tunnel.enabled),
        };

        let info = tunnel_info_from_config(&updated);

        // Persist.
        let config_path = {
            let mut cfg = self.config.write().await;
            if let Some(t) = cfg.tunnels.iter_mut().find(|t| t.name == req.name) {
                *t = updated.clone();
            }
            cfg.config_path.clone()
        };

        if let Some(path) = &config_path {
            let cfg = self.config.read().await;
            if let Err(e) = cfg.save_to_file(path) {
                return Err(format!("Failed to save config: {e}"));
            }
        }

        // Cloudflare sync.
        if let Some(sync) = self.tunnel_sync() {
            let enabled_changed = updated.enabled != old_enabled;
            let domain_changed = updated.domain != old_domain;

            if enabled_changed && !updated.enabled {
                // Disabled: remove from Cloudflare.
                if let Err(e) = sync.remove_single_tunnel(&old_domain).await {
                    tracing::warn!("Cloudflare remove_single_tunnel failed: {e}");
                }
            } else if enabled_changed && updated.enabled {
                // Re-enabled: add to Cloudflare.
                if let Err(e) = sync.add_single_tunnel(&updated).await {
                    tracing::warn!("Cloudflare add_single_tunnel failed: {e}");
                }
            } else if updated.enabled && domain_changed {
                // Domain changed while enabled: update ingress + DNS.
                if let Err(e) = sync.update_single_tunnel(&old_domain, &updated).await {
                    tracing::warn!("Cloudflare update_single_tunnel failed: {e}");
                }
            }
        }

        if old_tunnel.enabled && !updated.enabled {
            self.tunnel_manager.stop_tunnel(&req.name).await;
        } else {
            self.tunnel_manager.restart_tunnel(&req.name, updated).await;
        }

        self.emit(TunnelEvent::Updated(info.clone()));
        Ok(info)
    }

    pub async fn delete_tunnel(
        &self,
        req: DeleteTunnelRequest,
    ) -> Result<TunnelDeletedResponse, String> {
        let tunnel = {
            let cfg = self.config.read().await;
            match cfg.tunnels.iter().find(|t| t.name == req.name) {
                Some(t) => t.clone(),
                None => {
                    return Err(format!("Tunnel '{}' not found", req.name));
                }
            }
        };

        // Persist removal.
        let config_path = {
            let mut cfg = self.config.write().await;
            cfg.tunnels.retain(|t| t.name != req.name);
            cfg.config_path.clone()
        };

        if let Some(path) = &config_path {
            let cfg = self.config.read().await;
            if let Err(e) = cfg.save_to_file(path) {
                return Err(format!("Failed to save config: {e}"));
            }
        }

        // Cloudflare: remove ingress + DNS.
        if tunnel.enabled
            && let Some(sync) = self.tunnel_sync()
            && let Err(e) = sync.remove_single_tunnel(&tunnel.domain).await
        {
            tracing::warn!("Cloudflare remove_single_tunnel failed: {e}");
        }

        // Stop local proxy.
        self.tunnel_manager.stop_tunnel(&req.name).await;

        let resp = TunnelDeletedResponse { name: req.name };
        self.emit(TunnelEvent::Deleted(resp.clone()));
        Ok(resp)
    }

    pub async fn sync_tunnels(&self) -> Result<SyncReportResponse, String> {
        let Some(sync) = self.tunnel_sync() else {
            return Err("Cloudflare integration is not configured".to_string());
        };

        let cfg = self.config.read().await;
        let report = sync.sync_to_cloudflare(&cfg).await;
        drop(cfg);

        let resp = SyncReportResponse {
            added: report.added,
            removed: report.removed,
            unknown_hosts: report.unknown_hosts,
            errors: report.errors,
        };

        Ok(resp)
    }

    pub async fn confirm_remove_hosts(
        &self,
        req: ConfirmRemoveHostsRequest,
    ) -> Result<SyncReportResponse, String> {
        let Some(sync) = self.tunnel_sync() else {
            return Err("Cloudflare integration is not configured".to_string());
        };

        match sync.remove_hosts(&req.hosts).await {
            Ok(removed) => Ok(SyncReportResponse {
                added: vec![],
                removed,
                unknown_hosts: vec![],
                errors: vec![],
            }),
            Err(e) => Err(format!("Failed to remove hosts: {e}")),
        }
    }

    pub async fn replay_request(
        &self,
        req: ReplayRequestPayload,
    ) -> Result<ReplayResponsePayload, String> {
        macro_rules! err {
            ($msg:expr) => {
                return Ok(ReplayResponsePayload {
                    id: None,
                    error: Some($msg),
                })
            };
        }

        let (target_port, max_body_size) = {
            let cfg = self.config.read().await;
            match cfg.tunnels.iter().find(|t| t.name == req.tunnel_name) {
                Some(t) => (t.target_port, cfg.capture.max_request_body_size),
                None => err!(format!("Tunnel '{}' not found", req.tunnel_name)),
            }
        };

        let body_bytes = match base64::engine::general_purpose::STANDARD.decode(&req.body) {
            Ok(b) => b,
            Err(e) => err!(format!("Invalid base64 body: {e}")),
        };

        let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
            Ok(m) => m,
            Err(_) => err!(format!("Invalid HTTP method: {}", req.method)),
        };

        let full_url = format!("http://127.0.0.1:{}{}", target_port, req.url);

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut request_builder = client.request(method, &full_url);

        for (key, value) in &req.headers {
            let lower = key.to_lowercase();
            // Skip headers that reqwest or the HTTP layer manages automatically.
            if lower == "host" || lower == "content-length" || lower == "transfer-encoding" {
                continue;
            }
            if let (Ok(k), Ok(v)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                request_builder = request_builder.header(k, v);
            }
        }

        if !body_bytes.is_empty() {
            request_builder = request_builder.body(body_bytes.clone());
        }

        let start = std::time::Instant::now();

        let response = match request_builder.send().await {
            Ok(r) => r,
            Err(e) => err!(format!("Request failed: {e}")),
        };

        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        let status = response.status().as_u16();
        let mut resp_headers = std::collections::HashMap::new();
        for (k, v) in response.headers() {
            if let Ok(v_str) = v.to_str() {
                resp_headers.insert(k.to_string(), v_str.to_string());
            }
        }

        let resp_body = match response.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => err!(format!("Failed to read response body: {e}")),
        };
        let resp_body =
            crate::http_body::decode_response_body_async(&resp_headers, resp_body, max_body_size)
                .await;

        // Build the stored exchange with replayed = true.
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now();

        let stored_request = crate::storage::StoredRequest {
            id: id.clone(),
            timestamp: now,
            tunnel_name: req.tunnel_name.clone(),
            method: req.method.clone(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: body_bytes,
            raw_request: vec![],
            replayed: true,
        };

        let stored_response = crate::storage::StoredResponse {
            request_id: id.clone(),
            timestamp: now,
            status,
            headers: resp_headers,
            body: resp_body,
            raw_response: vec![],
            response_time_ms: Some(elapsed),
        };

        let exchange = crate::storage::RequestExchange {
            request: stored_request,
            response: Some(stored_response),
        };

        self.request_storage.store_exchange(exchange).await;

        Ok(ReplayResponsePayload {
            id: Some(id),
            error: None,
        })
    }

    pub async fn get_cloudflare_status(&self) -> CloudflareStatusResponse {
        let (configured, tunnel_id, tunnel_name) = {
            let cfg = self.config.read().await;
            match &cfg.cloudflare {
                Some(cf) => (true, cf.tunnel_id.clone(), Some(cf.tunnel_name.clone())),
                None => (false, None, None),
            }
        };

        let connector = if configured {
            *self.connector.borrow()
        } else {
            ConnectorState::Stopped
        };

        CloudflareStatusResponse {
            configured,
            tunnel_id,
            tunnel_name,
            connector,
        }
    }

    pub async fn clear_requests_for_tunnel(&self, tunnel_name: &str) {
        self.request_storage
            .clear_requests_for_tunnel(tunnel_name)
            .await;
    }
}

pub fn tunnel_info_from_config(t: &TunnelConfig) -> TunnelInfo {
    TunnelInfo {
        name: t.name.clone(),
        domain: t.domain.clone(),
        socket_path: t.socket_path.clone(),
        destination: t.target_port,
        enabled: t.enabled,
    }
}

pub fn get_default_socket_path(tunnel_name: &str) -> String {
    let suffix: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    let filename = format!("tunneldesk-{}-{}.sock", tunnel_name, suffix);
    let mut dir = env::temp_dir();
    dir.push(filename);
    dir.to_string_lossy().to_string()
}

pub fn request_to_base64(request: &crate::storage::StoredRequest) -> StoredRequestWithBase64 {
    StoredRequestWithBase64 {
        id: request.id.clone(),
        timestamp: request.timestamp,
        tunnel_name: request.tunnel_name.clone(),
        method: request.method.clone(),
        url: request.url.clone(),
        headers: request.headers.clone(),
        body: base64::engine::general_purpose::STANDARD.encode(&request.body),
        raw_request: base64::engine::general_purpose::STANDARD.encode(&request.raw_request),
        replayed: request.replayed,
    }
}

pub fn response_to_base64(response: &crate::storage::StoredResponse) -> StoredResponseWithBase64 {
    StoredResponseWithBase64 {
        request_id: response.request_id.clone(),
        timestamp: response.timestamp,
        status: response.status,
        headers: response.headers.clone(),
        body: base64::engine::general_purpose::STANDARD.encode(&response.body),
        raw_response: base64::engine::general_purpose::STANDARD.encode(&response.raw_response),
        response_time_ms: response.response_time_ms,
    }
}

pub fn exchange_to_base64(exchange: &crate::storage::RequestExchange) -> RequestExchangeWithBase64 {
    RequestExchangeWithBase64 {
        request: request_to_base64(&exchange.request),
        response: exchange.response.as_ref().map(response_to_base64),
    }
}

pub fn websocket_message_to_base64(
    message: &crate::storage::StoredWebSocketMessage,
) -> StoredWebSocketMessageWithBase64 {
    StoredWebSocketMessageWithBase64 {
        id: message.id.clone(),
        timestamp: message.timestamp,
        tunnel_name: message.tunnel_name.clone(),
        upgrade_request_id: message.upgrade_request_id.clone(),
        direction: message.direction.clone(),
        message_type: message.message_type.clone(),
        payload: base64::engine::general_purpose::STANDARD.encode(&message.payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudflare::CloudflareClient;

    fn make_service(cloudflare: bool) -> AppService {
        let mut config = Config::default_config();
        if cloudflare {
            config.cloudflare = Some(crate::config::CloudflareConfig {
                api_token: "tok".into(),
                account_id: "acc".into(),
                zone_id: "zone".into(),
                tunnel_id: Some("tid".into()),
                tunnel_name: "td".into(),
                tunnel_token: None,
                manage_cloudflared: true,
            });
        }
        let request_storage = Arc::new(RequestStorage::new(10));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(10));
        let tunnel_manager = Arc::new(TunnelManager::new(
            &config,
            request_storage.clone(),
            websocket_storage.clone(),
        ));
        AppService::new(
            Arc::new(RwLock::new(config)),
            tunnel_manager,
            request_storage,
            websocket_storage,
        )
    }

    fn socket_in(dir: &tempfile::TempDir, name: &str) -> Option<String> {
        Some(dir.path().join(name).to_string_lossy().into_owned())
    }

    #[tokio::test]
    async fn tunnel_changes_are_broadcast_as_events() {
        let service = make_service(false);
        let dir = tempfile::tempdir().unwrap();
        let mut events = service.subscribe_events();

        service
            .create_tunnel(CreateTunnelRequest {
                name: "api".into(),
                domain: "api.example.com".into(),
                socket_path: socket_in(&dir, "api.sock"),
                target_port: 4000,
            })
            .await
            .unwrap();
        assert!(matches!(events.recv().await.unwrap(), TunnelEvent::Created(i) if i.name == "api"));

        service
            .update_tunnel(UpdateTunnelRequest {
                name: "api".into(),
                domain: None,
                socket_path: None,
                target_port: Some(4001),
                enabled: None,
            })
            .await
            .unwrap();
        assert!(
            matches!(events.recv().await.unwrap(), TunnelEvent::Updated(i) if i.destination == 4001)
        );

        service
            .delete_tunnel(DeleteTunnelRequest { name: "api".into() })
            .await
            .unwrap();
        assert!(matches!(events.recv().await.unwrap(), TunnelEvent::Deleted(d) if d.name == "api"));
        service.tunnel_manager.shutdown().await;
    }

    #[tokio::test]
    async fn failed_change_emits_no_event() {
        let service = make_service(false);
        let mut events = service.subscribe_events();
        assert!(
            service
                .delete_tunnel(DeleteTunnelRequest {
                    name: "ghost".into()
                })
                .await
                .is_err()
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn sync_is_unavailable_until_set() {
        let service = make_service(true);
        assert!(service.tunnel_sync().is_none());
        assert!(service.sync_tunnels().await.is_err());

        let client = CloudflareClient::new("tok", "acc", "zone").unwrap();
        let sync = Arc::new(TunnelSync::new(client, "tid"));
        assert!(service.set_tunnel_sync(sync.clone()));
        assert!(
            !service.set_tunnel_sync(sync),
            "second set must be rejected"
        );
        assert!(service.tunnel_sync().is_some());
    }

    #[tokio::test]
    async fn cloudflare_status_reports_connector_state() {
        let service = make_service(true);
        assert_eq!(
            service.get_cloudflare_status().await.connector,
            ConnectorState::Stopped
        );
        service
            .connector_sender()
            .send_replace(ConnectorState::Connected);
        let status = service.get_cloudflare_status().await;
        assert!(status.configured);
        assert_eq!(status.connector, ConnectorState::Connected);
        assert_eq!(
            *service.subscribe_connector().borrow(),
            ConnectorState::Connected
        );
    }

    #[tokio::test]
    async fn replayed_compressed_response_is_stored_decoded() {
        use std::io::Write as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(b"{\"replayed\":true}").unwrap();
        let compressed = gzip.finish().unwrap();
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            compressed.len()
        )
        .into_bytes();
        response.extend_from_slice(&compressed);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            stream.write_all(&response).await.unwrap();
            let _ = stream.shutdown().await;
        });

        let service = make_service(false);
        service.config.write().await.tunnels.push(TunnelConfig {
            name: "api".into(),
            domain: "api.example.com".into(),
            socket_path: "/tmp/unused.sock".into(),
            target_port,
            enabled: true,
        });

        let result = service
            .replay_request(ReplayRequestPayload {
                tunnel_name: "api".into(),
                method: "GET".into(),
                url: "/".into(),
                headers: [("Accept-Encoding".to_string(), "gzip".to_string())].into(),
                body: String::new(),
            })
            .await
            .unwrap();
        assert_eq!(result.error, None);

        let exchange = service
            .request_storage
            .get_request_by_id(&result.id.unwrap())
            .await
            .unwrap();
        assert_eq!(exchange.response.unwrap().body, b"{\"replayed\":true}");
    }

    #[tokio::test]
    async fn unconfigured_cloudflare_reports_stopped_connector() {
        let service = make_service(false);
        service
            .connector_sender()
            .send_replace(ConnectorState::Connected);
        let status = service.get_cloudflare_status().await;
        assert!(!status.configured);
        assert_eq!(status.connector, ConnectorState::Stopped);
    }
}
