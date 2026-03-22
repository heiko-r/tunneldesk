use crate::storage::{
    RequestStorage, StoredRequest, StoredResponse, StoredWebSocketMessage, WebSocketMessageStorage,
    WebSocketMessageType,
};
use std::collections::HashMap;
use tracing::info;

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

#[derive(Clone)]
pub struct Capture {
    pub storage: RequestStorage,
    pub websocket_storage: WebSocketMessageStorage,
    log_level: LogLevel,
}

impl Capture {
    pub fn new(max_stored_requests: usize, stdout_level: &str) -> Self {
        Self {
            storage: RequestStorage::new(max_stored_requests),
            websocket_storage: WebSocketMessageStorage::new(max_stored_requests * 10), // Store more messages than requests
            log_level: LogLevel::from(stdout_level),
        }
    }

    pub async fn capture_raw_message(
        &self,
        tunnel_name: &str,
        direction: &str,
        raw_message: &[u8],
    ) -> anyhow::Result<()> {
        let message_str = String::from_utf8_lossy(raw_message);
        let lines: Vec<&str> = message_str.lines().collect();

        if !lines.is_empty() {
            let first_line = lines[0];

            if first_line.starts_with("GET")
                || first_line.starts_with("POST")
                || first_line.starts_with("PUT")
                || first_line.starts_with("DELETE")
                || first_line.starts_with("HEAD")
                || first_line.starts_with("OPTIONS")
                || first_line.starts_with("PATCH")
            {
                // HTTP Request
                let parts: Vec<&str> = first_line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let method = parts[0].to_string();
                    let path = parts[1].to_string();

                    // Parse headers
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

                    // Extract body
                    let body = if body_start < lines.len() {
                        lines[body_start..].join("\n").into_bytes()
                    } else {
                        Vec::new()
                    };

                    // Log based on level
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
                                "[{}] Body: {:?}",
                                tunnel_name,
                                String::from_utf8_lossy(&body)
                            );
                        }
                    }

                    // Store as request
                    let stored_request = StoredRequest {
                        id: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        tunnel_name: tunnel_name.to_string(),
                        method,
                        url: path,
                        headers,
                        body,
                        raw_request: raw_message.to_vec(),
                    };

                    self.storage.store_request(stored_request).await;
                    return Ok(());
                }
            } else if first_line.starts_with("HTTP/") {
                // HTTP Response
                let parts: Vec<&str> = first_line.split_whitespace().collect();
                if parts.len() >= 2
                    && let Ok(status) = parts[1].parse::<u16>()
                {
                    // Parse headers
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

                    // Extract body
                    let body = if body_start < lines.len() {
                        lines[body_start..].join("\n").into_bytes()
                    } else {
                        Vec::new()
                    };

                    // Log based on level
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
                                "[{}] Body: {:?}",
                                tunnel_name,
                                String::from_utf8_lossy(&body)
                            );
                        }
                    }

                    // Store as response
                    let stored_response = StoredResponse {
                        request_id: "unknown".to_string(),
                        timestamp: chrono::Utc::now(),
                        status,
                        headers,
                        body,
                        raw_response: raw_message.to_vec(),
                    };

                    self.storage.store_response(stored_response).await;
                    return Ok(());
                }
            }
        }

        // If not HTTP, store as raw binary data
        info!(
            "Raw message [{}]: {} bytes - direction: {}",
            tunnel_name,
            raw_message.len(),
            direction
        );
        Ok(())
    }

    pub async fn capture_websocket_message_raw(
        &self,
        tunnel_name: &str,
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

        let stored_message = StoredWebSocketMessage {
            id: message_id,
            timestamp,
            tunnel_name: tunnel_name.to_string(),
            upgrade_request_id: "unknown".to_string(), // This would need to be matched with upgrade
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
                let payload_preview = if unmasked_payload.len() > 100 {
                    format!("{}...", String::from_utf8_lossy(&unmasked_payload[..100]))
                } else {
                    String::from_utf8_lossy(&unmasked_payload).to_string()
                };
                info!(
                    "[{}] WS {} - opcode: {}, payload: {}",
                    tunnel_name, direction, opcode, payload_preview
                );
            }
        }

        Ok(())
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
        let capture = Capture::new(100, "full");
        assert!(matches!(capture.log_level, LogLevel::Full));
    }

    #[tokio::test]
    async fn test_capture_raw_message_get_request() {
        let capture = Capture::new(100, "off");
        let raw_message = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\nContent-Type: application/json\r\n\r\n{\"key\":\"value\"}";

        let result = capture
            .capture_raw_message("test_tunnel", "incoming", raw_message)
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
        let capture = Capture::new(100, "off");
        let raw_message = b"POST /api/data HTTP/1.1\r\nHost: api.example.com\r\nContent-Length: 12\r\n\r\nHello World!";

        let result = capture
            .capture_raw_message("api_tunnel", "outgoing", raw_message)
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
        let capture = Capture::new(100, "off");
        let raw_message = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"success\":true}";

        let result = capture
            .capture_raw_message("test_tunnel", "incoming", raw_message)
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
        let capture = Capture::new(100, "off");
        let raw_message = b"Invalid HTTP message";

        let result = capture
            .capture_raw_message("test_tunnel", "incoming", raw_message)
            .await;
        assert!(result.is_ok());

        // Should not store any requests
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_raw_message_empty() {
        let capture = Capture::new(100, "off");
        let raw_message = b"";

        let result = capture
            .capture_raw_message("test_tunnel", "incoming", raw_message)
            .await;
        assert!(result.is_ok());

        // Should not store any requests
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_raw_message_request_without_body() {
        let capture = Capture::new(100, "off");
        let raw_message = b"GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n";

        let result = capture
            .capture_raw_message("test_tunnel", "incoming", raw_message)
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
        let capture = Capture::new(100, "off");

        // Create a simple WebSocket text frame (unmasked, opcode 0x1)
        let mut frame = Vec::new();
        frame.push(0x81); // FIN=1, opcode=0x1 (text)
        frame.push(0x10); // Payload length = 16 (for "Hello WebSocket!")
        frame.extend_from_slice(b"Hello WebSocket!");

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "outgoing", &frame)
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
        let capture = Capture::new(100, "off");

        // Create a WebSocket binary frame (unmasked, opcode 0x2)
        let mut frame = Vec::new();
        frame.push(0x82); // FIN=1, opcode=0x2 (binary)
        frame.push(0x04); // Payload length = 4
        frame.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "incoming", &frame)
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
        let capture = Capture::new(100, "off");

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
            .capture_websocket_message_raw("ws_tunnel", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.payload, b"Hello");
    }

    #[tokio::test]
    async fn test_capture_websocket_message_too_short() {
        let capture = Capture::new(100, "off");
        let frame = vec![0x81]; // Incomplete frame

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        // Should not store any messages
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_websocket_message_incomplete_payload() {
        let capture = Capture::new(100, "off");

        // Frame header indicates 10 bytes but only provides 5
        let mut frame = Vec::new();
        frame.push(0x81); // FIN=1, opcode=0x1 (text)
        frame.push(0x0A); // Payload length = 10
        frame.extend_from_slice(b"Hello"); // Only 5 bytes of payload

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        // Should not store any messages
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 0);
    }

    #[tokio::test]
    async fn test_capture_websocket_message_extended_payload_length() {
        let capture = Capture::new(100, "off");

        // Create a frame with 126-byte payload length (2-byte extended length)
        let mut frame = Vec::new();
        frame.push(0x81); // FIN=1, opcode=0x1 (text)
        frame.push(0x7E); // Masked=0, payload length = 126 (extended)
        frame.extend_from_slice(&[0x00, 0x7E]); // Extended payload length = 126
        frame.extend_from_slice(&[0x42; 126]); // 126 bytes of payload

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "outgoing", &frame)
            .await;
        assert!(result.is_ok());

        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload.len(), 126);
    }

    #[tokio::test]
    async fn test_capture_websocket_message_unknown_opcode() {
        let capture = Capture::new(100, "off");

        // Create a frame with unknown opcode (should default to Binary)
        let mut frame = Vec::new();
        frame.push(0x88); // FIN=1, opcode=0x8 (close)
        frame.push(0x02); // Payload length = 2
        frame.extend_from_slice(&[0x03, 0xE8]); // Close code 1000

        let result = capture
            .capture_websocket_message_raw("ws_tunnel", "outgoing", &frame)
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

    #[test]
    fn test_all_http_methods() {
        let methods = ["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"];

        for method in methods {
            let capture = Capture::new(100, "off");
            let raw_message = format!("{} /test HTTP/1.1\r\nHost: example.com\r\n\r\n", method);

            // Use tokio runtime to test async function in sync test
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(async {
                capture
                    .capture_raw_message("test_tunnel", "incoming", raw_message.as_bytes())
                    .await
            });

            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_storage_limits() {
        let capture = Capture::new(2, "off"); // Very small storage limit

        // Store 3 requests
        for i in 0..3 {
            let raw_message = format!("GET /test{} HTTP/1.1\r\nHost: example.com\r\n\r\n", i);
            capture
                .capture_raw_message("test_tunnel", "incoming", raw_message.as_bytes())
                .await
                .unwrap();
        }

        // Should only keep the 2 most recent requests
        let requests = capture.storage.get_all_requests().await;
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn test_websocket_storage_limits() {
        let capture = Capture::new(1, "off"); // This creates websocket storage with 10 messages limit

        // Store 11 WebSocket messages
        for i in 0..11 {
            let frame = vec![
                0x81,    // FIN=1, opcode=0x1 (text)
                0x01,    // Payload length = 1
                i as u8, // Payload
            ];

            capture
                .capture_websocket_message_raw("ws_tunnel", "outgoing", &frame)
                .await
                .unwrap();
        }

        // Should only keep the 10 most recent messages
        let messages = capture.websocket_storage.get_all_messages().await;
        assert_eq!(messages.len(), 10);
    }
}
