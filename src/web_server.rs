use axum::{
    Router,
    body::Bytes,
    extract::{
        State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::Engine as _;
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::cloudflared::CloudflaredService;
use crate::config::{Config, TunnelConfig};
use crate::storage::{QueryFilter, WebSocketMessageFilter};
use crate::sync::TunnelSync;
use crate::tunnel::TunnelManager;

/// A [`RequestExchange`](crate::storage::RequestExchange) with binary fields
/// base64-encoded for safe JSON transport to the browser.
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

/// Payload for a replay response sent to the browser.
///
/// On success `id` is the ID of the stored replayed exchange; on error `error` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResponsePayload {
    /// ID of the stored replayed exchange (`None` when the request failed before storage).
    pub id: Option<String>,
    /// Error message when `id` is `None`.
    pub error: Option<String>,
}

/// Commands sent by the browser over the GUI WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WebSocketMessage {
    // --- Query / subscribe ---
    ListTunnels,
    QueryRequests(QueryFilter),
    QueryWebSocketMessages(WebSocketMessageFilter),
    Subscribe(QueryFilter),
    Unsubscribe,
    // --- Tunnel CRUD ---
    CreateTunnel(CreateTunnelRequest),
    UpdateTunnel(UpdateTunnelRequest),
    DeleteTunnel(DeleteTunnelRequest),
    // --- Cloudflare management ---
    SyncTunnels,
    ConfirmRemoveHosts(ConfirmRemoveHostsRequest),
    GetCloudflareStatus,
    // --- Replay ---
    ReplayRequest(ReplayRequestPayload),
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

/// Responses sent by the server over the GUI WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WebSocketResponse {
    // --- Query responses ---
    Tunnels(Vec<TunnelInfo>),
    Requests(Vec<RequestExchangeWithBase64>),
    WebSocketMessages(Vec<StoredWebSocketMessageWithBase64>),
    /// Push notification for a newly completed request–response exchange.
    NewRequest(Box<RequestExchangeWithBase64>),
    /// Push notification for a newly stored WebSocket frame.
    NewWebSocketMessage(Box<StoredWebSocketMessageWithBase64>),
    // --- CRUD responses ---
    TunnelCreated(TunnelInfo),
    TunnelUpdated(TunnelInfo),
    TunnelDeleted(TunnelDeletedResponse),
    // --- Cloudflare management responses ---
    SyncReport(SyncReportResponse),
    /// Hosts found on Cloudflare but absent from local config; requires user confirmation.
    UnknownHostsFound(UnknownHostsFoundResponse),
    CloudflareStatus(CloudflareStatusResponse),
    // --- Replay ---
    ReplayResponse(ReplayResponsePayload),
    Error(String),
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
    pub service_running: bool,
}

#[derive(Embed)]
#[folder = "frontend/build"]
struct FrontendAssets;

async fn serve_frontend(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "200.html" } else { path };
    serve_asset(path)
        .or_else(|| serve_asset("200.html"))
        .unwrap_or_else(|| axum::http::StatusCode::NOT_FOUND.into_response())
}

fn serve_asset(path: &str) -> Option<Response> {
    let content = FrontendAssets::get(path)?;
    let mime = content.metadata.mimetype();
    Some(
        (
            [(header::CONTENT_TYPE, mime)],
            Bytes::from(content.data.into_owned()),
        )
            .into_response(),
    )
}

/// Serves the static web UI and handles GUI WebSocket connections.
#[derive(Clone)]
pub struct WebServer {
    config: Arc<RwLock<Config>>,
    tunnel_manager: Arc<TunnelManager>,
    tunnel_sync: Option<Arc<TunnelSync>>,
    request_storage: Arc<crate::storage::RequestStorage>,
    websocket_storage: Arc<crate::storage::WebSocketMessageStorage>,
    current_filter: Arc<RwLock<Option<QueryFilter>>>,
    current_ws_filter: Arc<RwLock<Option<WebSocketMessageFilter>>>,
}

impl WebServer {
    /// Creates a new `WebServer`.
    pub fn new(
        config: Arc<RwLock<Config>>,
        tunnel_manager: Arc<TunnelManager>,
        tunnel_sync: Option<Arc<TunnelSync>>,
        request_storage: Arc<crate::storage::RequestStorage>,
        websocket_storage: Arc<crate::storage::WebSocketMessageStorage>,
    ) -> Self {
        Self {
            config,
            tunnel_manager,
            tunnel_sync,
            request_storage,
            websocket_storage,
            current_filter: Arc::new(RwLock::new(None)),
            current_ws_filter: Arc::new(RwLock::new(None)),
        }
    }

    /// Binds to the configured port and serves the web UI and WebSocket API.
    pub async fn start(&self) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/ws", get(websocket_handler))
            .fallback(serve_frontend)
            .with_state(Arc::new(self.clone()));

        let port = self.config.read().await.gui.port;
        let addr = format!("127.0.0.1:{port}");
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        tracing::info!("Web GUI server listening on http://{}", addr);

        axum::serve(listener, app).await?;
        Ok(())
    }

    // ── Query handlers ────────────────────────────────────────────────────────

    async fn handle_list_tunnels(&self) -> WebSocketResponse {
        let cfg = self.config.read().await;
        let tunnels = cfg.tunnels.iter().map(tunnel_info_from_config).collect();
        WebSocketResponse::Tunnels(tunnels)
    }

    async fn handle_query_requests(&self, filter: &QueryFilter) -> WebSocketResponse {
        let requests = self.request_storage.query_requests(filter).await;
        let requests_with_base64: Vec<RequestExchangeWithBase64> =
            requests.iter().map(exchange_to_base64).collect();
        WebSocketResponse::Requests(requests_with_base64)
    }

    async fn handle_query_websocket_messages(
        &self,
        filter: &WebSocketMessageFilter,
    ) -> WebSocketResponse {
        let messages = self.websocket_storage.query_messages(filter).await;
        let messages_with_base64: Vec<StoredWebSocketMessageWithBase64> =
            messages.iter().map(websocket_message_to_base64).collect();
        WebSocketResponse::WebSocketMessages(messages_with_base64)
    }

    async fn handle_subscribe(&self, filter: QueryFilter) -> WebSocketResponse {
        *self.current_filter.write().await = Some(filter);
        WebSocketResponse::Requests(vec![])
    }

    async fn handle_unsubscribe(&self) -> WebSocketResponse {
        *self.current_filter.write().await = None;
        *self.current_ws_filter.write().await = None;
        WebSocketResponse::Requests(vec![])
    }

    // ── CRUD handlers ─────────────────────────────────────────────────────────

    async fn handle_create_tunnel(&self, req: CreateTunnelRequest) -> WebSocketResponse {
        // Validate uniqueness.
        {
            let cfg = self.config.read().await;
            if cfg.tunnels.iter().any(|t| t.name == req.name) {
                return WebSocketResponse::Error(format!(
                    "A tunnel named '{}' already exists",
                    req.name
                ));
            }
        }

        let socket_path = req
            .socket_path
            .unwrap_or_else(|| format!("/tmp/tunneldesk-{}.sock", req.name));

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
                return WebSocketResponse::Error(format!("Failed to save config: {e}"));
            }
        }

        // Cloudflare: add ingress rule + DNS.
        if new_tunnel.enabled
            && let Some(sync) = &self.tunnel_sync
            && let Err(e) = sync.add_single_tunnel(&new_tunnel).await
        {
            tracing::warn!("Cloudflare add_single_tunnel failed: {e}");
        }

        // Start local proxy.
        self.tunnel_manager.start_tunnel(new_tunnel).await;

        WebSocketResponse::TunnelCreated(info)
    }

    async fn handle_update_tunnel(&self, req: UpdateTunnelRequest) -> WebSocketResponse {
        let old_tunnel = {
            let cfg = self.config.read().await;
            match cfg.tunnels.iter().find(|t| t.name == req.name) {
                Some(t) => t.clone(),
                None => {
                    return WebSocketResponse::Error(format!("Tunnel '{}' not found", req.name));
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
                return WebSocketResponse::Error(format!("Failed to save config: {e}"));
            }
        }

        // Cloudflare sync.
        if let Some(sync) = &self.tunnel_sync {
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

        WebSocketResponse::TunnelUpdated(info)
    }

    async fn handle_delete_tunnel(&self, req: DeleteTunnelRequest) -> WebSocketResponse {
        let tunnel = {
            let cfg = self.config.read().await;
            match cfg.tunnels.iter().find(|t| t.name == req.name) {
                Some(t) => t.clone(),
                None => {
                    return WebSocketResponse::Error(format!("Tunnel '{}' not found", req.name));
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
                return WebSocketResponse::Error(format!("Failed to save config: {e}"));
            }
        }

        // Cloudflare: remove ingress + DNS.
        if tunnel.enabled
            && let Some(sync) = &self.tunnel_sync
            && let Err(e) = sync.remove_single_tunnel(&tunnel.domain).await
        {
            tracing::warn!("Cloudflare remove_single_tunnel failed: {e}");
        }

        // Stop local proxy.
        self.tunnel_manager.stop_tunnel(&req.name).await;

        WebSocketResponse::TunnelDeleted(TunnelDeletedResponse { name: req.name })
    }

    // ── Cloudflare management handlers ───────────────────────────────────────

    async fn handle_sync_tunnels(&self) -> WebSocketResponse {
        let sync = match &self.tunnel_sync {
            Some(s) => s.clone(),
            None => {
                return WebSocketResponse::Error(
                    "Cloudflare integration is not configured".to_string(),
                );
            }
        };

        let cfg = self.config.read().await;
        let report = sync.sync_to_cloudflare(&cfg).await;
        drop(cfg);

        let unknown = report.unknown_hosts.clone();
        let resp = SyncReportResponse {
            added: report.added,
            removed: report.removed,
            unknown_hosts: report.unknown_hosts,
            errors: report.errors,
        };

        // If there are unknown hosts, also emit an UnknownHostsFound message.
        // The handler sends only one response per message, so we embed the
        // unknown-hosts info inside the SyncReport and let the frontend decide.
        let _ = unknown;

        WebSocketResponse::SyncReport(resp)
    }

    async fn handle_confirm_remove_hosts(
        &self,
        req: ConfirmRemoveHostsRequest,
    ) -> WebSocketResponse {
        let sync = match &self.tunnel_sync {
            Some(s) => s.clone(),
            None => {
                return WebSocketResponse::Error(
                    "Cloudflare integration is not configured".to_string(),
                );
            }
        };

        match sync.remove_hosts(&req.hosts).await {
            Ok(removed) => WebSocketResponse::SyncReport(SyncReportResponse {
                added: vec![],
                removed,
                unknown_hosts: vec![],
                errors: vec![],
            }),
            Err(e) => WebSocketResponse::Error(format!("Failed to remove hosts: {e}")),
        }
    }

    // ── Replay handler ────────────────────────────────────────────────────────

    async fn handle_replay_request(&self, req: ReplayRequestPayload) -> WebSocketResponse {
        macro_rules! err {
            ($msg:expr) => {
                return WebSocketResponse::ReplayResponse(ReplayResponsePayload {
                    id: None,
                    error: Some($msg),
                })
            };
        }

        let target_port = {
            let cfg = self.config.read().await;
            match cfg.tunnels.iter().find(|t| t.name == req.tunnel_name) {
                Some(t) => t.target_port,
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

        WebSocketResponse::ReplayResponse(ReplayResponsePayload {
            id: Some(id),
            error: None,
        })
    }

    async fn handle_get_cloudflare_status(&self) -> WebSocketResponse {
        let (configured, tunnel_id, tunnel_name) = {
            let cfg = self.config.read().await;
            match &cfg.cloudflare {
                Some(cf) => (true, cf.tunnel_id.clone(), Some(cf.tunnel_name.clone())),
                None => (false, None, None),
            }
        };

        let service_running = if configured {
            CloudflaredService::is_running().await
        } else {
            false
        };

        WebSocketResponse::CloudflareStatus(CloudflareStatusResponse {
            configured,
            tunnel_id,
            tunnel_name,
            service_running,
        })
    }
}

fn tunnel_info_from_config(t: &TunnelConfig) -> TunnelInfo {
    TunnelInfo {
        name: t.name.clone(),
        domain: t.domain.clone(),
        socket_path: t.socket_path.clone(),
        destination: t.target_port,
        enabled: t.enabled,
    }
}

async fn websocket_handler(ws: WebSocketUpgrade, State(server): State<Arc<WebServer>>) -> Response {
    ws.on_upgrade(|socket| websocket_connection(socket, server))
}

async fn websocket_connection(mut socket: axum::extract::ws::WebSocket, server: Arc<WebServer>) {
    // Subscribe to request and WebSocket message broadcasts
    let mut request_receiver = server.request_storage.subscribe_requests();
    let mut ws_message_receiver = server.websocket_storage.subscribe_messages();
    let current_filter = server.current_filter.clone();

    loop {
        tokio::select! {
            // Handle incoming WebSocket messages
            Some(msg) = socket.recv() => {
                if let Ok(msg) = msg {
                    match msg {
                        Message::Text(text) => {
                            if let Ok(ws_message) = serde_json::from_str::<WebSocketMessage>(&text) {
                                let response = match ws_message {
                                    WebSocketMessage::ListTunnels => server.handle_list_tunnels().await,
                                    WebSocketMessage::QueryRequests(filter) => {
                                        server.handle_query_requests(&filter).await
                                    }
                                    WebSocketMessage::QueryWebSocketMessages(filter) => {
                                        server.handle_query_websocket_messages(&filter).await
                                    }
                                    WebSocketMessage::Subscribe(filter) => {
                                        server.handle_subscribe(filter).await
                                    }
                                    WebSocketMessage::Unsubscribe => server.handle_unsubscribe().await,
                                    WebSocketMessage::CreateTunnel(req) => {
                                        server.handle_create_tunnel(req).await
                                    }
                                    WebSocketMessage::UpdateTunnel(req) => {
                                        server.handle_update_tunnel(req).await
                                    }
                                    WebSocketMessage::DeleteTunnel(req) => {
                                        server.handle_delete_tunnel(req).await
                                    }
                                    WebSocketMessage::SyncTunnels => {
                                        server.handle_sync_tunnels().await
                                    }
                                    WebSocketMessage::ConfirmRemoveHosts(req) => {
                                        server.handle_confirm_remove_hosts(req).await
                                    }
                                    WebSocketMessage::GetCloudflareStatus => {
                                        server.handle_get_cloudflare_status().await
                                    }
                                    WebSocketMessage::ReplayRequest(req) => {
                                        server.handle_replay_request(req).await
                                    }
                                };

                                if let Ok(response_text) = serde_json::to_string(&response) {
                                    let _ = socket.send(Message::Text(response_text)).await;
                                }
                            } else {
                                tracing::warn!("Could not parse WebSocket message: {text}");
                            }
                        }
                        Message::Binary(binary) => {
                            tracing::warn!("Received binary message: {:?}", binary);
                        }
                        _ => break, // Connection closed
                    }
                } else {
                    break; // Connection error
                }
            }
            // Handle broadcast request messages
            Ok(exchange) = request_receiver.recv() => {
                let filter = current_filter.read().await;

                // Check if this exchange matches the current filter
                let matches_filter = if let Some(ref filter) = *filter {
                    filter.matches(&exchange)
                } else {
                    true // No filter means accept all
                };

                drop(filter); // Release the lock

                if matches_filter {
                    let response = WebSocketResponse::NewRequest(Box::new(exchange_to_base64(&exchange)));
                    if let Ok(response_text) = serde_json::to_string(&response) {
                        let _ = socket.send(Message::Text(response_text)).await;
                    }
                }
            }
            // Handle broadcast WebSocket messages
            Ok(ws_msg) = ws_message_receiver.recv() => {
                let response = WebSocketResponse::NewWebSocketMessage(Box::new(websocket_message_to_base64(&ws_msg)));
                if let Ok(response_text) = serde_json::to_string(&response) {
                    let _ = socket.send(Message::Text(response_text)).await;
                }
            }
        }
    }
}

fn request_to_base64(request: &crate::storage::StoredRequest) -> StoredRequestWithBase64 {
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

fn response_to_base64(response: &crate::storage::StoredResponse) -> StoredResponseWithBase64 {
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

fn exchange_to_base64(exchange: &crate::storage::RequestExchange) -> RequestExchangeWithBase64 {
    RequestExchangeWithBase64 {
        request: request_to_base64(&exchange.request),
        response: exchange.response.as_ref().map(response_to_base64),
    }
}

fn websocket_message_to_base64(
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
    use crate::config::{CaptureConfig, Config, GuiConfig, LoggingConfig, TunnelConfig};
    use crate::storage::{
        RequestStorage, StatusFilter, StoredRequest, StoredResponse, StoredWebSocketMessage,
        WebSocketMessageStorage, WebSocketMessageType,
    };
    use std::collections::HashMap;

    fn make_config() -> Config {
        Config {
            tunnels: vec![
                TunnelConfig {
                    name: "tunnel-a".to_string(),
                    domain: "a.example.com".to_string(),
                    socket_path: "/tmp/a.sock".to_string(),
                    target_port: 3000,
                    enabled: true,
                },
                TunnelConfig {
                    name: "tunnel-b".to_string(),
                    domain: "b.example.com".to_string(),
                    socket_path: "/tmp/b.sock".to_string(),
                    target_port: 3001,
                    enabled: true,
                },
            ],
            logging: LoggingConfig {
                stdout_level: "off".to_string(),
                max_request_body_size: 1024,
            },
            capture: CaptureConfig {
                max_stored_requests: 100,
                max_request_body_size: 1024 * 1024,
            },
            gui: GuiConfig { port: 8080 },
            cloudflare: None,
            config_path: None,
        }
    }

    fn make_web_server() -> WebServer {
        let config = Arc::new(RwLock::new(make_config()));
        let req_storage = Arc::new(RequestStorage::new(100));
        let ws_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let tm = Arc::new(TunnelManager::new(
            &make_config(),
            req_storage.clone(),
            ws_storage.clone(),
        ));
        WebServer::new(config, tm, None, req_storage, ws_storage)
    }

    fn make_stored_request(id: &str, tunnel: &str, method: &str, url: &str) -> StoredRequest {
        StoredRequest {
            id: id.to_string(),
            timestamp: chrono::Utc::now(),
            tunnel_name: tunnel.to_string(),
            method: method.to_string(),
            url: url.to_string(),
            headers: HashMap::new(),
            body: b"request body".to_vec(),
            raw_request: b"GET / HTTP/1.1\r\n\r\n".to_vec(),
            replayed: false,
        }
    }

    fn make_stored_response(request_id: &str, status: u16) -> StoredResponse {
        StoredResponse {
            request_id: request_id.to_string(),
            timestamp: chrono::Utc::now(),
            status,
            headers: HashMap::new(),
            body: b"response body".to_vec(),
            raw_response: b"HTTP/1.1 200 OK\r\n\r\n".to_vec(),
            response_time_ms: Some(42.0),
        }
    }

    // --- request_to_base64 ---

    #[test]
    fn test_request_to_base64_encodes_body_and_raw() {
        let req = make_stored_request("r1", "tunnel-a", "GET", "/path");
        let out = request_to_base64(&req);

        assert_eq!(out.id, "r1");
        assert_eq!(out.method, "GET");
        assert_eq!(out.url, "/path");
        assert_eq!(
            out.body,
            base64::engine::general_purpose::STANDARD.encode(b"request body")
        );
        assert_eq!(
            out.raw_request,
            base64::engine::general_purpose::STANDARD.encode(b"GET / HTTP/1.1\r\n\r\n")
        );
    }

    // --- response_to_base64 ---

    #[test]
    fn test_response_to_base64_encodes_body_and_raw() {
        let resp = make_stored_response("r1", 200);
        let out = response_to_base64(&resp);

        assert_eq!(out.request_id, "r1");
        assert_eq!(out.status, 200);
        assert_eq!(
            out.body,
            base64::engine::general_purpose::STANDARD.encode(b"response body")
        );
        assert_eq!(
            out.raw_response,
            base64::engine::general_purpose::STANDARD.encode(b"HTTP/1.1 200 OK\r\n\r\n")
        );
        assert_eq!(out.response_time_ms, Some(42.0));
    }

    // --- websocket_message_to_base64 ---

    #[test]
    fn test_websocket_message_to_base64_encodes_payload() {
        let msg = StoredWebSocketMessage {
            id: "m1".to_string(),
            timestamp: chrono::Utc::now(),
            tunnel_name: "tunnel-a".to_string(),
            upgrade_request_id: "r1".to_string(),
            direction: "→".to_string(),
            message_type: WebSocketMessageType::Text,
            payload: b"hello ws".to_vec(),
        };
        let out = websocket_message_to_base64(&msg);

        assert_eq!(out.id, "m1");
        assert_eq!(out.direction, "→");
        assert_eq!(
            out.payload,
            base64::engine::general_purpose::STANDARD.encode(b"hello ws")
        );
    }

    // --- exchange_to_base64 ---

    #[test]
    fn test_exchange_to_base64_without_response() {
        let req = make_stored_request("r1", "tunnel-a", "POST", "/submit");
        let exchange = crate::storage::RequestExchange {
            request: req,
            response: None,
        };
        let out = exchange_to_base64(&exchange);

        assert_eq!(out.request.id, "r1");
        assert!(out.response.is_none());
    }

    #[test]
    fn test_exchange_to_base64_with_response() {
        let req = make_stored_request("r1", "tunnel-a", "GET", "/");
        let resp = make_stored_response("r1", 404);
        let exchange = crate::storage::RequestExchange {
            request: req,
            response: Some(resp),
        };
        let out = exchange_to_base64(&exchange);

        assert!(out.response.is_some());
        assert_eq!(out.response.unwrap().status, 404);
    }

    // --- handle_list_tunnels ---

    #[tokio::test]
    async fn test_handle_list_tunnels_returns_all_tunnels() {
        let server = make_web_server();
        let response = server.handle_list_tunnels().await;

        let WebSocketResponse::Tunnels(tunnels) = response else {
            panic!("expected Tunnels variant");
        };

        assert_eq!(tunnels.len(), 2);
        assert_eq!(tunnels[0].name, "tunnel-a");
        assert_eq!(tunnels[0].domain, "a.example.com");
        assert_eq!(tunnels[0].destination, 3000);
        assert!(tunnels[0].enabled);
        assert_eq!(tunnels[1].name, "tunnel-b");
        assert_eq!(tunnels[1].destination, 3001);
    }

    // --- handle_query_requests ---

    #[tokio::test]
    async fn test_handle_query_requests_returns_matching() {
        let config = Arc::new(RwLock::new(make_config()));
        let req_storage = Arc::new(RequestStorage::new(100));
        let ws_storage = Arc::new(WebSocketMessageStorage::new(100));
        let tm = Arc::new(TunnelManager::new(
            &make_config(),
            req_storage.clone(),
            ws_storage.clone(),
        ));
        let server = WebServer::new(config, tm, None, req_storage.clone(), ws_storage);

        let req = make_stored_request("r1", "tunnel-a", "GET", "/api");
        req_storage.store_request(req).await;

        let filter = QueryFilter {
            tunnel_name: Some("tunnel-a".to_string()),
            ..Default::default()
        };
        let response = server.handle_query_requests(&filter).await;

        let WebSocketResponse::Requests(exchanges) = response else {
            panic!("expected Requests variant");
        };
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].request.id, "r1");
    }

    #[tokio::test]
    async fn test_handle_query_requests_empty_when_no_match() {
        let server = make_web_server();

        let filter = QueryFilter {
            tunnel_name: Some("no-such-tunnel".to_string()),
            ..Default::default()
        };
        let response = server.handle_query_requests(&filter).await;

        let WebSocketResponse::Requests(exchanges) = response else {
            panic!("expected Requests variant");
        };
        assert!(exchanges.is_empty());
    }

    // --- handle_create_tunnel ---

    #[tokio::test]
    async fn test_handle_create_tunnel_adds_tunnel() {
        let server = make_web_server();
        let req = CreateTunnelRequest {
            name: "new-tunnel".to_string(),
            domain: "new.example.com".to_string(),
            socket_path: Some("/tmp/new.sock".to_string()),
            target_port: 9999,
        };
        let response = server.handle_create_tunnel(req).await;

        let WebSocketResponse::TunnelCreated(info) = response else {
            panic!("expected TunnelCreated, got {:?}", response);
        };
        assert_eq!(info.name, "new-tunnel");
        assert_eq!(info.domain, "new.example.com");
        assert_eq!(info.destination, 9999);
        assert!(info.enabled);

        // Config must contain the new tunnel.
        let cfg = server.config.read().await;
        assert_eq!(cfg.tunnels.len(), 3);
        assert!(cfg.tunnels.iter().any(|t| t.name == "new-tunnel"));
    }

    #[tokio::test]
    async fn test_handle_create_tunnel_rejects_duplicate_name() {
        let server = make_web_server();
        let req = CreateTunnelRequest {
            name: "tunnel-a".to_string(), // already exists
            domain: "other.example.com".to_string(),
            socket_path: None,
            target_port: 7777,
        };
        let response = server.handle_create_tunnel(req).await;
        assert!(matches!(response, WebSocketResponse::Error(_)));
    }

    #[tokio::test]
    async fn test_handle_create_tunnel_generates_socket_path() {
        let server = make_web_server();
        let req = CreateTunnelRequest {
            name: "auto-path".to_string(),
            domain: "auto.example.com".to_string(),
            socket_path: None, // should be auto-generated
            target_port: 5555,
        };
        let response = server.handle_create_tunnel(req).await;
        assert!(matches!(response, WebSocketResponse::TunnelCreated(_)));

        let cfg = server.config.read().await;
        let t = cfg.tunnels.iter().find(|t| t.name == "auto-path").unwrap();
        assert_eq!(t.socket_path, "/tmp/tunneldesk-auto-path.sock");
    }

    // --- handle_update_tunnel ---

    #[tokio::test]
    async fn test_handle_update_tunnel_changes_domain() {
        let server = make_web_server();
        let req = UpdateTunnelRequest {
            name: "tunnel-a".to_string(),
            domain: Some("updated.example.com".to_string()),
            socket_path: None,
            target_port: None,
            enabled: None,
        };
        let response = server.handle_update_tunnel(req).await;

        let WebSocketResponse::TunnelUpdated(info) = response else {
            panic!("expected TunnelUpdated, got {:?}", response);
        };
        assert_eq!(info.domain, "updated.example.com");

        let cfg = server.config.read().await;
        let t = cfg.tunnels.iter().find(|t| t.name == "tunnel-a").unwrap();
        assert_eq!(t.domain, "updated.example.com");
    }

    #[tokio::test]
    async fn test_handle_update_tunnel_disables_tunnel() {
        let config = Arc::new(RwLock::new(make_config()));
        let req_storage = Arc::new(RequestStorage::new(100));
        let ws_storage = Arc::new(WebSocketMessageStorage::new(1000));
        let tm = Arc::new(TunnelManager::new(
            &make_config(),
            req_storage.clone(),
            ws_storage.clone(),
        ));
        let server = WebServer::new(config, tm.clone(), None, req_storage, ws_storage);

        // Seed tunnel-a so it has a live handle; disabling should stop it.
        let tunnel_a = make_config()
            .tunnels
            .into_iter()
            .find(|t| t.name == "tunnel-a")
            .unwrap();
        tm.start_tunnel(tunnel_a).await;
        assert!(tm.is_tunnel_running("tunnel-a").await);

        let req = UpdateTunnelRequest {
            name: "tunnel-a".to_string(),
            domain: None,
            socket_path: None,
            target_port: None,
            enabled: Some(false),
        };
        let response = server.handle_update_tunnel(req).await;

        let WebSocketResponse::TunnelUpdated(info) = response else {
            panic!("expected TunnelUpdated, got {:?}", response);
        };
        assert_eq!(info.enabled, false);

        let cfg = server.config.read().await;
        let t = cfg.tunnels.iter().find(|t| t.name == "tunnel-a").unwrap();
        assert_eq!(t.enabled, false);

        // stop_tunnel must have been called (not restart_tunnel, which would
        // re-add the handle).
        assert!(!tm.is_tunnel_running("tunnel-a").await);
    }

    #[tokio::test]
    async fn test_handle_update_tunnel_not_found() {
        let server = make_web_server();
        let req = UpdateTunnelRequest {
            name: "ghost".to_string(),
            domain: None,
            socket_path: None,
            target_port: None,
            enabled: None,
        };
        let response = server.handle_update_tunnel(req).await;
        assert!(matches!(response, WebSocketResponse::Error(_)));
    }

    // --- handle_delete_tunnel ---

    #[tokio::test]
    async fn test_handle_delete_tunnel_removes_tunnel() {
        let server = make_web_server();
        let req = DeleteTunnelRequest {
            name: "tunnel-a".to_string(),
        };
        let response = server.handle_delete_tunnel(req).await;

        let WebSocketResponse::TunnelDeleted(resp) = response else {
            panic!("expected TunnelDeleted, got {:?}", response);
        };
        assert_eq!(resp.name, "tunnel-a");

        let cfg = server.config.read().await;
        assert_eq!(cfg.tunnels.len(), 1);
        assert!(cfg.tunnels.iter().all(|t| t.name != "tunnel-a"));
    }

    #[tokio::test]
    async fn test_handle_delete_tunnel_not_found() {
        let server = make_web_server();
        let req = DeleteTunnelRequest {
            name: "ghost".to_string(),
        };
        let response = server.handle_delete_tunnel(req).await;
        assert!(matches!(response, WebSocketResponse::Error(_)));
    }

    // --- handle_get_cloudflare_status ---

    #[tokio::test]
    async fn test_handle_get_cloudflare_status_not_configured() {
        let server = make_web_server();
        let response = server.handle_get_cloudflare_status().await;

        let WebSocketResponse::CloudflareStatus(status) = response else {
            panic!("expected CloudflareStatus");
        };
        assert!(!status.configured);
        assert!(status.tunnel_id.is_none());
        assert!(!status.service_running);
    }

    #[tokio::test]
    async fn test_handle_get_cloudflare_status_configured() {
        let mut cfg = make_config();
        cfg.cloudflare = Some(crate::config::CloudflareConfig {
            api_token: "tok".to_string(),
            account_id: "acc".to_string(),
            zone_id: "zone".to_string(),
            tunnel_id: Some("tid-123".to_string()),
            tunnel_name: "myapp".to_string(),
            tunnel_token: Some("token".to_string()),
        });

        let config = Arc::new(RwLock::new(cfg.clone()));
        let req_storage = Arc::new(RequestStorage::new(100));
        let ws_storage = Arc::new(WebSocketMessageStorage::new(100));
        let tm = Arc::new(TunnelManager::new(
            &cfg,
            req_storage.clone(),
            ws_storage.clone(),
        ));
        let server = WebServer::new(config, tm, None, req_storage, ws_storage);

        let response = server.handle_get_cloudflare_status().await;
        let WebSocketResponse::CloudflareStatus(status) = response else {
            panic!("expected CloudflareStatus");
        };
        assert!(status.configured);
        assert_eq!(status.tunnel_id.as_deref(), Some("tid-123"));
        assert_eq!(status.tunnel_name.as_deref(), Some("myapp"));
    }

    // --- handle_replay_request ---

    #[tokio::test]
    async fn test_handle_replay_request_tunnel_not_found() {
        let server = make_web_server();
        let req = ReplayRequestPayload {
            tunnel_name: "nonexistent".to_string(),
            method: "GET".to_string(),
            url: "/api".to_string(),
            headers: HashMap::new(),
            body: String::new(),
        };
        let response = server.handle_replay_request(req).await;
        let WebSocketResponse::ReplayResponse(payload) = response else {
            panic!("expected ReplayResponse");
        };
        assert!(payload.id.is_none());
        assert!(payload.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn test_handle_replay_request_invalid_base64_body() {
        let server = make_web_server();
        let req = ReplayRequestPayload {
            tunnel_name: "tunnel-a".to_string(),
            method: "POST".to_string(),
            url: "/api".to_string(),
            headers: HashMap::new(),
            body: "not valid base64!!!".to_string(),
        };
        let response = server.handle_replay_request(req).await;
        let WebSocketResponse::ReplayResponse(payload) = response else {
            panic!("expected ReplayResponse");
        };
        assert!(payload.id.is_none());
        assert!(payload.error.unwrap().contains("Invalid base64"));
    }

    #[tokio::test]
    async fn test_handle_replay_request_invalid_method() {
        let server = make_web_server();
        let req = ReplayRequestPayload {
            tunnel_name: "tunnel-a".to_string(),
            method: "NOTAMETHOD\x00".to_string(),
            url: "/api".to_string(),
            headers: HashMap::new(),
            body: String::new(),
        };
        let response = server.handle_replay_request(req).await;
        let WebSocketResponse::ReplayResponse(payload) = response else {
            panic!("expected ReplayResponse");
        };
        assert!(payload.id.is_none());
        assert!(payload.error.is_some());
    }

    #[test]
    fn test_replay_request_payload_serialization() {
        let payload = ReplayRequestPayload {
            tunnel_name: "t".to_string(),
            method: "POST".to_string(),
            url: "/submit".to_string(),
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: "e30=".to_string(), // base64("{}")
        };
        let msg = WebSocketMessage::ReplayRequest(payload);
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"ReplayRequest\""));
        assert!(json.contains("\"method\":\"POST\""));
    }

    #[test]
    fn test_replay_response_payload_serialization_success() {
        let payload = ReplayResponsePayload {
            id: Some("abc-123".to_string()),
            error: None,
        };
        let resp = WebSocketResponse::ReplayResponse(payload);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"ReplayResponse\""));
        assert!(json.contains("\"id\":\"abc-123\""));
    }

    #[test]
    fn test_replay_response_payload_serialization_error() {
        let payload = ReplayResponsePayload {
            id: None,
            error: Some("Tunnel 'foo' not found".to_string()),
        };
        let resp = WebSocketResponse::ReplayResponse(payload);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"id\":null"));
        assert!(json.contains("Tunnel 'foo' not found"));
    }

    // --- handle_sync_tunnels without cloudflare ---

    #[tokio::test]
    async fn test_handle_sync_tunnels_without_cloudflare_returns_error() {
        let server = make_web_server();
        let response = server.handle_sync_tunnels().await;
        assert!(matches!(response, WebSocketResponse::Error(_)));
    }

    // --- QueryFilter::matches (via storage) ---

    #[test]
    fn test_query_filter_matches_no_criteria_always_true() {
        let req = make_stored_request("r1", "tunnel-a", "GET", "/");
        let exchange = crate::storage::RequestExchange {
            request: req,
            response: None,
        };
        let filter = QueryFilter::default();
        assert!(filter.matches(&exchange));
    }

    #[test]
    fn test_query_filter_matches_tunnel_name() {
        let req = make_stored_request("r1", "tunnel-a", "GET", "/");
        let exchange = crate::storage::RequestExchange {
            request: req,
            response: None,
        };

        let matching = QueryFilter {
            tunnel_name: Some("tunnel-a".to_string()),
            ..Default::default()
        };
        assert!(matching.matches(&exchange));

        let non_matching = QueryFilter {
            tunnel_name: Some("tunnel-b".to_string()),
            ..Default::default()
        };
        assert!(!non_matching.matches(&exchange));
    }

    #[test]
    fn test_query_filter_matches_method_case_insensitive() {
        let req = make_stored_request("r1", "tunnel-a", "GET", "/");
        let exchange = crate::storage::RequestExchange {
            request: req,
            response: None,
        };

        let filter = QueryFilter {
            method: Some("get".to_string()),
            ..Default::default()
        };
        assert!(filter.matches(&exchange));
    }

    #[test]
    fn test_query_filter_matches_url_contains() {
        let req = make_stored_request("r1", "tunnel-a", "GET", "/api/users");
        let exchange = crate::storage::RequestExchange {
            request: req,
            response: None,
        };

        let matching = QueryFilter {
            url_contains: Some("/api/".to_string()),
            ..Default::default()
        };
        assert!(matching.matches(&exchange));

        let non_matching = QueryFilter {
            url_contains: Some("/other/".to_string()),
            ..Default::default()
        };
        assert!(!non_matching.matches(&exchange));
    }

    #[test]
    fn test_query_filter_matches_status_requires_response() {
        let req = make_stored_request("r1", "tunnel-a", "GET", "/");
        let exchange_no_resp = crate::storage::RequestExchange {
            request: req.clone(),
            response: None,
        };
        let filter = QueryFilter {
            status: Some(StatusFilter::Exact(200)),
            ..Default::default()
        };
        assert!(!filter.matches(&exchange_no_resp));

        let exchange_with_resp = crate::storage::RequestExchange {
            request: req,
            response: Some(make_stored_response("r1", 200)),
        };
        assert!(filter.matches(&exchange_with_resp));
    }

    #[test]
    fn test_query_filter_matches_time_range() {
        use chrono::Duration;
        let now = chrono::Utc::now();
        let mut req = make_stored_request("r1", "tunnel-a", "GET", "/");
        req.timestamp = now;

        let exchange = crate::storage::RequestExchange {
            request: req,
            response: None,
        };

        let in_range = QueryFilter {
            since: Some(now - Duration::seconds(1)),
            until: Some(now + Duration::seconds(1)),
            ..Default::default()
        };
        assert!(in_range.matches(&exchange));

        let too_late = QueryFilter {
            since: Some(now + Duration::seconds(1)),
            ..Default::default()
        };
        assert!(!too_late.matches(&exchange));

        let too_early = QueryFilter {
            until: Some(now - Duration::seconds(1)),
            ..Default::default()
        };
        assert!(!too_early.matches(&exchange));
    }

    // --- WebSocket message JSON serialization ---

    #[test]
    fn test_ws_message_create_tunnel_serialization() {
        let msg = WebSocketMessage::CreateTunnel(CreateTunnelRequest {
            name: "my-tunnel".to_string(),
            domain: "my.example.com".to_string(),
            socket_path: None,
            target_port: 8080,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"CreateTunnel\""));
        assert!(json.contains("\"name\":\"my-tunnel\""));
    }

    #[test]
    fn test_ws_message_update_tunnel_deserialization() {
        let json = r#"{"type":"UpdateTunnel","data":{"name":"t","enabled":false}}"#;
        let msg: WebSocketMessage = serde_json::from_str(json).unwrap();
        let WebSocketMessage::UpdateTunnel(req) = msg else {
            panic!("expected UpdateTunnel");
        };
        assert_eq!(req.name, "t");
        assert_eq!(req.enabled, Some(false));
        assert!(req.domain.is_none());
    }

    #[test]
    fn test_tunnel_info_enabled_field() {
        let info = TunnelInfo {
            name: "t".to_string(),
            domain: "t.example.com".to_string(),
            socket_path: "/tmp/t.sock".to_string(),
            destination: 3000,
            enabled: false,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["enabled"], false);
    }
}
