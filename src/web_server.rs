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
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::app_service::{
    AppService, CloudflareStatusResponse, ConfirmRemoveHostsRequest, CreateTunnelRequest,
    DeleteTunnelRequest, ReplayRequestPayload, ReplayResponsePayload, RequestExchangeWithBase64,
    StoredWebSocketMessageWithBase64, SyncReportResponse, TunnelDeletedResponse, TunnelInfo,
    UnknownHostsFoundResponse, UpdateTunnelRequest, exchange_to_base64,
    websocket_message_to_base64,
};
use crate::storage::{QueryFilter, WebSocketMessageFilter};

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
    // --- Request management ---
    ClearRequests(String),
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
    pub(crate) app_service: Arc<AppService>,
    current_filter: Arc<RwLock<Option<QueryFilter>>>,
    current_ws_filter: Arc<RwLock<Option<WebSocketMessageFilter>>>,
}

impl WebServer {
    /// Creates a new `WebServer`.
    pub fn new(app_service: Arc<AppService>) -> Self {
        Self {
            app_service,
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

        let port = self.app_service.config.read().await.gui.port;
        let addr = format!("127.0.0.1:{port}");
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        tracing::info!("Web GUI server listening on http://{}", addr);

        axum::serve(listener, app).await?;
        Ok(())
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

async fn websocket_handler(ws: WebSocketUpgrade, State(server): State<Arc<WebServer>>) -> Response {
    ws.on_upgrade(|socket| websocket_connection(socket, server))
}

async fn websocket_connection(mut socket: axum::extract::ws::WebSocket, server: Arc<WebServer>) {
    // Subscribe to request and WebSocket message broadcasts
    let mut request_receiver = server.app_service.request_storage.subscribe_requests();
    let mut ws_message_receiver = server.app_service.websocket_storage.subscribe_messages();
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
                                    WebSocketMessage::ClearRequests(tunnel_name) => {
                                        server.handle_clear_requests(tunnel_name).await
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
    use base64::Engine as _;
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
        let app_service = Arc::new(AppService::new(config, tm, None, req_storage, ws_storage));
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
        let app_service = Arc::new(AppService::new(
            config,
            tm,
            None,
            req_storage.clone(),
            ws_storage,
        ));
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
        let app_service = Arc::new(AppService::new(
            config,
            tm,
            None,
            req_storage.clone(),
            ws_storage,
        ));
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
        let app_service = Arc::new(AppService::new(
            config,
            tm.clone(),
            None,
            req_storage,
            ws_storage,
        ));
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
        let app_service = Arc::new(AppService::new(config, tm, None, req_storage, ws_storage));
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
