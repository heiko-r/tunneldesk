use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocketUpgrade},
    },
    response::Response,
    routing::get,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::services::{ServeDir, ServeFile};

use crate::config::Config;
use crate::storage::{QueryFilter, WebSocketMessageFilter};

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

/// Commands sent by the browser over the GUI WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WebSocketMessage {
    ListTunnels,
    QueryRequests(QueryFilter),
    QueryWebSocketMessages(WebSocketMessageFilter),
    Subscribe(QueryFilter),
    Unsubscribe,
}

/// Responses sent by the server over the GUI WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WebSocketResponse {
    Tunnels(Vec<TunnelInfo>),
    Requests(Vec<RequestExchangeWithBase64>),
    WebSocketMessages(Vec<StoredWebSocketMessageWithBase64>),
    /// Push notification for a newly completed request–response exchange.
    NewRequest(Box<RequestExchangeWithBase64>),
    /// Push notification for a newly stored WebSocket frame.
    NewWebSocketMessage(Box<StoredWebSocketMessageWithBase64>),
    Error(String),
}

/// Metadata about a configured tunnel, sent to the browser in a [`WebSocketResponse::Tunnels`] message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub name: String,
    pub domain: String,
    pub socket_path: String,
    /// Local TCP port the tunnel forwards to.
    pub destination: u16,
}

/// Serves the static web UI and handles GUI WebSocket connections.
#[derive(Clone)]
pub struct WebServer {
    config: Config,
    request_storage: Arc<crate::storage::RequestStorage>,
    websocket_storage: Arc<crate::storage::WebSocketMessageStorage>,
    current_filter: Arc<RwLock<Option<QueryFilter>>>,
    current_ws_filter: Arc<RwLock<Option<WebSocketMessageFilter>>>,
}

impl WebServer {
    /// Creates a new `WebServer` using `config` and the shared storage instances.
    pub fn new(
        config: Config,
        request_storage: Arc<crate::storage::RequestStorage>,
        websocket_storage: Arc<crate::storage::WebSocketMessageStorage>,
    ) -> Self {
        Self {
            config,
            request_storage,
            websocket_storage,
            current_filter: Arc::new(RwLock::new(None)),
            current_ws_filter: Arc::new(RwLock::new(None)),
        }
    }

    /// Binds to the configured port and serves the web UI and WebSocket API.
    /// Returns an error if the TCP listener cannot be bound.
    pub async fn start(&self) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/ws", get(websocket_handler))
            .fallback_service(
                ServeDir::new("frontend/build")
                    .not_found_service(ServeFile::new("frontend/build/200.html")),
            )
            .with_state(Arc::new(self.clone()));

        let addr = format!("127.0.0.1:{}", self.config.gui.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        tracing::info!("Web GUI server listening on http://{}", addr);

        axum::serve(listener, app).await?;
        Ok(())
    }

    async fn handle_list_tunnels(&self) -> WebSocketResponse {
        let tunnels: Vec<TunnelInfo> = self
            .config
            .tunnels
            .iter()
            .map(|t| TunnelInfo {
                name: t.name.clone(),
                domain: t.domain.clone(),
                socket_path: t.socket_path.clone(),
                destination: t.target_port,
            })
            .collect();

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
        WebSocketResponse::Requests(vec![]) // Empty response, subscription confirmed
    }

    async fn handle_unsubscribe(&self) -> WebSocketResponse {
        *self.current_filter.write().await = None;
        *self.current_ws_filter.write().await = None;
        WebSocketResponse::Requests(vec![]) // Empty response, unsubscription confirmed
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
                                };

                                if let Ok(response_text) = serde_json::to_string(&response) {
                                    let _ = socket.send(Message::Text(response_text)).await;
                                }
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

// Helper function to convert StoredRequest to StoredRequestWithBase64
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
    }
}

// Helper function to convert StoredResponse to StoredResponseWithBase64
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

// Helper function to convert RequestExchange to RequestExchangeWithBase64
fn exchange_to_base64(exchange: &crate::storage::RequestExchange) -> RequestExchangeWithBase64 {
    RequestExchangeWithBase64 {
        request: request_to_base64(&exchange.request),
        response: exchange.response.as_ref().map(response_to_base64),
    }
}

// Helper function to convert StoredWebSocketMessage to StoredWebSocketMessageWithBase64
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
                },
                TunnelConfig {
                    name: "tunnel-b".to_string(),
                    domain: "b.example.com".to_string(),
                    socket_path: "/tmp/b.sock".to_string(),
                    target_port: 3001,
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
        }
    }

    fn make_web_server() -> WebServer {
        WebServer::new(
            make_config(),
            Arc::new(RequestStorage::new(100)),
            Arc::new(WebSocketMessageStorage::new(1000)),
        )
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
        assert_eq!(tunnels[1].name, "tunnel-b");
        assert_eq!(tunnels[1].destination, 3001);
    }

    // --- handle_query_requests ---

    #[tokio::test]
    async fn test_handle_query_requests_returns_matching() {
        let req_storage = Arc::new(RequestStorage::new(100));
        let ws_storage = Arc::new(WebSocketMessageStorage::new(100));
        let server = WebServer::new(make_config(), req_storage.clone(), ws_storage);

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
        // No response → must not match when status filter is set.
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
}
