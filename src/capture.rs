use crate::storage::{
    RequestStorage, StoredRequest, StoredResponse, StoredWebSocketMessage, WebSocketMessageStorage,
    WebSocketMessageType,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Verbosity of stdout logging for captured traffic.
#[derive(Debug, Clone, PartialEq)]
pub enum LogLevel {
    Off,
    Basic,
    Full,
}

impl From<&str> for LogLevel {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "off" => LogLevel::Off,
            "basic" => LogLevel::Basic,
            "full" => LogLevel::Full,
            _ => LogLevel::Basic,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Off => write!(f, "off"),
            LogLevel::Basic => write!(f, "basic"),
            LogLevel::Full => write!(f, "full"),
        }
    }
}

/// Parses and stores captured proxy traffic.
///
/// Each [`Proxy`](crate::proxy::Proxy) instance owns a `Capture` that receives raw
/// bytes from both directions of a TCP connection, identifies HTTP request/response
/// messages and WebSocket frames, and persists them to the shared storage.
#[derive(Clone)]
pub struct Capture {
    pub storage: RequestStorage,
    pub websocket_storage: WebSocketMessageStorage,
    log_level: LogLevel,
    /// Maximum body bytes to print when `log_level` is `Full`.
    log_body_limit: usize,
    // Track WebSocket upgrade requests per connection
    websocket_upgrades: Arc<RwLock<HashMap<String, String>>>, // connection_id -> request_id
}

/// HTTP method tokens to identify request lines.
pub(crate) const HTTP_METHODS: &[&str] =
    &["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"];

/// Returns `true` if `line` starts with a known HTTP method token.
pub(crate) fn is_http_request_line(line: &str) -> bool {
    HTTP_METHODS.iter().any(|m| line.starts_with(m))
}

/// Extracts the body portion from a raw HTTP message by finding the header/body
/// separator (`\r\n\r\n`, or `\n\n` as a fallback).  Returns an empty `Vec`
/// when no separator is found or when `has_body` is `false`.
fn extract_body_from_raw(raw_message: &[u8], has_body: bool) -> Vec<u8> {
    if !has_body {
        return Vec::new();
    }
    // Prefer the canonical \r\n\r\n separator.
    for i in 0..raw_message.len().saturating_sub(3) {
        if raw_message[i] == b'\r'
            && raw_message[i + 1] == b'\n'
            && raw_message[i + 2] == b'\r'
            && raw_message[i + 3] == b'\n'
        {
            return raw_message[i + 4..].to_vec();
        }
    }
    // Fall back to \n\n.
    for i in 0..raw_message.len().saturating_sub(1) {
        if raw_message[i] == b'\n' && raw_message[i + 1] == b'\n' {
            return raw_message[i + 2..].to_vec();
        }
    }
    Vec::new()
}

/// Parses HTTP headers from a slice of message lines (starting at line 0).
/// Returns the header map and the index of the first body line (0 when no
/// blank line is found, which means there is no body).
fn parse_http_headers(lines: &[&str]) -> (HashMap<String, String>, usize) {
    let mut headers = HashMap::new();
    let mut body_start = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            body_start = i + 1;
            break;
        }
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_string();
            let value = line[colon_pos + 1..].trim().to_string();
            headers.insert(key, value);
        }
    }
    (headers, body_start)
}

/// Returns `true` if the headers indicate a WebSocket upgrade handshake.
fn is_websocket_upgrade(headers: &HashMap<String, String>) -> bool {
    headers
        .iter()
        .any(|(k, v)| k.to_lowercase() == "upgrade" && v.to_lowercase() == "websocket")
        && headers
            .iter()
            .any(|(k, v)| k.to_lowercase() == "connection" && v.to_lowercase().contains("upgrade"))
}

impl Capture {
    /// Creates a new `Capture`.
    ///
    /// * `stdout_level` — parsed by [`LogLevel::from`]; unknown values default to `"basic"`.
    /// * `log_body_limit` — maximum body bytes written to stdout in `full` mode.
    pub fn new(
        request_storage: RequestStorage,
        websocket_storage: WebSocketMessageStorage,
        stdout_level: &str,
        log_body_limit: usize,
    ) -> Self {
        Self {
            storage: request_storage,
            websocket_storage,
            log_level: LogLevel::from(stdout_level),
            log_body_limit,
            websocket_upgrades: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Classifies `raw_message` as an HTTP request, HTTP response, or raw binary
    /// data, then logs and stores it accordingly.
    ///
    /// * `connection_id` — opaque identifier for the TCP connection; used to
    ///   match responses to their requests.
    /// * `direction` — human-readable direction string used in raw-message logs.
    pub async fn capture_raw_message(
        &self,
        tunnel_name: &str,
        connection_id: &str,
        direction: &str,
        raw_message: &[u8],
    ) -> anyhow::Result<()> {
        let message_str = String::from_utf8_lossy(raw_message);
        let lines: Vec<&str> = message_str.lines().collect();

        if let Some(first_line) = lines.first() {
            if is_http_request_line(first_line) {
                return self
                    .capture_http_request(tunnel_name, connection_id, &lines, raw_message)
                    .await;
            } else if first_line.starts_with("HTTP/") {
                return self
                    .capture_http_response(tunnel_name, connection_id, &lines, raw_message)
                    .await;
            }
        }

        // Not HTTP — log as raw binary.
        info!(
            "Raw message [{}]: {} bytes - direction: {}",
            tunnel_name,
            raw_message.len(),
            direction
        );
        Ok(())
    }

    async fn capture_http_request(
        &self,
        tunnel_name: &str,
        connection_id: &str,
        lines: &[&str],
        raw_message: &[u8],
    ) -> anyhow::Result<()> {
        let parts: Vec<&str> = lines[0].split_whitespace().collect();
        if parts.len() < 2 {
            return Ok(());
        }
        let method = parts[0].to_string();
        let path = parts[1].to_string();

        let (headers, body_start) = parse_http_headers(lines);
        let body = extract_body_from_raw(raw_message, body_start < lines.len());

        match self.log_level {
            LogLevel::Off => {}
            LogLevel::Basic => {
                info!("→ {} {} [{}]", parts[0], parts[1], tunnel_name);
            }
            LogLevel::Full => {
                info!(
                    "[{}] → {} {} - {} bytes",
                    tunnel_name,
                    parts[0],
                    parts[1],
                    raw_message.len()
                );
                info!("[{}] Headers: {:?}", tunnel_name, headers);
                info!(
                    "[{}] Body: {}",
                    tunnel_name,
                    Self::body_preview(&body, self.log_body_limit)
                );
            }
        }

        let stored_request = StoredRequest {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            tunnel_name: tunnel_name.to_string(),
            method,
            url: path,
            headers: headers.clone(),
            body,
            raw_request: raw_message.to_vec(),
            replayed: false,
        };

        if is_websocket_upgrade(&headers) {
            let mut upgrades = self.websocket_upgrades.write().await;
            upgrades.insert(connection_id.to_string(), stored_request.id.clone());
            info!(
                "[{}] WebSocket upgrade detected - request_id: {}, connection_id: {}",
                tunnel_name, stored_request.id, connection_id
            );
        }

        self.storage
            .store_request_with_connection(stored_request, connection_id)
            .await;
        Ok(())
    }

    async fn capture_http_response(
        &self,
        tunnel_name: &str,
        connection_id: &str,
        lines: &[&str],
        raw_message: &[u8],
    ) -> anyhow::Result<()> {
        let parts: Vec<&str> = lines[0].split_whitespace().collect();
        if parts.len() < 2 {
            return Ok(());
        }
        let Ok(status) = parts[1].parse::<u16>() else {
            return Ok(());
        };

        let (headers, body_start) = parse_http_headers(lines);
        let body = extract_body_from_raw(raw_message, body_start < lines.len());

        match self.log_level {
            LogLevel::Off => {}
            LogLevel::Basic => {
                info!("← {} [{}]", status, tunnel_name);
            }
            LogLevel::Full => {
                info!(
                    "[{}] ← {} - {} bytes",
                    tunnel_name,
                    status,
                    raw_message.len()
                );
                info!("[{}] Headers: {:?}", tunnel_name, headers);
                info!(
                    "[{}] Body: {}",
                    tunnel_name,
                    Self::body_preview(&body, self.log_body_limit)
                );
            }
        }

        let now = chrono::Utc::now();
        let (request_id, response_time_ms) = self
            .storage
            .get_next_pending_request_for_connection(connection_id)
            .await
            .map(|(id, request_time)| {
                let elapsed = now - request_time;
                let ms = elapsed.num_microseconds().unwrap_or(0) as f64 / 1000.0;
                (id, Some(ms))
            })
            .unwrap_or_else(|| ("unknown".to_string(), None));

        let stored_response = StoredResponse {
            request_id,
            timestamp: now,
            status,
            headers,
            body,
            raw_response: raw_message.to_vec(),
            response_time_ms,
        };

        self.storage.store_response(stored_response).await;
        Ok(())
    }

    /// Parses a raw WebSocket frame from `raw_frame`, unmasks the payload if
    /// necessary, and stores it linked to the upgrade request for `connection_id`.
    /// Returns `Ok(())` immediately when the frame is incomplete or too short.
    pub async fn capture_websocket_message_raw(
        &self,
        tunnel_name: &str,
        connection_id: &str,
        direction: &str,
        raw_frame: &[u8],
    ) -> anyhow::Result<()> {
        if raw_frame.len() < 2 {
            return Ok(());
        }

        let first_byte = raw_frame[0];
        let second_byte = raw_frame[1];

        let opcode = first_byte & 0x0F;
        let masked = (second_byte & 0x80) != 0;
        let payload_len = (second_byte & 0x7F) as usize;

        let mut header_len = 2;
        if payload_len == 126 {
            header_len += 2;
        } else if payload_len == 127 {
            header_len += 8;
        }

        if masked {
            header_len += 4;
        }

        let total_len = header_len + payload_len;
        if raw_frame.len() < total_len {
            return Ok(());
        }

        let payload = &raw_frame[header_len..total_len];

        // Unmask payload if it's masked (client-to-server messages)
        let unmasked_payload = if masked {
            let masking_key = &raw_frame[header_len - 4..header_len];
            let mut unmasked = payload.to_vec();
            for (i, byte) in unmasked.iter_mut().enumerate() {
                *byte ^= masking_key[i % 4];
            }
            unmasked
        } else {
            payload.to_vec()
        };

        // Determine message type
        let message_type = match opcode {
            0x1 => WebSocketMessageType::Text,
            0x2 => WebSocketMessageType::Binary,
            _ => WebSocketMessageType::Binary, // Default for other opcodes including close
        };

        // Store the WebSocket message
        let message_id = uuid::Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now();

        // Get the upgrade request ID for this connection, if any
        let upgrades = self.websocket_upgrades.read().await;
        let upgrade_request_id = upgrades
            .get(connection_id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        drop(upgrades); // Release the read lock

        let stored_message = StoredWebSocketMessage {
            id: message_id,
            timestamp,
            tunnel_name: tunnel_name.to_string(),
            upgrade_request_id,
            direction: direction.to_string(),
            message_type,
            payload: unmasked_payload.clone(),
        };

        self.websocket_storage.store_message(stored_message).await;

        // Log based on level
        match self.log_level {
            LogLevel::Off => {}
            LogLevel::Basic => {
                info!("WS {} [{}]", direction, tunnel_name);
            }
            LogLevel::Full => {
                let payload_preview = Self::body_preview(&unmasked_payload, self.log_body_limit);
                info!(
                    "[{}] WS {} - opcode: {}, payload: {}",
                    tunnel_name, direction, opcode, payload_preview
                );
            }
        }

        Ok(())
    }

    /// Returns a UTF-8 lossy representation of `body` truncated to `limit`
    /// bytes, with a `...` suffix when truncated.
    fn body_preview(body: &[u8], limit: usize) -> String {
        if body.len() <= limit {
            String::from_utf8_lossy(body).into_owned()
        } else {
            format!("{}...", String::from_utf8_lossy(&body[..limit]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_from_str() {
        assert_eq!(LogLevel::from("off"), LogLevel::Off);
        assert_eq!(LogLevel::from("OFF"), LogLevel::Off);
        assert_eq!(LogLevel::from("basic"), LogLevel::Basic);
        assert_eq!(LogLevel::from("BASIC"), LogLevel::Basic);
        assert_eq!(LogLevel::from("full"), LogLevel::Full);
        assert_eq!(LogLevel::from("FULL"), LogLevel::Full);
        assert_eq!(LogLevel::from("invalid"), LogLevel::Basic); // Default
        assert_eq!(LogLevel::from(""), LogLevel::Basic); // Default
    }

    #[test]
    fn test_log_level_display() {
        assert_eq!(LogLevel::Off.to_string(), "off");
        assert_eq!(LogLevel::Basic.to_string(), "basic");
        assert_eq!(LogLevel::Full.to_string(), "full");
    }

    #[test]
    fn test_capture_new() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "full", 1024);
        assert!(matches!(capture.log_level, LogLevel::Full));
        assert_eq!(capture.log_body_limit, 1024);
    }

    #[test]
    fn test_body_preview_below_limit() {
        assert_eq!(Capture::body_preview(b"hello", 10), "hello");
    }

    #[test]
    fn test_body_preview_at_limit() {
        assert_eq!(Capture::body_preview(b"hello", 5), "hello");
    }

    #[test]
    fn test_body_preview_exceeds_limit() {
        assert_eq!(Capture::body_preview(b"hello world", 5), "hello...");
    }

    #[test]
    fn test_body_preview_empty() {
        assert_eq!(Capture::body_preview(b"", 10), "");
    }

    #[test]
    fn test_body_preview_zero_limit() {
        assert_eq!(Capture::body_preview(b"hello", 0), "...");
    }

    #[tokio::test]
    async fn test_capture_raw_message_get_request() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let raw_message = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\nContent-Type: application/json\r\n\r\n{\"key\":\"value\"}";

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "incoming", raw_message)
            .await;
        assert!(result.is_ok());

        // Check that the request was stored
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0].request;
        assert_eq!(request.method, "GET");
        assert_eq!(request.url, "/api/users");
        assert_eq!(request.tunnel_name, "test_tunnel");
        assert_eq!(
            request.headers.get("Host"),
            Some(&"example.com".to_string())
        );
        assert_eq!(
            request.headers.get("Content-Type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(request.body, b"{\"key\":\"value\"}");
    }

    #[tokio::test]
    async fn test_capture_raw_message_post_request() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let raw_message = b"POST /api/data HTTP/1.1\r\nHost: api.example.com\r\nContent-Length: 12\r\n\r\nHello World!";

        let result = capture
            .capture_raw_message("api_tunnel", "test-connection", "outgoing", raw_message)
            .await;
        assert!(result.is_ok());

        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0].request;
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/api/data");
        assert_eq!(request.body, b"Hello World!");
    }

    #[tokio::test]
    async fn test_capture_raw_message_http_response() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let raw_message = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"success\":true}";

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "incoming", raw_message)
            .await;
        assert!(result.is_ok());

        // Responses are stored separately, not as part of request exchanges
        // Let's check if there are any requests (should be 0 for response-only)
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 0);

        // The response was stored but we can't easily access it directly through the public API
        // This test just verifies that processing a response doesn't crash and doesn't create requests
    }

    #[tokio::test]
    async fn test_capture_raw_message_invalid_http() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let raw_message = b"Invalid HTTP message";

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "incoming", raw_message)
            .await;
        assert!(result.is_ok());

        // Should not store any requests
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_raw_message_empty() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let raw_message = b"";

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "incoming", raw_message)
            .await;
        assert!(result.is_ok());

        // Should not store any requests
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_raw_message_request_without_body() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let raw_message = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n";

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "incoming", raw_message)
            .await;
        assert!(result.is_ok());

        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0].request;
        assert_eq!(request.method, "GET");
        assert_eq!(request.body, b"");
    }

    #[tokio::test]
    async fn test_capture_websocket_message_text() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Create a simple WebSocket text frame (unmasked, opcode 0x1)
        let mut frame = Vec::new();
        frame.push(0x81); // FIN=1, opcode=0x1 (text)
        frame.push(0x10); // Payload length = 16 (for "Hello WebSocket!")
        frame.extend_from_slice(b"Hello WebSocket!");

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "test-connection", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.tunnel_name, "ws_tunnel");
        assert_eq!(message.direction, "outgoing");
        assert!(matches!(message.message_type, WebSocketMessageType::Text));
        assert_eq!(message.payload, b"Hello WebSocket!");
    }

    #[tokio::test]
    async fn test_capture_websocket_message_binary() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Create a WebSocket binary frame (unmasked, opcode 0x2)
        let mut frame = Vec::new();
        frame.push(0x82); // FIN=1, opcode=0x2 (binary)
        frame.push(0x04); // Payload length = 4
        frame.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "test-connection", "incoming", &frame)
            .await;
        assert!(result.is_ok());

        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.direction, "incoming");
        assert!(matches!(message.message_type, WebSocketMessageType::Binary));
        assert_eq!(message.payload, &[0x01, 0x02, 0x03, 0x04]);
    }

    #[tokio::test]
    async fn test_capture_websocket_message_masked() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Create a masked WebSocket text frame (client-to-server)
        let mut frame = Vec::new();
        frame.push(0x81); // FIN=1, opcode=0x1 (text)
        frame.push(0x85); // Masked=1, payload length = 5
        // Masking key
        frame.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        // Masked payload: "Hello" XOR with masking key
        frame.extend_from_slice(&[
            0x48 ^ 0x12,
            0x65 ^ 0x34,
            0x6C ^ 0x56,
            0x6C ^ 0x78,
            0x6F ^ 0x12,
        ]);

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "test-connection", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.payload, b"Hello");
    }

    #[tokio::test]
    async fn test_capture_websocket_message_too_short() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let frame = vec![0x81]; // Incomplete frame

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "test-connection", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        // Should not store any messages
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_websocket_message_incomplete_payload() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Frame header indicates 10 bytes but only provides 5
        let mut frame = Vec::new();
        frame.push(0x81); // FIN=1, opcode=0x1 (text)
        frame.push(0x0A); // Payload length = 10
        frame.extend_from_slice(b"Hello"); // Only 5 bytes of payload

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "test-connection", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        // Should not store any messages
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_websocket_message_extended_payload_length() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Create a frame with 126-byte payload length (2-byte extended length)
        let mut frame = Vec::new();
        frame.push(0x81); // FIN=1, opcode=0x1 (text)
        frame.push(0x7E); // Masked=0, payload length = 126 (extended)
        frame.extend_from_slice(&[0x00, 0x7E]); // Extended payload length = 126
        frame.extend_from_slice(&[0x42; 126]); // 126 bytes of payload

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "test-connection", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload.len(), 126);
    }

    #[tokio::test]
    async fn test_capture_websocket_message_unknown_opcode() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Create a frame with unknown opcode (should default to Binary)
        let mut frame = Vec::new();
        frame.push(0x88); // FIN=1, opcode=0x8 (close)
        frame.push(0x02); // Payload length = 2
        frame.extend_from_slice(&[0x03, 0xE8]); // Close code 1000

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "test-connection", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        // Unknown opcodes should default to Binary
        assert!(matches!(
            messages[0].message_type,
            WebSocketMessageType::Binary
        ));
    }

    #[tokio::test]
    async fn test_all_http_methods() {
        let methods = ["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"];

        for method in methods.iter() {
            let storage = RequestStorage::new(100);
            let websocket_storage = WebSocketMessageStorage::new(1000);
            let capture = Capture::new(storage, websocket_storage, "off", 1024);
            let raw_message = format!("{} /test HTTP/1.1\r\nHost: example.com\r\n\r\n", method);

            let result = capture
                .capture_raw_message(
                    "test_tunnel",
                    "test-connection",
                    "incoming",
                    raw_message.as_bytes(),
                )
                .await;

            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_storage_limits() {
        let storage = RequestStorage::new(2);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024); // Very small storage limit

        // Store 3 requests
        for i in 0..3 {
            let raw_message = format!("GET /test{} HTTP/1.1\r\nHost: example.com\r\n\r\n", i);
            capture
                .capture_raw_message(
                    "test_tunnel",
                    "test-connection",
                    "incoming",
                    raw_message.as_bytes(),
                )
                .await
                .unwrap();
        }

        // Should only keep the 2 most recent requests
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn test_websocket_upgrade_tracking() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let connection_id = "test-connection-ws";

        // First, capture a WebSocket upgrade request
        let upgrade_request = b"GET /ws HTTP/1.1\r\nHost: example.com\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";

        capture
            .capture_raw_message("test_tunnel", connection_id, "→", upgrade_request)
            .await
            .unwrap();

        // Verify the upgrade request was stored
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let upgrade_request_id = &requests[0].request.id;

        // Now capture WebSocket messages on the same connection
        let ws_frame = b"\x81\x05Hello"; // Unmasked text frame
        capture
            .capture_websocket_message_raw("test_tunnel", connection_id, "outgoing", ws_frame)
            .await
            .unwrap();

        // Verify the WebSocket message has the correct upgrade_request_id
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.upgrade_request_id, *upgrade_request_id);
        assert_eq!(message.tunnel_name, "test_tunnel");
        assert_eq!(message.direction, "outgoing");
    }

    #[tokio::test]
    async fn test_websocket_message_without_upgrade() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let connection_id = "test-connection-no-ws";

        // Capture WebSocket message without prior upgrade request
        let ws_frame = b"\x81\x05Hello"; // Unmasked text frame
        capture
            .capture_websocket_message_raw("test_tunnel", connection_id, "outgoing", ws_frame)
            .await
            .unwrap();

        // Verify the WebSocket message has "unknown" upgrade_request_id
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.upgrade_request_id, "unknown");
        assert_eq!(message.tunnel_name, "test_tunnel");
        assert_eq!(message.direction, "outgoing");
    }

    #[tokio::test]
    async fn test_non_websocket_upgrade_request() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);
        let connection_id = "test-connection-http";

        // Capture a regular HTTP request (not WebSocket upgrade)
        let regular_request = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n";
        capture
            .capture_raw_message("test_tunnel", connection_id, "→", regular_request)
            .await
            .unwrap();

        // Now capture WebSocket messages on the same connection
        let ws_frame = b"\x81\x05Hello"; // Unmasked text frame
        capture
            .capture_websocket_message_raw("test_tunnel", connection_id, "outgoing", ws_frame)
            .await
            .unwrap();

        // Verify the WebSocket message has "unknown" upgrade_request_id
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.upgrade_request_id, "unknown");
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::storage::RequestStorage;

    #[tokio::test]
    async fn test_connection_based_request_response_matching() {
        let storage = RequestStorage::new(100);
        let websocket_storage = crate::storage::WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage.clone(), websocket_storage, "off", 1024);

        let connection_id = "test-connection-1";

        // Store a request
        let request_data = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n";
        capture
            .capture_raw_message("test_tunnel", connection_id, "→", request_data)
            .await
            .unwrap();

        // Store a response - should be matched to the request
        let response_data =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"users\":[]}";
        capture
            .capture_raw_message("test_tunnel", connection_id, "←", response_data)
            .await
            .unwrap();

        // Verify the request and response are linked
        let requests = storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);

        let exchange = &requests[0];
        assert_eq!(exchange.request.method, "GET");
        assert_eq!(exchange.request.url, "/api/users");
        assert!(exchange.response.is_some());

        let response = exchange.response.as_ref().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.request_id, exchange.request.id);
    }

    #[tokio::test]
    async fn test_multiple_requests_same_connection_fifo_ordering() {
        let storage = RequestStorage::new(100);
        let websocket_storage = crate::storage::WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage.clone(), websocket_storage, "off", 1024);

        let connection_id = "test-connection-2";

        // Store multiple requests
        let req1 = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let req2 = b"POST /api/data HTTP/1.1\r\nHost: example.com\r\n\r\n{\"data\":\"value\"}";

        capture
            .capture_raw_message("test_tunnel", connection_id, "→", req1)
            .await
            .unwrap();
        capture
            .capture_raw_message("test_tunnel", connection_id, "→", req2)
            .await
            .unwrap();

        // Store responses - should match in FIFO order
        let resp1 = b"HTTP/1.1 200 OK\r\n\r\n{\"users\":[]}";
        let resp2 = b"HTTP/1.1 201 Created\r\n\r\n{\"id\":123}";

        capture
            .capture_raw_message("test_tunnel", connection_id, "←", resp1)
            .await
            .unwrap();
        capture
            .capture_raw_message("test_tunnel", connection_id, "←", resp2)
            .await
            .unwrap();

        // Verify FIFO matching
        let sorted_filter = crate::storage::QueryFilter {
            sort_direction: Some(crate::storage::SortDirection::Asc),
            ..Default::default()
        };
        let requests = storage.query_requests(&sorted_filter).await;
        assert_eq!(requests.len(), 2);

        // First request should match first response
        assert_eq!(requests[0].request.method, "GET");
        assert_eq!(requests[0].request.url, "/api/users");
        assert!(requests[0].response.is_some());
        assert_eq!(requests[0].response.as_ref().unwrap().status, 200);

        // Second request should match second response
        assert_eq!(requests[1].request.method, "POST");
        assert_eq!(requests[1].request.url, "/api/data");
        assert!(requests[1].response.is_some());
        assert_eq!(requests[1].response.as_ref().unwrap().status, 201);
    }

    #[tokio::test]
    async fn test_different_connections_independent_matching() {
        let storage = RequestStorage::new(100);
        let websocket_storage = crate::storage::WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage.clone(), websocket_storage, "off", 1024);

        let conn1 = "connection-1";
        let conn2 = "connection-2";

        // Store requests on different connections
        let req1 = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let req2 = b"GET /api/posts HTTP/1.1\r\nHost: example.com\r\n\r\n";

        capture
            .capture_raw_message("test_tunnel", conn1, "→", req1)
            .await
            .unwrap();
        capture
            .capture_raw_message("test_tunnel", conn2, "→", req2)
            .await
            .unwrap();

        // Store responses - should be matched independently per connection
        let resp1 = b"HTTP/1.1 200 OK\r\n\r\n{\"users\":[]}";
        let resp2 = b"HTTP/1.1 404 Not Found\r\n\r\n{\"error\":\"Not found\"}";

        capture
            .capture_raw_message("test_tunnel", conn2, "←", resp2)
            .await
            .unwrap(); // Response for conn2
        capture
            .capture_raw_message("test_tunnel", conn1, "←", resp1)
            .await
            .unwrap(); // Response for conn1

        // Verify independent matching
        let requests = storage.get_all_requests().await;
        assert_eq!(requests.len(), 2);

        // Both requests should have responses
        for exchange in &requests {
            assert!(exchange.response.is_some());
        }

        // Find the specific requests to verify correct matching
        let users_req = requests
            .iter()
            .find(|r| r.request.url == "/api/users")
            .unwrap();
        let posts_req = requests
            .iter()
            .find(|r| r.request.url == "/api/posts")
            .unwrap();

        assert_eq!(users_req.response.as_ref().unwrap().status, 200);
        assert_eq!(posts_req.response.as_ref().unwrap().status, 404);
    }

    #[tokio::test]
    async fn test_capture_binary_request_body() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Create a binary request body (PNG image data)
        let png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde\x00\x00\x00\x0cIDATx\x9cc``\x00\x00\x00\x00\x01\x00\x01\x00\x00\x00\x00IEND\xaeB`\x82";

        let raw_request = format!(
            "POST /upload HTTP/1.1\r\nHost: example.com\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
            png_header.len()
        );
        let mut full_request = raw_request.into_bytes();
        full_request.extend_from_slice(png_header);

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "outgoing", &full_request)
            .await;
        assert!(result.is_ok());

        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0].request;

        // Verify headers
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/upload");
        assert_eq!(
            request.headers.get("Content-Type"),
            Some(&"image/png".to_string())
        );

        // Verify binary body is preserved exactly
        assert_eq!(request.body, png_header);
        assert_eq!(request.body.len(), png_header.len());

        // Verify raw request contains binary data
        assert!(
            request
                .raw_request
                .windows(png_header.len())
                .any(|window| window == png_header)
        );
    }

    #[tokio::test]
    async fn test_capture_binary_response_body() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // First capture a request to have something to match the response to
        let request_data = b"GET /image.png HTTP/1.1\r\nHost: example.com\r\n\r\n";
        capture
            .capture_raw_message("test_tunnel", "test-connection", "outgoing", request_data)
            .await
            .unwrap();

        // Create a binary response body (JPEG image data)
        let jpeg_header = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x01\x00H\x00H\x00\x00\xff\xdb\x00C\x00\x08\x06\x06\x07\x06\x05\x08\x07\x07\x07\t\x0e\x12\x11\t\x12\x11\t\x12\x11\t\x12\x11\t\x12\x11\t\x12\x11\t\x12\x11\t\x12\x11\t\x13\x13\x1a\x1a\x1a\x1a\x1a\x1a\x1a\x1a\x1a\x1a\xff\xc0\x00\x11\x08\x00\x01\x00\x01\x01\x01\x11\x00\x02\x11\x01\x03\x11\x01\xff\xc4\x00\x14\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x08\xff\xc4\x00\x14\x10\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xff\xda\x00\x0c\x03\x01\x00\x02\x11\x03\x11\x00\x3f\x00\x80\xff\xd9";

        let raw_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg_header.len()
        );
        let mut full_response = raw_response.into_bytes();
        full_response.extend_from_slice(jpeg_header);

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "incoming", &full_response)
            .await;
        assert!(result.is_ok());

        // Get the request exchange to check the response
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let exchange = &requests[0];

        // Verify response exists and has correct binary data
        assert!(exchange.response.is_some());
        let response = exchange.response.as_ref().unwrap();

        // Verify headers
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("Content-Type"),
            Some(&"image/jpeg".to_string())
        );

        // Verify binary body is preserved exactly
        assert_eq!(response.body, jpeg_header);
        assert_eq!(response.body.len(), jpeg_header.len());

        // Verify raw response contains binary data
        assert!(
            response
                .raw_response
                .windows(jpeg_header.len())
                .any(|window| window == jpeg_header)
        );
    }

    #[tokio::test]
    async fn test_capture_mixed_binary_text_data() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Test with mixed binary and text data that includes null bytes
        let mixed_data = b"Hello\x00\x01\x02World\xff\xfe\x03\x04End";

        let raw_request = format!(
            "POST /mixed HTTP/1.1\r\nHost: example.com\r\nContent-Length: {}\r\n\r\n",
            mixed_data.len()
        );
        let mut full_request = raw_request.into_bytes();
        full_request.extend_from_slice(mixed_data);

        let result = capture
            .capture_raw_message("test_tunnel", "test-connection", "outgoing", &full_request)
            .await;
        assert!(result.is_ok());

        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0].request;

        // Verify mixed binary data is preserved exactly
        assert_eq!(request.body, mixed_data);
        assert_eq!(request.body.len(), mixed_data.len());

        // Verify the data includes all the binary bytes
        assert_eq!(request.body[0], b'H');
        assert_eq!(request.body[5], 0x00); // null byte
        assert_eq!(request.body[6], 0x01);
        assert_eq!(request.body[13], 0xff); // high byte
        assert_eq!(request.body[17], b'E');
    }

    #[tokio::test]
    async fn test_binary_body_with_different_line_endings() {
        let storage = RequestStorage::new(100);
        let websocket_storage = WebSocketMessageStorage::new(1000);
        let capture = Capture::new(storage, websocket_storage, "off", 1024);

        // Test with \r\n\r\n line endings
        let binary_body = b"\x01\x02\x03\x04\x05";
        let request_with_crlf = b"POST /test HTTP/1.1\r\nHost: example.com\r\nContent-Length: 5\r\n\r\n\x01\x02\x03\x04\x05";

        let result = capture
            .capture_raw_message(
                "test_tunnel",
                "test-connection",
                "outgoing",
                request_with_crlf,
            )
            .await;
        assert!(result.is_ok());

        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0].request;
        assert_eq!(request.body, binary_body);

        // Clear storage for next test
        capture.storage.clear().await;

        // Test with \n\n line endings
        let request_with_lf =
            b"POST /test HTTP/1.1\nHost: example.com\nContent-Length: 5\n\n\x01\x02\x03\x04\x05";

        let result = capture
            .capture_raw_message(
                "test_tunnel",
                "test-connection2",
                "outgoing",
                request_with_lf,
            )
            .await;
        assert!(result.is_ok());

        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 1);
        let request = &requests[0].request;
        assert_eq!(request.body, binary_body);
    }
}
