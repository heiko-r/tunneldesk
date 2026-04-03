//! MCP (Model Context Protocol) server for TunnelDesk.
//!
//! Exposes tools for managing Cloudflare tunnels, querying captured HTTP
//! requests and WebSocket messages, and replaying them — accessible to any
//! AI tool that speaks the MCP protocol over stdio.
//!
//! Enabled by the `mcp` Cargo feature; activated at runtime with `--mcp`.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::storage::{
    QueryFilter, RequestStorage, StatusFilter, WebSocketMessageFilter, WebSocketMessageStorage,
};
use crate::sync::TunnelSync;
use crate::tunnel::TunnelManager;
use crate::web_server::{
    CreateTunnelRequest, DeleteTunnelRequest, ReplayRequestPayload, UpdateTunnelRequest, WebServer,
    WebSocketResponse,
};

// ── Tool parameter types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateTunnelParams {
    /// Unique name for the tunnel (used as identifier in all other tools).
    name: String,
    /// Public domain this tunnel is exposed on (e.g. "api.example.com").
    domain: String,
    /// Local TCP port to forward traffic to.
    target_port: u16,
    /// Unix socket path cloudflared connects to.
    /// Defaults to /tmp/tunneldesk-{name}.sock when omitted.
    socket_path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateTunnelParams {
    /// Name of the tunnel to update.
    name: String,
    /// New public domain.
    domain: Option<String>,
    /// New Unix socket path.
    socket_path: Option<String>,
    /// New local TCP target port.
    target_port: Option<u16>,
    /// Set to false to disable, or true to re-enable the tunnel.
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeleteTunnelParams {
    /// Name of the tunnel to permanently delete.
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryRequestsParams {
    /// Filter to a specific tunnel by name.
    tunnel_name: Option<String>,
    /// Filter by HTTP method, e.g. "GET" or "POST" (case-insensitive).
    method: Option<String>,
    /// Filter by URL substring match (case-sensitive).
    url_contains: Option<String>,
    /// Filter by HTTP status class: 2 = 2xx, 3 = 3xx, 4 = 4xx, 5 = 5xx.
    status_class: Option<u8>,
    /// Maximum number of results to return. Defaults to 20.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRequestParams {
    /// UUID of the request to retrieve full details for.
    id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryWebSocketMessagesParams {
    /// Filter to a specific tunnel by name.
    tunnel_name: Option<String>,
    /// Filter by the ID of the HTTP upgrade request that opened the WebSocket.
    upgrade_request_id: Option<String>,
    /// Maximum number of results to return. Defaults to 20.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReplayRequestParams {
    /// UUID of the previously captured request to replay.
    id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendRequestParams {
    /// Name of the tunnel whose local target port receives the request.
    tunnel_name: String,
    /// HTTP method, e.g. "GET", "POST", "PUT", "DELETE".
    method: String,
    /// Request path and optional query string, e.g. "/api/users?page=1".
    url: String,
    /// Optional request headers as key-value pairs.
    headers: Option<HashMap<String, String>>,
    /// Optional request body as a UTF-8 string.
    body: Option<String>,
}

// ── Serialisable output types ─────────────────────────────────────────────────

#[derive(Serialize)]
struct TunnelSummary {
    name: String,
    domain: String,
    socket_path: String,
    target_port: u16,
    enabled: bool,
}

#[derive(Serialize)]
struct RequestSummary {
    id: String,
    timestamp: String,
    tunnel_name: String,
    method: String,
    url: String,
    status: Option<u16>,
    response_time_ms: Option<f64>,
    replayed: bool,
}

#[derive(Serialize)]
struct RequestDetail {
    id: String,
    timestamp: String,
    tunnel_name: String,
    method: String,
    url: String,
    headers: HashMap<String, String>,
    body: String,
    replayed: bool,
    response: Option<ResponseDetail>,
}

#[derive(Serialize)]
struct ResponseDetail {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
    response_time_ms: Option<f64>,
}

#[derive(Serialize)]
struct WsMessageSummary {
    id: String,
    timestamp: String,
    tunnel_name: String,
    upgrade_request_id: String,
    direction: String,
    message_type: String,
    payload: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Converts bytes to a UTF-8 string, or a placeholder for binary data.
fn bytes_to_display(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => format!("<binary: {} bytes>", bytes.len()),
    }
}

/// Serialises a `WebSocketResponse` to pretty JSON for LLM-friendly display.
fn format_response(response: &WebSocketResponse) -> String {
    serde_json::to_string_pretty(response).unwrap_or_else(|e| format!("serialization error: {e}"))
}

// ── MCP server struct ─────────────────────────────────────────────────────────

/// The TunnelDesk MCP server.
///
/// Holds a `WebServer` clone (for reusing tunnel CRUD and replay logic) plus
/// direct references to the shared storages for efficient query operations.
#[derive(Clone)]
pub struct TunnelDeskMcp {
    tool_router: ToolRouter<TunnelDeskMcp>,
    /// Used for tunnel CRUD and request-replay operations.
    web_server: WebServer,
    config: Arc<RwLock<Config>>,
    request_storage: Arc<RequestStorage>,
    websocket_storage: Arc<WebSocketMessageStorage>,
}

#[tool_router]
impl TunnelDeskMcp {
    pub fn new(
        config: Arc<RwLock<Config>>,
        tunnel_manager: Arc<TunnelManager>,
        tunnel_sync: Option<Arc<TunnelSync>>,
        request_storage: Arc<RequestStorage>,
        websocket_storage: Arc<WebSocketMessageStorage>,
    ) -> Self {
        let web_server = WebServer::new(
            config.clone(),
            tunnel_manager,
            tunnel_sync,
            request_storage.clone(),
            websocket_storage.clone(),
        );
        Self {
            tool_router: Self::tool_router(),
            web_server,
            config,
            request_storage,
            websocket_storage,
        }
    }

    // ── Tunnel management ─────────────────────────────────────────────────────

    #[tool(
        description = "List all configured tunnels with their name, domain, target port, socket path, and enabled status."
    )]
    async fn list_tunnels(&self) -> Result<CallToolResult, McpError> {
        let cfg = self.config.read().await;
        let tunnels: Vec<TunnelSummary> = cfg
            .tunnels
            .iter()
            .map(|t| TunnelSummary {
                name: t.name.clone(),
                domain: t.domain.clone(),
                socket_path: t.socket_path.clone(),
                target_port: t.target_port,
                enabled: t.enabled,
            })
            .collect();
        let json = serde_json::to_string_pretty(&tunnels)
            .unwrap_or_else(|e| format!("serialization error: {e}"));
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        description = "Create a new tunnel. The local proxy starts immediately and Cloudflare is updated if configured."
    )]
    async fn create_tunnel(
        &self,
        Parameters(p): Parameters<CreateTunnelParams>,
    ) -> Result<CallToolResult, McpError> {
        let req = CreateTunnelRequest {
            name: p.name,
            domain: p.domain,
            socket_path: p.socket_path,
            target_port: p.target_port,
        };
        let response = self.web_server.handle_create_tunnel(req).await;
        Ok(CallToolResult::success(vec![Content::text(
            format_response(&response),
        )]))
    }

    #[tool(
        description = "Update an existing tunnel's domain, target port, socket path, or enabled status. The proxy is restarted to apply changes."
    )]
    async fn update_tunnel(
        &self,
        Parameters(p): Parameters<UpdateTunnelParams>,
    ) -> Result<CallToolResult, McpError> {
        let req = UpdateTunnelRequest {
            name: p.name,
            domain: p.domain,
            socket_path: p.socket_path,
            target_port: p.target_port,
            enabled: p.enabled,
        };
        let response = self.web_server.handle_update_tunnel(req).await;
        Ok(CallToolResult::success(vec![Content::text(
            format_response(&response),
        )]))
    }

    #[tool(
        description = "Permanently delete a tunnel. The proxy is stopped and the entry is removed from Cloudflare if configured."
    )]
    async fn delete_tunnel(
        &self,
        Parameters(p): Parameters<DeleteTunnelParams>,
    ) -> Result<CallToolResult, McpError> {
        let req = DeleteTunnelRequest { name: p.name };
        let response = self.web_server.handle_delete_tunnel(req).await;
        Ok(CallToolResult::success(vec![Content::text(
            format_response(&response),
        )]))
    }

    // ── Request inspection ────────────────────────────────────────────────────

    #[tool(
        description = "Query captured HTTP requests. Returns a summary list (id, method, url, status, response time). Use get_request to retrieve full headers and body for a specific request."
    )]
    async fn query_requests(
        &self,
        Parameters(p): Parameters<QueryRequestsParams>,
    ) -> Result<CallToolResult, McpError> {
        let filter = QueryFilter {
            tunnel_name: p.tunnel_name,
            method: p.method,
            status: p.status_class.map(StatusFilter::Class),
            url_contains: p.url_contains,
            ..Default::default()
        };
        let limit = p.limit.unwrap_or(20);
        let mut exchanges = self.request_storage.query_requests(&filter).await;
        exchanges.truncate(limit);

        let summaries: Vec<RequestSummary> = exchanges
            .iter()
            .map(|e| RequestSummary {
                id: e.request.id.clone(),
                timestamp: e.request.timestamp.to_rfc3339(),
                tunnel_name: e.request.tunnel_name.clone(),
                method: e.request.method.clone(),
                url: e.request.url.clone(),
                status: e.response.as_ref().map(|r| r.status),
                response_time_ms: e.response.as_ref().and_then(|r| r.response_time_ms),
                replayed: e.request.replayed,
            })
            .collect();

        let json = serde_json::to_string_pretty(&summaries)
            .unwrap_or_else(|e| format!("serialization error: {e}"));
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        description = "Get the full details of a captured HTTP request by ID, including all headers and the decoded request/response body."
    )]
    async fn get_request(
        &self,
        Parameters(p): Parameters<GetRequestParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.request_storage.get_request_by_id(&p.id).await {
            None => Ok(CallToolResult::success(vec![Content::text(format!(
                "No request found with id '{}'",
                p.id
            ))])),
            Some(e) => {
                let detail = RequestDetail {
                    id: e.request.id.clone(),
                    timestamp: e.request.timestamp.to_rfc3339(),
                    tunnel_name: e.request.tunnel_name.clone(),
                    method: e.request.method.clone(),
                    url: e.request.url.clone(),
                    headers: e.request.headers.clone(),
                    body: bytes_to_display(&e.request.body),
                    replayed: e.request.replayed,
                    response: e.response.as_ref().map(|r| ResponseDetail {
                        status: r.status,
                        headers: r.headers.clone(),
                        body: bytes_to_display(&r.body),
                        response_time_ms: r.response_time_ms,
                    }),
                };
                let json = serde_json::to_string_pretty(&detail)
                    .unwrap_or_else(|err| format!("serialization error: {err}"));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
        }
    }

    #[tool(
        description = "Query captured WebSocket messages. Filter by tunnel name or by the ID of the HTTP upgrade request that opened the connection."
    )]
    async fn query_websocket_messages(
        &self,
        Parameters(p): Parameters<QueryWebSocketMessagesParams>,
    ) -> Result<CallToolResult, McpError> {
        let filter = WebSocketMessageFilter {
            tunnel_name: p.tunnel_name,
            upgrade_request_id: p.upgrade_request_id,
            ..Default::default()
        };
        let limit = p.limit.unwrap_or(20);
        let mut messages = self.websocket_storage.query_messages(&filter).await;
        messages.truncate(limit);

        let summaries: Vec<WsMessageSummary> = messages
            .iter()
            .map(|m| WsMessageSummary {
                id: m.id.clone(),
                timestamp: m.timestamp.to_rfc3339(),
                tunnel_name: m.tunnel_name.clone(),
                upgrade_request_id: m.upgrade_request_id.clone(),
                direction: m.direction.clone(),
                message_type: format!("{:?}", m.message_type),
                payload: bytes_to_display(&m.payload),
            })
            .collect();

        let json = serde_json::to_string_pretty(&summaries)
            .unwrap_or_else(|e| format!("serialization error: {e}"));
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    // ── Request replay ────────────────────────────────────────────────────────

    #[tool(
        description = "Replay a previously captured HTTP request by its ID. Sends the original request again to the tunnel's local target and stores the new exchange."
    )]
    async fn replay_request(
        &self,
        Parameters(p): Parameters<ReplayRequestParams>,
    ) -> Result<CallToolResult, McpError> {
        let exchange = self.request_storage.get_request_by_id(&p.id).await;
        let req_data = match exchange {
            None => {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "No request found with id '{}'",
                    p.id
                ))]));
            }
            Some(e) => e.request,
        };

        let body_b64 = base64::engine::general_purpose::STANDARD.encode(&req_data.body);
        let payload = ReplayRequestPayload {
            tunnel_name: req_data.tunnel_name,
            method: req_data.method,
            url: req_data.url,
            headers: req_data.headers,
            body: body_b64,
        };

        let response = self.web_server.handle_replay_request(payload).await;
        Ok(CallToolResult::success(vec![Content::text(
            format_response(&response),
        )]))
    }

    #[tool(
        description = "Send a custom HTTP request directly to a tunnel's local target port. The exchange is stored for inspection. Returns the new exchange ID on success."
    )]
    async fn send_request(
        &self,
        Parameters(p): Parameters<SendRequestParams>,
    ) -> Result<CallToolResult, McpError> {
        let body_bytes = p.body.unwrap_or_default().into_bytes();
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(&body_bytes);
        let payload = ReplayRequestPayload {
            tunnel_name: p.tunnel_name,
            method: p.method,
            url: p.url,
            headers: p.headers.unwrap_or_default(),
            body: body_b64,
        };

        let response = self.web_server.handle_replay_request(payload).await;
        Ok(CallToolResult::success(vec![Content::text(
            format_response(&response),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for TunnelDeskMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "TunnelDesk MCP server — manage Cloudflare tunnels and inspect \
             captured traffic.\n\n\
             Tunnel management:\n\
             • list_tunnels — see all configured tunnels\n\
             • create_tunnel — add a tunnel (starts proxy immediately)\n\
             • update_tunnel — change domain, port, enabled status, etc.\n\
             • delete_tunnel — remove a tunnel permanently\n\n\
             Request inspection:\n\
             • query_requests — list captured HTTP requests with filters\n\
             • get_request — full details (headers + body) for one request\n\
             • query_websocket_messages — list captured WebSocket frames\n\n\
             Replay:\n\
             • replay_request — re-send a captured request by ID\n\
             • send_request — send a custom HTTP request to a tunnel target"
                    .to_string(),
            )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use tokio::sync::RwLock;

    use crate::config::{CaptureConfig, Config, GuiConfig, LoggingConfig, TunnelConfig};

    fn base_config(tunnels: Vec<TunnelConfig>) -> Config {
        Config {
            tunnels,
            logging: LoggingConfig {
                stdout_level: "off".to_string(),
                max_request_body_size: 1024,
            },
            capture: CaptureConfig {
                max_stored_requests: 100,
                max_request_body_size: 1024 * 1024,
            },
            gui: GuiConfig { port: 9999 },
            cloudflare: None,
            config_path: None,
        }
    }
    use crate::storage::{RequestExchange, RequestStorage, StoredRequest, WebSocketMessageStorage};
    use crate::tunnel::TunnelManager;

    use super::*;

    fn test_mcp(tunnels: Vec<TunnelConfig>) -> TunnelDeskMcp {
        let config = Arc::new(RwLock::new(base_config(tunnels)));
        let request_storage = Arc::new(RequestStorage::new(100));
        let websocket_storage = Arc::new(WebSocketMessageStorage::new(100));
        let empty_cfg = base_config(vec![]);
        let tunnel_manager = Arc::new(TunnelManager::new(
            &empty_cfg,
            request_storage.clone(),
            websocket_storage.clone(),
        ));
        TunnelDeskMcp::new(
            config,
            tunnel_manager,
            None,
            request_storage,
            websocket_storage,
        )
    }

    fn make_tunnel(name: &str, port: u16) -> TunnelConfig {
        TunnelConfig {
            name: name.to_string(),
            domain: format!("{name}.example.com"),
            socket_path: format!("/tmp/{name}.sock"),
            target_port: port,
            enabled: true,
        }
    }

    fn make_exchange(id: &str, tunnel: &str, method: &str, url: &str) -> RequestExchange {
        RequestExchange {
            request: StoredRequest {
                id: id.to_string(),
                timestamp: chrono::Utc::now(),
                tunnel_name: tunnel.to_string(),
                method: method.to_string(),
                url: url.to_string(),
                headers: HashMap::new(),
                body: b"hello".to_vec(),
                raw_request: vec![],
                replayed: false,
            },
            response: None,
        }
    }

    /// Extracts all text from a `CallToolResult` by serialising it to JSON.
    /// The MCP `Content` type is a complex annotated wrapper, so JSON is the
    /// most robust way to inspect the text payload in tests.
    fn result_text(result: &CallToolResult) -> String {
        serde_json::to_string(&result.content).unwrap()
    }

    #[tokio::test]
    async fn list_tunnels_returns_configured_tunnels() {
        let mcp = test_mcp(vec![make_tunnel("api", 8080), make_tunnel("web", 3000)]);
        let result = mcp.list_tunnels().await.unwrap();
        let text = result_text(&result);
        assert!(text.contains("api"));
        assert!(text.contains("web"));
        assert!(text.contains("8080"));
        assert!(text.contains("3000"));
    }

    #[tokio::test]
    async fn list_tunnels_empty_returns_empty_array() {
        let mcp = test_mcp(vec![]);
        let result = mcp.list_tunnels().await.unwrap();
        let text = result_text(&result);
        // The JSON-serialised content wraps the text, but the inner text is "[]"
        assert!(text.contains("[]"));
    }

    #[tokio::test]
    async fn query_requests_empty_storage() {
        let mcp = test_mcp(vec![]);
        let result = mcp
            .query_requests(Parameters(QueryRequestsParams {
                tunnel_name: None,
                method: None,
                url_contains: None,
                status_class: None,
                limit: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.contains("[]"));
    }

    #[tokio::test]
    async fn query_requests_returns_stored_requests() {
        let mcp = test_mcp(vec![]);
        mcp.request_storage
            .store_exchange(make_exchange("req-1", "api", "GET", "/users"))
            .await;
        mcp.request_storage
            .store_exchange(make_exchange("req-2", "api", "POST", "/items"))
            .await;

        let result = mcp
            .query_requests(Parameters(QueryRequestsParams {
                tunnel_name: None,
                method: None,
                url_contains: None,
                status_class: None,
                limit: Some(10),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.contains("req-1"));
        assert!(text.contains("req-2"));
    }

    #[tokio::test]
    async fn query_requests_respects_limit() {
        let mcp = test_mcp(vec![]);
        for i in 0..10 {
            mcp.request_storage
                .store_exchange(make_exchange(&format!("req-{i}"), "api", "GET", "/path"))
                .await;
        }

        let result = mcp
            .query_requests(Parameters(QueryRequestsParams {
                tunnel_name: None,
                method: None,
                url_contains: None,
                status_class: None,
                limit: Some(3),
            }))
            .await
            .unwrap();
        // The text content is a JSON array of 3 summaries. Verify by counting id occurrences.
        let text = result_text(&result);
        // Each summary has an "id" field; with limit=3 we get at most 3.
        let id_count = text.matches("req-").count();
        assert!(
            id_count <= 3,
            "expected at most 3 results, text was: {text}"
        );
    }

    #[tokio::test]
    async fn get_request_not_found() {
        let mcp = test_mcp(vec![]);
        let result = mcp
            .get_request(Parameters(GetRequestParams {
                id: "nonexistent".to_string(),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.contains("nonexistent"));
    }

    #[tokio::test]
    async fn get_request_returns_full_details() {
        let mcp = test_mcp(vec![]);
        let mut exchange = make_exchange("req-42", "api", "GET", "/health");
        exchange.request.body = b"request body".to_vec();
        mcp.request_storage.store_exchange(exchange).await;

        let result = mcp
            .get_request(Parameters(GetRequestParams {
                id: "req-42".to_string(),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.contains("req-42"));
        assert!(text.contains("/health"));
        assert!(text.contains("request body"));
    }

    #[tokio::test]
    async fn query_websocket_messages_empty() {
        let mcp = test_mcp(vec![]);
        let result = mcp
            .query_websocket_messages(Parameters(QueryWebSocketMessagesParams {
                tunnel_name: None,
                upgrade_request_id: None,
                limit: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.contains("[]"));
    }

    #[test]
    fn bytes_to_display_utf8() {
        assert_eq!(bytes_to_display(b"hello"), "hello");
    }

    #[test]
    fn bytes_to_display_binary() {
        let result = bytes_to_display(&[0xff, 0xfe, 0x00]);
        assert!(result.starts_with("<binary:"));
        assert!(result.contains("3 bytes"));
    }

    #[tokio::test]
    async fn replay_request_not_found() {
        let mcp = test_mcp(vec![]);
        let result = mcp
            .replay_request(Parameters(ReplayRequestParams {
                id: "missing-id".to_string(),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.contains("missing-id"));
    }
}
