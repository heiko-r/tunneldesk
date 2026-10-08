use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine as _;
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::app_service::{
    AppService, CloudflareStatusResponse, ConfirmRemoveHostsRequest, CreateTunnelRequest,
    DeleteTunnelRequest, ReplayRequestPayload, ReplayResponsePayload, RequestExchangeWithBase64,
    StoredWebSocketMessageWithBase64, SyncReportResponse, TunnelDeletedResponse, TunnelEvent,
    TunnelInfo, UnknownHostsFoundResponse, UpdateTunnelRequest, exchange_to_base64,
    websocket_message_to_base64,
};
use crate::core::ClientRegistry;
use crate::security::{Decision, HEALTH_PATH, LOGIN_PAGE, SecurityPolicy};
use crate::storage::{QueryFilter, RequestExchange, RequestUpdate, WebSocketMessageFilter};

/// Commands sent by the browser over the GUI WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WebSocketMessage {
    // --- Query / subscribe ---
    ListTunnels,
    QueryRequests(QueryFilter),
    QueryWebSocketMessages(WebSocketMessageFilter),
    /// Fetches one exchange by request ID, e.g. to resync a streaming body.
    GetRequest(String),
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
    // --- Request management ---
    ClearRequests(String),
    // --- Core lifecycle ---
    GetCoreStatus,
    /// Stops the tunnels and the core for all attached frontends.
    ShutdownCore,
}

/// Responses sent by the server over the GUI WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WebSocketResponse {
    // --- Query responses ---
    Tunnels(Vec<TunnelInfo>),
    Requests(Vec<RequestExchangeWithBase64>),
    WebSocketMessages(Vec<StoredWebSocketMessageWithBase64>),
    /// A single exchange, answering [`WebSocketMessage::GetRequest`].
    Request(Box<RequestExchangeWithBase64>),
    /// Push notification for an exchange whose response was stored or
    /// finished streaming.
    NewRequest(Box<RequestExchangeWithBase64>),
    /// Push notification for body bytes appended to a streaming response.
    ResponseBodyAppended(ResponseBodyAppended),
    /// Push notification for a newly stored WebSocket frame.
    NewWebSocketMessage(Box<StoredWebSocketMessageWithBase64>),
    // --- CRUD responses (also pushed to every client on change) ---
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
    // --- Core lifecycle ---
    CoreStatus(CoreStatusResponse),
    ShuttingDown,
    Error(String),
}

/// Body bytes appended to a streaming response.  `data` (base64) continues the
/// body at byte `offset`; clients holding a shorter body have missed data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseBodyAppended {
    pub request_id: String,
    pub tunnel_name: String,
    pub offset: usize,
    pub data: String,
}

impl From<TunnelEvent> for WebSocketResponse {
    fn from(event: TunnelEvent) -> Self {
        match event {
            TunnelEvent::Created(info) => Self::TunnelCreated(info),
            TunnelEvent::Updated(info) => Self::TunnelUpdated(info),
            TunnelEvent::Deleted(resp) => Self::TunnelDeleted(resp),
        }
    }
}

/// Information about the core process, pushed whenever attached clients change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreStatusResponse {
    /// Number of attached frontends (UI connections and MCP bridges).
    pub attached_clients: usize,
    pub pid: u32,
    pub port: u16,
    pub config_path: String,
    /// Seconds until an unused core exits; `None` for a foreground core.
    pub idle_timeout_secs: Option<u64>,
}

/// Unauthenticated identity check used by frontends to find their core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub config_hash: String,
    pub version: String,
    pub pid: u32,
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

/// Everything the HTTP endpoint needs to know about the core it belongs to.
#[derive(Clone)]
pub struct ServerContext {
    /// The port actually bound (resolved when `gui.port = 0`).
    pub bound_port: u16,
    pub access_token: String,
    pub allowed_origins: Vec<String>,
    pub config_path: PathBuf,
    pub config_hash: String,
    /// Cancelled to request a core shutdown.
    pub shutdown: CancellationToken,
    pub clients: Arc<ClientRegistry>,
    /// Idle timeout of a background core; `None` for a foreground core.
    pub idle_timeout: Option<Duration>,
}

impl ServerContext {
    /// A context not tied to a running core, for handler tests.
    #[cfg(test)]
    pub fn standalone() -> Self {
        Self {
            bound_port: 0,
            access_token: crate::config::generate_access_token(),
            allowed_origins: Vec::new(),
            config_path: PathBuf::new(),
            config_hash: String::new(),
            shutdown: CancellationToken::new(),
            clients: ClientRegistry::new(),
            idle_timeout: None,
        }
    }
}

/// Subscription state of a single GUI WebSocket connection.
#[derive(Debug, Default)]
struct ConnectionState {
    filter: Option<QueryFilter>,
}

impl ConnectionState {
    fn accepts(&self, exchange: &RequestExchange) -> bool {
        self.filter.as_ref().is_none_or(|f| f.matches(exchange))
    }

    /// Converts a storage update into the push notification for this
    /// connection, if it is subscribed to it.  Body appends are filtered by
    /// tunnel only; clients ignore appends for requests they do not show.
    fn notification(&self, update: &RequestUpdate) -> Option<WebSocketResponse> {
        match update {
            RequestUpdate::Exchange(exchange) => self
                .accepts(exchange)
                .then(|| WebSocketResponse::NewRequest(Box::new(exchange_to_base64(exchange)))),
            RequestUpdate::ResponseBodyAppended {
                tunnel_name,
                request_id,
                offset,
                data,
            } => self
                .filter
                .as_ref()
                .is_none_or(|f| f.tunnel_name.as_ref().is_none_or(|t| t == tunnel_name))
                .then(|| {
                    WebSocketResponse::ResponseBodyAppended(ResponseBodyAppended {
                        request_id: request_id.clone(),
                        tunnel_name: tunnel_name.clone(),
                        offset: *offset,
                        data: base64::engine::general_purpose::STANDARD.encode(data),
                    })
                }),
        }
    }
}

/// Serves the static web UI, the GUI WebSocket, the control API and MCP.
#[derive(Clone)]
pub struct WebServer {
    pub(crate) app_service: Arc<AppService>,
    context: ServerContext,
    policy: Arc<SecurityPolicy>,
}

impl WebServer {
    /// Creates a `WebServer` with a standalone context (not bound to a core).
    #[cfg(test)]
    pub fn new(app_service: Arc<AppService>) -> Self {
        Self::with_context(app_service, ServerContext::standalone())
    }

    pub fn with_context(app_service: Arc<AppService>, context: ServerContext) -> Self {
        let policy = Arc::new(SecurityPolicy::new(
            context.bound_port,
            context.access_token.clone(),
            &context.allowed_origins,
        ));
        Self {
            app_service,
            context,
            policy,
        }
    }

    /// Binds `127.0.0.1:port` and returns the listener with the actual port,
    /// which differs from `port` when `port` is `0`.
    pub async fn bind(port: u16) -> anyhow::Result<(TcpListener, u16)> {
        use anyhow::Context as _;
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("failed to bind 127.0.0.1:{port}"))?;
        let bound = listener.local_addr()?.port();
        Ok((listener, bound))
    }

    /// Builds the router with the security layer in front of every route.
    pub fn router(&self) -> Router {
        let state = Arc::new(self.clone());
        let router = Router::new()
            .route("/ws", get(websocket_handler))
            .route(HEALTH_PATH, get(health_handler))
            .route("/api/attach", get(attach_handler))
            .route("/api/shutdown", post(shutdown_handler));
        #[cfg(feature = "mcp")]
        let router = router.nest_service(
            "/mcp",
            crate::mcp::http_service(
                self.app_service.clone(),
                self.context.shutdown.child_token(),
            ),
        );
        router
            .fallback(serve_frontend)
            .layer(middleware::from_fn_with_state(state.clone(), guard))
            .with_state(state)
    }

    /// Serves until the context's shutdown token is cancelled.
    pub async fn serve(self, listener: TcpListener) -> anyhow::Result<()> {
        let shutdown = self.context.shutdown.clone();
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await?;
        Ok(())
    }

    fn core_status(&self) -> CoreStatusResponse {
        CoreStatusResponse {
            attached_clients: self.context.clients.count(),
            pid: std::process::id(),
            port: self.context.bound_port,
            config_path: self.context.config_path.to_string_lossy().into_owned(),
            idle_timeout_secs: self.context.idle_timeout.map(|d| d.as_secs()),
        }
    }

    async fn handle_message(
        &self,
        message: WebSocketMessage,
        conn: &mut ConnectionState,
    ) -> WebSocketResponse {
        match message {
            WebSocketMessage::ListTunnels => self.handle_list_tunnels().await,
            WebSocketMessage::QueryRequests(filter) => self.handle_query_requests(&filter).await,
            WebSocketMessage::QueryWebSocketMessages(filter) => {
                self.handle_query_websocket_messages(&filter).await
            }
            WebSocketMessage::GetRequest(id) => self.handle_get_request(&id).await,
            WebSocketMessage::Subscribe(filter) => {
                conn.filter = Some(filter);
                WebSocketResponse::Requests(vec![])
            }
            WebSocketMessage::Unsubscribe => {
                conn.filter = None;
                WebSocketResponse::Requests(vec![])
            }
            WebSocketMessage::CreateTunnel(req) => self.handle_create_tunnel(req).await,
            WebSocketMessage::UpdateTunnel(req) => self.handle_update_tunnel(req).await,
            WebSocketMessage::DeleteTunnel(req) => self.handle_delete_tunnel(req).await,
            WebSocketMessage::SyncTunnels => self.handle_sync_tunnels().await,
            WebSocketMessage::ConfirmRemoveHosts(req) => {
                self.handle_confirm_remove_hosts(req).await
            }
            WebSocketMessage::GetCloudflareStatus => self.handle_get_cloudflare_status().await,
            WebSocketMessage::ReplayRequest(req) => self.handle_replay_request(req).await,
            WebSocketMessage::ClearRequests(tunnel_name) => {
                self.handle_clear_requests(tunnel_name).await
            }
            WebSocketMessage::GetCoreStatus => WebSocketResponse::CoreStatus(self.core_status()),
            WebSocketMessage::ShutdownCore => {
                tracing::info!("Shutdown requested from the UI");
                self.context.shutdown.cancel();
                WebSocketResponse::ShuttingDown
            }
        }
    }

    // ── Query handlers ────────────────────────────────────────────────────────

    async fn handle_list_tunnels(&self) -> WebSocketResponse {
        let cfg = self.app_service.config.read().await;
        let tunnels = cfg
            .tunnels
            .iter()
            .map(crate::app_service::tunnel_info_from_config)
            .collect();
        WebSocketResponse::Tunnels(tunnels)
    }

    async fn handle_query_requests(&self, filter: &QueryFilter) -> WebSocketResponse {
        let requests = self
            .app_service
            .request_storage
            .query_requests(filter)
            .await;
        let requests_with_base64: Vec<RequestExchangeWithBase64> =
            requests.iter().map(exchange_to_base64).collect();
        WebSocketResponse::Requests(requests_with_base64)
    }

    async fn handle_get_request(&self, id: &str) -> WebSocketResponse {
        match self.app_service.request_storage.get_request_by_id(id).await {
            Some(exchange) => WebSocketResponse::Request(Box::new(exchange_to_base64(&exchange))),
            None => WebSocketResponse::Error(format!("Request {id} not found")),
        }
    }

    async fn handle_query_websocket_messages(
        &self,
        filter: &WebSocketMessageFilter,
    ) -> WebSocketResponse {
        let messages = self
            .app_service
            .websocket_storage
            .query_messages(filter)
            .await;
        let messages_with_base64: Vec<StoredWebSocketMessageWithBase64> =
            messages.iter().map(websocket_message_to_base64).collect();
        WebSocketResponse::WebSocketMessages(messages_with_base64)
    }

    // ── CRUD handlers ─────────────────────────────────────────────────────────

    pub(crate) async fn handle_create_tunnel(&self, req: CreateTunnelRequest) -> WebSocketResponse {
        match self.app_service.create_tunnel(req).await {
            Ok(info) => WebSocketResponse::TunnelCreated(info),
            Err(e) => WebSocketResponse::Error(e),
        }
    }

    pub(crate) async fn handle_update_tunnel(&self, req: UpdateTunnelRequest) -> WebSocketResponse {
        match self.app_service.update_tunnel(req).await {
            Ok(info) => WebSocketResponse::TunnelUpdated(info),
            Err(e) => WebSocketResponse::Error(e),
        }
    }

    pub(crate) async fn handle_delete_tunnel(&self, req: DeleteTunnelRequest) -> WebSocketResponse {
        match self.app_service.delete_tunnel(req).await {
            Ok(resp) => WebSocketResponse::TunnelDeleted(resp),
            Err(e) => WebSocketResponse::Error(e),
        }
    }

    // ── Cloudflare management handlers ───────────────────────────────────────

    async fn handle_sync_tunnels(&self) -> WebSocketResponse {
        match self.app_service.sync_tunnels().await {
            Ok(resp) => WebSocketResponse::SyncReport(resp),
            Err(e) => WebSocketResponse::Error(e),
        }
    }

    async fn handle_confirm_remove_hosts(
        &self,
        req: ConfirmRemoveHostsRequest,
    ) -> WebSocketResponse {
        match self.app_service.confirm_remove_hosts(req).await {
            Ok(resp) => WebSocketResponse::SyncReport(resp),
            Err(e) => WebSocketResponse::Error(e),
        }
    }

    // ── Replay handler ────────────────────────────────────────────────────────

    pub(crate) async fn handle_replay_request(
        &self,
        req: ReplayRequestPayload,
    ) -> WebSocketResponse {
        match self.app_service.replay_request(req).await {
            Ok(resp) => WebSocketResponse::ReplayResponse(resp),
            Err(e) => WebSocketResponse::Error(e),
        }
    }

    async fn handle_get_cloudflare_status(&self) -> WebSocketResponse {
        let status = self.app_service.get_cloudflare_status().await;
        WebSocketResponse::CloudflareStatus(status)
    }

    async fn handle_clear_requests(&self, tunnel_name: String) -> WebSocketResponse {
        self.app_service
            .clear_requests_for_tunnel(&tunnel_name)
            .await;
        WebSocketResponse::Requests(vec![])
    }
}

/// Applies the [`SecurityPolicy`] to every request and records activity.
async fn guard(State(server): State<Arc<WebServer>>, request: Request, next: Next) -> Response {
    match server
        .policy
        .evaluate(request.method(), request.uri(), request.headers())
    {
        Decision::Allow { authenticated } => {
            if authenticated {
                server.context.clients.touch();
            }
            next.run(request).await
        }
        Decision::SetCookieAndEnter { cookie, location } => (
            StatusCode::OK,
            [
                (header::SET_COOKIE, cookie),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            // A script navigation (instead of an HTTP redirect) starts on this
            // origin, so the browser attaches the SameSite=Strict cookie.
            Html(format!(
                "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>TunnelDesk</title>\
                 <script>location.replace({})</script></head><body></body></html>",
                serde_json::to_string(&location).unwrap_or_else(|_| "\"/\"".to_string())
            )),
        )
            .into_response(),
        Decision::LoginPage => (StatusCode::UNAUTHORIZED, Html(LOGIN_PAGE)).into_response(),
        Decision::Reject(status, message) => (status, message).into_response(),
    }
}

async fn health_handler(State(server): State<Arc<WebServer>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        config_hash: server.context.config_hash.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
    })
}

async fn shutdown_handler(State(server): State<Arc<WebServer>>) -> impl IntoResponse {
    tracing::info!("Shutdown requested via API");
    server.context.shutdown.cancel();
    (StatusCode::ACCEPTED, "Shutting down")
}

/// Presence connection held by frontends without a UI WebSocket (MCP bridge).
async fn attach_handler(ws: WebSocketUpgrade, State(server): State<Arc<WebServer>>) -> Response {
    ws.on_upgrade(move |mut socket| async move {
        let _guard = server.context.clients.attach();
        let shutdown = server.context.shutdown.clone();
        loop {
            tokio::select! {
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                _ = shutdown.cancelled() => {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    })
}

async fn websocket_handler(ws: WebSocketUpgrade, State(server): State<Arc<WebServer>>) -> Response {
    ws.on_upgrade(|socket| websocket_connection(socket, server))
}

async fn send_response(socket: &mut WebSocket, response: &WebSocketResponse) -> bool {
    match serde_json::to_string(response) {
        Ok(text) => socket.send(Message::Text(text)).await.is_ok(),
        Err(_) => true,
    }
}

async fn websocket_connection(mut socket: WebSocket, server: Arc<WebServer>) {
    let _guard = server.context.clients.attach();
    let mut conn = ConnectionState::default();
    let mut request_receiver = server.app_service.request_storage.subscribe_requests();
    let mut ws_message_receiver = server.app_service.websocket_storage.subscribe_messages();
    let mut events = server.app_service.subscribe_events();
    let mut connector = server.app_service.subscribe_connector();
    let mut clients = server.context.clients.subscribe();
    let shutdown = server.context.shutdown.clone();

    loop {
        let response = tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<WebSocketMessage>(&text) {
                        Ok(message) => Some(server.handle_message(message, &mut conn).await),
                        Err(_) => {
                            tracing::warn!("Could not parse WebSocket message: {text}");
                            None
                        }
                    }
                }
                Some(Ok(Message::Binary(binary))) => {
                    tracing::warn!("Received binary message: {:?}", binary);
                    None
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => None,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            },
            Ok(update) = request_receiver.recv() => conn.notification(&update),
            Ok(ws_msg) = ws_message_receiver.recv() => Some(
                WebSocketResponse::NewWebSocketMessage(Box::new(websocket_message_to_base64(&ws_msg)))
            ),
            Ok(event) = events.recv() => Some(event.into()),
            Ok(()) = connector.changed() => Some(WebSocketResponse::CloudflareStatus(
                server.app_service.get_cloudflare_status().await,
            )),
            Ok(()) = clients.changed() => Some(WebSocketResponse::CoreStatus(server.core_status())),
            _ = shutdown.cancelled() => {
                let _ = send_response(&mut socket, &WebSocketResponse::ShuttingDown).await;
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
        };

        if let Some(response) = response
            && !send_response(&mut socket, &response).await
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_service::{
        AppService, exchange_to_base64, get_default_socket_path, request_to_base64,
        response_to_base64, websocket_message_to_base64,
    };
    use crate::config::{CaptureConfig, Config, GuiConfig, LoggingConfig, TunnelConfig};
    use crate::storage::{
        RequestStorage, StatusFilter, StoredRequest, StoredResponse, StoredWebSocketMessage,
        WebSocketMessageStorage, WebSocketMessageType,
    };
    use crate::tunnel::TunnelManager;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

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
            gui: GuiConfig::with_port(8080),
            core: Default::default(),
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
        let app_service = Arc::new(AppService::new(config, tm, req_storage, ws_storage));
        WebServer::new(app_service)
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
            streaming: false,
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
        let app_service = Arc::new(AppService::new(config, tm, req_storage.clone(), ws_storage));
        let server = WebServer::new(app_service);

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
    async fn test_handle_clear_requests() {
        let config = Arc::new(RwLock::new(make_config()));
        let req_storage = Arc::new(RequestStorage::new(100));
        let ws_storage = Arc::new(WebSocketMessageStorage::new(100));
        let tm = Arc::new(TunnelManager::new(
            &make_config(),
            req_storage.clone(),
            ws_storage.clone(),
        ));
        let app_service = Arc::new(AppService::new(config, tm, req_storage.clone(), ws_storage));
        let server = WebServer::new(app_service);

        // Store requests for different tunnels
        let req1 = make_stored_request("r1", "tunnel-a", "GET", "/api");
        let req2 = make_stored_request("r2", "tunnel-b", "POST", "/submit");
        let req3 = make_stored_request("r3", "tunnel-a", "PUT", "/update");

        req_storage.store_request(req1).await;
        req_storage.store_request(req2).await;
        req_storage.store_request(req3).await;

        assert_eq!(req_storage.get_count().await, 3);

        // Clear requests for tunnel-a
        let response = server.handle_clear_requests("tunnel-a".to_string()).await;

        let WebSocketResponse::Requests(exchanges) = response else {
            panic!("expected Requests variant");
        };
        assert_eq!(exchanges.len(), 0);

        // Verify only tunnel-b requests remain
        assert_eq!(req_storage.get_count().await, 1);
        assert!(req_storage.get_request_by_id("r1").await.is_none());
        assert!(req_storage.get_request_by_id("r3").await.is_none());
        assert!(req_storage.get_request_by_id("r2").await.is_some());
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
        let cfg = server.app_service.config.read().await;
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

        let cfg = server.app_service.config.read().await;
        let t = cfg.tunnels.iter().find(|t| t.name == "auto-path").unwrap();
        assert!(t.socket_path.contains("tunneldesk-auto-path"));
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

        let cfg = server.app_service.config.read().await;
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
        let app_service = Arc::new(AppService::new(config, tm.clone(), req_storage, ws_storage));
        let server = WebServer::new(app_service);

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
        assert!(!info.enabled);

        let cfg = server.app_service.config.read().await;
        let t = cfg.tunnels.iter().find(|t| t.name == "tunnel-a").unwrap();
        assert!(!t.enabled);

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

        let cfg = server.app_service.config.read().await;
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
        assert_eq!(
            status.connector,
            crate::cloudflared::ConnectorState::Stopped
        );
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
            manage_cloudflared: true,
        });

        let config = Arc::new(RwLock::new(cfg.clone()));
        let req_storage = Arc::new(RequestStorage::new(100));
        let ws_storage = Arc::new(WebSocketMessageStorage::new(100));
        let tm = Arc::new(TunnelManager::new(
            &cfg,
            req_storage.clone(),
            ws_storage.clone(),
        ));
        let app_service = Arc::new(AppService::new(config, tm, req_storage, ws_storage));
        let server = WebServer::new(app_service);

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

    // --- get_default_socket_path ---

    #[test]
    fn test_get_default_socket_path_generates_valid_path() {
        let tunnel_name = "test-tunnel";
        let path = get_default_socket_path(tunnel_name);

        // Check that the path contains the tunnel name
        assert!(path.contains(tunnel_name));

        // Check that it has the expected format
        assert!(path.contains("tunneldesk-"));
        assert!(path.contains(&format!("-{}-", tunnel_name)));

        // Check that it ends with .sock
        assert!(path.ends_with(".sock"));

        // Check that the suffix is 8 characters long (alphanumeric)
        let parts: Vec<&str> = path.split('-').collect();
        assert_eq!(parts.len(), 4); // ["tunneldesk", "test", "tunnel", "ABCDEF.sock"]
    }

    #[test]
    fn test_get_default_socket_path_unique_calls() {
        let tunnel_name = "unique-tunnel";

        // Generate multiple paths and ensure they're different
        let path1 = get_default_socket_path(tunnel_name);
        let path2 = get_default_socket_path(tunnel_name);
        let path3 = get_default_socket_path(tunnel_name);

        // All paths should be different
        assert_ne!(path1, path2);
        assert_ne!(path2, path3);
        assert_ne!(path1, path3);

        // But they should all contain the tunnel name
        assert!(path1.contains(tunnel_name));
        assert!(path2.contains(tunnel_name));
        assert!(path3.contains(tunnel_name));
    }

    #[test]
    fn test_get_default_socket_path_different_tunnel_names() {
        let path1 = get_default_socket_path("tunnel-a");
        let path2 = get_default_socket_path("tunnel-b");

        // Paths should be different
        assert_ne!(path1, path2);

        // Each should contain its respective tunnel name
        assert!(path1.contains("tunnel-a"));
        assert!(path2.contains("tunnel-b"));

        // Neither should contain the other's tunnel name
        assert!(!path1.contains("tunnel-b"));
        assert!(!path2.contains("tunnel-a"));
    }

    #[test]
    fn test_get_default_socket_path_special_characters() {
        let tunnel_name = "tunnel_with_123";
        let path = get_default_socket_path(tunnel_name);

        // Should handle special characters in tunnel names
        assert!(path.contains(tunnel_name));
        assert!(path.ends_with(".sock"));
    }
}

/// Tests against a real listening server: multiple clients, pushes, the
/// control API and the security layer as wired into the router.
#[cfg(test)]
mod server_tests {
    use super::*;
    use crate::config::Config;
    use crate::storage::{RequestStorage, StoredRequest, StoredResponse, WebSocketMessageStorage};
    use crate::tunnel::TunnelManager;
    use axum::body::Body;
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio::sync::RwLock;
    use tokio_tungstenite::tungstenite;
    use tower::ServiceExt as _;

    const TOKEN: &str = "test-token";

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    struct TestServer {
        port: u16,
        app: Arc<AppService>,
        context: ServerContext,
        _dir: tempfile::TempDir,
    }

    fn make_app() -> Arc<AppService> {
        let config = Config::default_config();
        let requests = Arc::new(RequestStorage::new(100));
        let messages = Arc::new(WebSocketMessageStorage::new(100));
        let tunnels = Arc::new(TunnelManager::new(
            &config,
            requests.clone(),
            messages.clone(),
        ));
        Arc::new(AppService::new(
            Arc::new(RwLock::new(config)),
            tunnels,
            requests,
            messages,
        ))
    }

    fn make_context(port: u16) -> ServerContext {
        ServerContext {
            bound_port: port,
            access_token: TOKEN.to_string(),
            allowed_origins: vec!["http://localhost:5173".to_string()],
            config_path: PathBuf::from("/tmp/tunneldesk-test.toml"),
            config_hash: "0123456789abcdef".to_string(),
            shutdown: CancellationToken::new(),
            clients: ClientRegistry::new(),
            idle_timeout: None,
        }
    }

    async fn start_server() -> TestServer {
        let (listener, port) = WebServer::bind(0).await.unwrap();
        let app = make_app();
        let context = make_context(port);
        let server = WebServer::with_context(app.clone(), context.clone());
        tokio::spawn(async move { server.serve(listener).await.unwrap() });
        TestServer {
            port,
            app,
            context,
            _dir: tempfile::tempdir().unwrap(),
        }
    }

    async fn connect(port: u16) -> Ws {
        let url = format!("ws://127.0.0.1:{port}/ws?token={TOKEN}");
        tokio_tungstenite::connect_async(url).await.unwrap().0
    }

    async fn send(ws: &mut Ws, message: &WebSocketMessage) {
        let text = serde_json::to_string(message).unwrap();
        ws.send(tungstenite::Message::Text(text)).await.unwrap();
    }

    /// Returns the next response matching `pred`, skipping unrelated pushes.
    async fn next_matching(
        ws: &mut Ws,
        pred: impl Fn(&WebSocketResponse) -> bool,
    ) -> Option<WebSocketResponse> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
        loop {
            let msg = tokio::time::timeout_at(deadline, ws.next()).await.ok()??;
            if let Ok(tungstenite::Message::Text(text)) = msg {
                let response: WebSocketResponse = serde_json::from_str(&text).unwrap();
                if pred(&response) {
                    return Some(response);
                }
            }
        }
    }

    fn exchange(id: &str, tunnel: &str) -> crate::storage::RequestExchange {
        crate::storage::RequestExchange {
            request: StoredRequest {
                id: id.to_string(),
                timestamp: chrono::Utc::now(),
                tunnel_name: tunnel.to_string(),
                method: "GET".to_string(),
                url: "/".to_string(),
                headers: Default::default(),
                body: vec![],
                raw_request: vec![],
                replayed: false,
            },
            response: None,
        }
    }

    async fn wait_for_clients(context: &ServerContext, expected: usize) {
        let mut rx = context.clients.subscribe();
        tokio::time::timeout(Duration::from_secs(2), rx.wait_for(|c| *c == expected))
            .await
            .unwrap_or_else(|_| panic!("expected {expected} attached clients"))
            .unwrap();
    }

    #[tokio::test]
    async fn subscriptions_are_per_connection() {
        let server = start_server().await;
        let mut a = connect(server.port).await;
        let mut b = connect(server.port).await;
        for (ws, tunnel) in [(&mut a, "tunnel-a"), (&mut b, "tunnel-b")] {
            send(
                ws,
                &WebSocketMessage::Subscribe(QueryFilter {
                    tunnel_name: Some(tunnel.to_string()),
                    ..Default::default()
                }),
            )
            .await;
            next_matching(ws, |r| matches!(r, WebSocketResponse::Requests(_)))
                .await
                .expect("subscribe acknowledgement");
        }

        server
            .app
            .request_storage
            .store_exchange(exchange("req-a", "tunnel-a"))
            .await;

        let got = next_matching(&mut a, |r| matches!(r, WebSocketResponse::NewRequest(_))).await;
        assert!(matches!(got, Some(WebSocketResponse::NewRequest(e)) if e.request.id == "req-a"));
        let leaked = next_matching(&mut b, |r| matches!(r, WebSocketResponse::NewRequest(_))).await;
        assert!(leaked.is_none(), "client B must not see tunnel-a requests");
    }

    async fn subscribe(ws: &mut Ws, tunnel: &str) {
        send(
            ws,
            &WebSocketMessage::Subscribe(QueryFilter {
                tunnel_name: Some(tunnel.to_string()),
                ..Default::default()
            }),
        )
        .await;
        next_matching(ws, |r| matches!(r, WebSocketResponse::Requests(_)))
            .await
            .expect("subscribe acknowledgement");
    }

    #[tokio::test]
    async fn streaming_bodies_are_pushed_to_subscribers_of_their_tunnel() {
        let server = start_server().await;
        let mut a = connect(server.port).await;
        let mut b = connect(server.port).await;
        subscribe(&mut a, "tunnel-a").await;
        subscribe(&mut b, "tunnel-b").await;

        let storage = &server.app.request_storage;
        storage.store_exchange(exchange("sse", "tunnel-a")).await;
        storage
            .store_response(StoredResponse {
                request_id: "sse".to_string(),
                timestamp: chrono::Utc::now(),
                status: 200,
                headers: Default::default(),
                body: vec![],
                raw_response: vec![],
                response_time_ms: None,
                streaming: true,
            })
            .await;
        let started = next_matching(
            &mut a,
            |r| matches!(r, WebSocketResponse::NewRequest(e) if e.response.is_some()),
        )
        .await;
        assert!(matches!(
            started,
            Some(WebSocketResponse::NewRequest(e)) if e.response.as_ref().is_some_and(|r| r.streaming)
        ));

        storage
            .append_response_body("sse", b"data: 1\n\n", b"")
            .await;
        let appended = next_matching(&mut a, |r| {
            matches!(r, WebSocketResponse::ResponseBodyAppended(_))
        })
        .await;
        match appended {
            Some(WebSocketResponse::ResponseBodyAppended(update)) => {
                assert_eq!(update.request_id, "sse");
                assert_eq!(update.tunnel_name, "tunnel-a");
                assert_eq!(update.offset, 0);
                assert_eq!(update.data, "ZGF0YTogMQoK");
            }
            other => panic!("expected a body append, got {other:?}"),
        }

        storage.finish_response("sse", b"").await;
        let finished =
            next_matching(&mut a, |r| matches!(r, WebSocketResponse::NewRequest(_))).await;
        assert!(matches!(
            finished,
            Some(WebSocketResponse::NewRequest(e)) if e.response.as_ref().is_some_and(|r| !r.streaming)
        ));

        let leaked = next_matching(&mut b, |r| {
            matches!(
                r,
                WebSocketResponse::NewRequest(_) | WebSocketResponse::ResponseBodyAppended(_)
            )
        })
        .await;
        assert!(leaked.is_none(), "client B must not see tunnel-a updates");
    }

    #[test]
    fn body_appends_reach_unfiltered_connections() {
        let update = RequestUpdate::ResponseBodyAppended {
            tunnel_name: "t".to_string(),
            request_id: "r".to_string(),
            offset: 3,
            data: b"abc".to_vec(),
        };
        let unfiltered = ConnectionState::default();
        assert!(matches!(
            unfiltered.notification(&update),
            Some(WebSocketResponse::ResponseBodyAppended(u)) if u.offset == 3 && u.data == "YWJj"
        ));
        let any_tunnel = ConnectionState {
            filter: Some(QueryFilter::default()),
        };
        assert!(any_tunnel.notification(&update).is_some());
        let other_tunnel = ConnectionState {
            filter: Some(QueryFilter {
                tunnel_name: Some("other".to_string()),
                ..Default::default()
            }),
        };
        assert!(other_tunnel.notification(&update).is_none());
    }

    #[tokio::test]
    async fn get_request_returns_one_exchange() {
        let server = start_server().await;
        let mut ws = connect(server.port).await;
        server
            .app
            .request_storage
            .store_exchange(exchange("one", "tunnel-a"))
            .await;

        send(&mut ws, &WebSocketMessage::GetRequest("one".to_string())).await;
        let found = next_matching(&mut ws, |r| matches!(r, WebSocketResponse::Request(_))).await;
        assert!(matches!(found, Some(WebSocketResponse::Request(e)) if e.request.id == "one"));

        send(&mut ws, &WebSocketMessage::GetRequest("none".to_string())).await;
        let missing = next_matching(&mut ws, |r| matches!(r, WebSocketResponse::Error(_))).await;
        assert!(matches!(missing, Some(WebSocketResponse::Error(e)) if e.contains("none")));
    }

    #[tokio::test]
    async fn tunnel_changes_reach_every_client() {
        let server = start_server().await;
        let mut a = connect(server.port).await;
        let mut b = connect(server.port).await;
        wait_for_clients(&server.context, 2).await;

        // A change made outside the UI (e.g. by an MCP client).
        let socket = server._dir.path().join("pushed.sock");
        server
            .app
            .create_tunnel(crate::app_service::CreateTunnelRequest {
                name: "pushed".into(),
                domain: "pushed.example.com".into(),
                socket_path: Some(socket.to_string_lossy().into_owned()),
                target_port: 4321,
            })
            .await
            .unwrap();

        for ws in [&mut a, &mut b] {
            let got = next_matching(ws, |r| matches!(r, WebSocketResponse::TunnelCreated(_))).await;
            assert!(matches!(got, Some(WebSocketResponse::TunnelCreated(i)) if i.name == "pushed"));
        }
        server.app.tunnel_manager.shutdown().await;
    }

    #[tokio::test]
    async fn connector_changes_are_pushed() {
        let server = start_server().await;
        let mut ws = connect(server.port).await;
        wait_for_clients(&server.context, 1).await;
        server
            .app
            .connector_sender()
            .send_replace(crate::cloudflared::ConnectorState::Connected);
        let got = next_matching(&mut ws, |r| {
            matches!(r, WebSocketResponse::CloudflareStatus(_))
        })
        .await;
        assert!(got.is_some(), "expected a CloudflareStatus push");
    }

    #[tokio::test]
    async fn clients_are_counted_and_released() {
        let server = start_server().await;
        let mut first = connect(server.port).await;
        wait_for_clients(&server.context, 1).await;
        let second = connect(server.port).await;

        let got = next_matching(
            &mut first,
            |r| matches!(r, WebSocketResponse::CoreStatus(s) if s.attached_clients == 2),
        )
        .await;
        assert!(got.is_some(), "expected CoreStatus push with 2 clients");

        drop(second);
        wait_for_clients(&server.context, 1).await;
        send(&mut first, &WebSocketMessage::GetCoreStatus).await;
        let got = next_matching(
            &mut first,
            |r| matches!(r, WebSocketResponse::CoreStatus(s) if s.attached_clients == 1),
        )
        .await;
        let Some(WebSocketResponse::CoreStatus(status)) = got else {
            panic!("expected CoreStatus");
        };
        assert_eq!(status.port, server.port);
        assert_eq!(status.pid, std::process::id());
        assert_eq!(status.idle_timeout_secs, None);
    }

    #[tokio::test]
    async fn shutdown_command_cancels_core_and_notifies_clients() {
        let server = start_server().await;
        let mut requester = connect(server.port).await;
        let mut other = connect(server.port).await;
        wait_for_clients(&server.context, 2).await;

        send(&mut requester, &WebSocketMessage::ShutdownCore).await;
        for ws in [&mut requester, &mut other] {
            let got = next_matching(ws, |r| matches!(r, WebSocketResponse::ShuttingDown)).await;
            assert!(
                got.is_some(),
                "every client must be told about the shutdown"
            );
        }
        assert!(server.context.shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn attach_endpoint_holds_a_client_slot() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
        let server = start_server().await;
        let mut request = format!("ws://127.0.0.1:{}/api/attach", server.port)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {TOKEN}").parse().unwrap());
        let (attach, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        wait_for_clients(&server.context, 1).await;
        drop(attach);
        wait_for_clients(&server.context, 0).await;
    }

    #[tokio::test]
    async fn attach_endpoint_requires_token() {
        let server = start_server().await;
        let result =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/api/attach", server.port))
                .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shutdown_endpoint_requires_token() {
        let server = start_server().await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://127.0.0.1:{}/api/shutdown", server.port);

        let denied = client.post(&url).send().await.unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert!(!server.context.shutdown.is_cancelled());

        let accepted = client.post(&url).bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
        assert!(server.context.shutdown.is_cancelled());
    }

    // ── Security layer via tower oneshot ──────────────────────────────────────

    fn router() -> Router {
        WebServer::with_context(make_app(), make_context(4567)).router()
    }

    fn request(method: &str, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Body::empty()).unwrap()
    }

    const HOST: (&str, &str) = ("host", "127.0.0.1:4567");

    #[tokio::test]
    async fn router_rejects_foreign_host() {
        let resp = router()
            .oneshot(request(
                "GET",
                "/api/health",
                &[("host", "attacker.test:4567")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn router_serves_health_without_token() {
        let resp = router()
            .oneshot(request("GET", "/api/health", &[HOST]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        let health: HealthResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(health.config_hash, "0123456789abcdef");
        assert_eq!(health.pid, std::process::id());
    }

    #[tokio::test]
    async fn router_rejects_foreign_origin_on_ws() {
        let resp = router()
            .oneshot(request(
                "GET",
                "/ws?token=test-token",
                &[HOST, ("origin", "https://evil.example.com")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn router_exchanges_query_token_for_cookie() {
        let resp = router()
            .oneshot(request("GET", "/?token=test-token", &[HOST]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(header::LOCATION).is_none());
        let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(cookie.starts_with("tunneldesk_token_4567=test-token;"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("location.replace(\"/\")"));
    }

    #[tokio::test]
    async fn router_shows_login_page_without_token() {
        let resp = router()
            .oneshot(request("GET", "/", &[HOST]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("tunneldesk open"));
    }

    #[tokio::test]
    async fn router_serves_ui_with_cookie() {
        let resp = router()
            .oneshot(request(
                "GET",
                "/",
                &[HOST, ("cookie", "tunneldesk_token_4567=test-token")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn router_touches_activity_on_authenticated_requests() {
        let context = make_context(4567);
        let router = WebServer::with_context(make_app(), context.clone()).router();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let before = context.clients.idle_for();
        router
            .oneshot(request(
                "GET",
                "/",
                &[HOST, ("authorization", "Bearer test-token")],
            ))
            .await
            .unwrap();
        assert!(context.clients.idle_for() < before);
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn router_protects_mcp_endpoint() {
        let resp = router()
            .oneshot(request(
                "POST",
                "/mcp",
                &[HOST, ("content-type", "application/json")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
