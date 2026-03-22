use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::capture::Capture;
use crate::config::TunnelConfig;

struct TeeReader<R> {
    inner: R,
    capture: Capture,
    tunnel_name: String,
    direction: String,
    buffer: Vec<u8>,
}

impl<R> TeeReader<R> {
    fn new(reader: R, capture: Capture, tunnel_name: String, direction: String) -> Self {
        Self {
            inner: reader,
            capture,
            tunnel_name,
            direction,
            buffer: Vec::new(),
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for TeeReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        let filled_len = buf.filled().len();

        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let new_data = &buf.filled()[filled_len..];
                if !new_data.is_empty() {
                    // Capture the data for analysis
                    this.buffer.extend_from_slice(new_data);

                    // Try to parse and capture complete messages from buffer
                    if let Err(e) = Self::process_buffer(
                        &this.capture,
                        &this.tunnel_name,
                        &this.direction,
                        &mut this.buffer,
                    ) {
                        debug!("Failed to process buffer: {}", e);
                    }
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<R: AsyncRead + Unpin> TeeReader<R> {
    fn process_buffer(
        capture: &Capture,
        tunnel_name: &str,
        direction: &str,
        buffer: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        // Try to detect and extract HTTP requests/responses
        while let Some((message, consumed)) = Self::try_extract_http_message(buffer)? {
            // Store the captured message
            tokio::spawn({
                let capture = capture.clone();
                let tunnel_name = tunnel_name.to_string();
                let direction = direction.to_string();
                let message = message.clone();
                async move {
                    if let Err(e) = capture
                        .capture_raw_message(&tunnel_name, &direction, &message)
                        .await
                    {
                        debug!("Failed to capture raw message: {}", e);
                    }
                }
            });

            // Remove consumed data from buffer
            buffer.drain(0..consumed);
        }

        // Also try WebSocket frame detection
        while let Some((message, consumed)) = Self::try_extract_websocket_frame(buffer)? {
            tokio::spawn({
                let capture = capture.clone();
                let tunnel_name = tunnel_name.to_string();
                let direction = direction.to_string();
                let message = message.clone();
                async move {
                    if let Err(e) = capture
                        .capture_websocket_message_raw(&tunnel_name, &direction, &message)
                        .await
                    {
                        debug!("Failed to capture WebSocket frame: {}", e);
                    }
                }
            });

            buffer.drain(0..consumed);
        }

        Ok(())
    }

    fn try_extract_http_message(buffer: &[u8]) -> anyhow::Result<Option<(Vec<u8>, usize)>> {
        // Look for HTTP request/response pattern
        if buffer.len() < 4 {
            return Ok(None);
        }

        // Convert to string for pattern matching
        let buffer_str = String::from_utf8_lossy(buffer);

        // Check if this looks like HTTP (starts with GET/POST/PUT/DELETE/etc or HTTP/1.x)
        let is_http = buffer_str
            .lines()
            .next()
            .map(|line| {
                line.starts_with("GET")
                    || line.starts_with("POST")
                    || line.starts_with("PUT")
                    || line.starts_with("DELETE")
                    || line.starts_with("HEAD")
                    || line.starts_with("OPTIONS")
                    || line.starts_with("PATCH")
                    || line.starts_with("HTTP/")
            })
            .unwrap_or(false);

        if !is_http {
            return Ok(None);
        }

        // Find the end of headers (\r\n\r\n)
        if let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let header_end = header_end + 4;

            // Try to determine content length
            let content_length = Self::extract_content_length(&buffer[..header_end]).unwrap_or(0);

            let total_length = header_end + content_length;

            // If we have the complete message, extract it
            if buffer.len() >= total_length {
                return Ok(Some((buffer[..total_length].to_vec(), total_length)));
            }
        }

        Ok(None)
    }

    fn try_extract_websocket_frame(buffer: &[u8]) -> anyhow::Result<Option<(Vec<u8>, usize)>> {
        // WebSocket frame format: https://tools.ietf.org/html/rfc6455#section-5.2
        if buffer.len() < 2 {
            return Ok(None);
        }

        let first_byte = buffer[0];
        let second_byte = buffer[1];

        // Check for WebSocket frame: first bit should be 0-3 (opcode), second bit should have mask bit
        let opcode = first_byte & 0x0F;
        let masked = (second_byte & 0x80) != 0;

        // Valid opcodes: 0x0 (continuation), 0x1 (text), 0x2 (binary), 0x8 (close), 0x9 (ping), 0xA (pong)
        let valid_opcode = matches!(opcode, 0x0 | 0x1 | 0x2 | 0x8 | 0x9 | 0xA);

        if !valid_opcode {
            return Ok(None);
        }

        let payload_len = (second_byte & 0x7F) as usize;
        let mut header_len = 2;

        // Extended payload length
        if payload_len == 126 {
            header_len += 2;
        } else if payload_len == 127 {
            header_len += 8;
        }

        // Masking key (if present)
        if masked {
            header_len += 4;
        }

        let total_len = header_len + payload_len;

        if buffer.len() >= total_len {
            return Ok(Some((buffer[..total_len].to_vec(), total_len)));
        }

        Ok(None)
    }

    fn extract_content_length(data: &[u8]) -> Option<usize> {
        let data_str = String::from_utf8_lossy(data);
        for line in data_str.lines() {
            if line.to_lowercase().starts_with("content-length:") {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() == 2 {
                    return parts[1].trim().parse().ok();
                }
            }
        }
        None
    }
}

pub struct Proxy {
    config: TunnelConfig,
    capture: Capture,
}

impl Proxy {
    pub fn new(config: TunnelConfig, max_stored_requests: usize, stdout_level: &str) -> Self {
        Self {
            capture: Capture::new(max_stored_requests, stdout_level),
            config,
        }
    }

    pub async fn start(&self, cancel_token: CancellationToken) -> anyhow::Result<()> {
        let socket_path = &self.config.socket_path;

        // Remove existing socket file if it exists
        if Path::new(socket_path).exists() {
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        info!("Tunnel '{}' listening on {}", self.config.name, socket_path);

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let config = self.config.clone();
                            let capture = self.capture.clone();

                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_connection(stream, config, capture).await {
                                    error!("Connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down tunnel '{}'", self.config.name);
                    break;
                }
            }
        }

        // Clean up socket file
        if Path::new(socket_path).exists() {
            std::fs::remove_file(socket_path)?;
        }

        Ok(())
    }

    async fn handle_connection(
        unix_stream: UnixStream,
        config: TunnelConfig,
        capture: Capture,
    ) -> anyhow::Result<()> {
        // Connect to target TCP port
        let target_addr = format!("127.0.0.1:{}", config.target_port);
        let tcp_stream = match TcpStream::connect(&target_addr).await {
            Ok(stream) => stream,
            Err(e) => {
                error!("Failed to connect to target {}: {}", target_addr, e);
                return Ok(());
            }
        };

        info!(
            "Established transparent forwarding: {} <-> {}",
            config.name, target_addr
        );

        // Split streams for bidirectional forwarding
        let (unix_reader, unix_writer) = unix_stream.into_split();
        let (tcp_reader, tcp_writer) = tcp_stream.into_split();

        // Create tee readers for capturing data
        let client_to_server_tee = TeeReader::new(
            unix_reader,
            capture.clone(),
            config.name.clone(),
            "→".to_string(),
        );

        let server_to_client_tee = TeeReader::new(
            tcp_reader,
            capture.clone(),
            config.name.clone(),
            "←".to_string(),
        );

        // Perform bidirectional forwarding using spawned tasks
        let (bytes_sent, bytes_received) = tokio::join!(
            async {
                let mut client_reader = client_to_server_tee;
                let mut server_writer = tcp_writer;
                tokio::io::copy(&mut client_reader, &mut server_writer).await
            },
            async {
                let mut server_reader = server_to_client_tee;
                let mut client_writer = unix_writer;
                tokio::io::copy(&mut server_reader, &mut client_writer).await
            }
        );

        match (bytes_sent, bytes_received) {
            (Ok(sent), Ok(received)) => {
                info!(
                    "Connection closed - sent: {} bytes, received: {} bytes",
                    sent, received
                );
            }
            (Err(e), _) | (_, Err(e)) => {
                error!("Forwarding error: {}", e);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    // Mock AsyncRead for testing
    struct MockReader {
        data: Vec<u8>,
        position: usize,
    }

    impl MockReader {
        fn new(data: Vec<u8>) -> Self {
            Self { data, position: 0 }
        }
    }

    impl AsyncRead for MockReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.position >= self.data.len() {
                return Poll::Ready(Ok(()));
            }

            let remaining = self.data.len() - self.position;
            let to_copy = std::cmp::min(remaining, buf.remaining());

            let end_pos = self.position + to_copy;
            buf.put_slice(&self.data[self.position..end_pos]);
            self.position = end_pos;

            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_tee_reader_creation() {
        let mock_reader = MockReader::new(vec![1, 2, 3]);
        let capture = Capture::new(100, "debug");
        let tunnel_name = "test_tunnel".to_string();
        let direction = "→".to_string();

        let tee_reader = TeeReader::new(mock_reader, capture, tunnel_name, direction);

        assert_eq!(tee_reader.tunnel_name, "test_tunnel");
        assert_eq!(tee_reader.direction, "→");
        assert!(tee_reader.buffer.is_empty());
    }

    #[tokio::test]
    async fn test_tee_reader_basic_reading() {
        let data = b"Hello, World!".to_vec();
        let mock_reader = MockReader::new(data.clone());
        let capture = Capture::new(100, "debug");
        let tee_reader = TeeReader::new(mock_reader, capture, "test".to_string(), "→".to_string());

        // Test that the TeeReader can be created and has the expected fields
        assert_eq!(tee_reader.tunnel_name, "test");
        assert_eq!(tee_reader.direction, "→");
        assert!(tee_reader.buffer.is_empty());
    }

    #[test]
    fn test_extract_http_request() {
        let http_request = b"GET / HTTP/1.1\r\nHost: example.com\r\nUser-Agent: test\r\n\r\n";
        let result = TeeReader::<MockReader>::try_extract_http_message(http_request).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, http_request);
        assert_eq!(consumed, http_request.len());
    }

    #[test]
    fn test_extract_http_response() {
        let http_response =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nHello";
        let result = TeeReader::<MockReader>::try_extract_http_message(http_response).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, http_response);
        assert_eq!(consumed, http_response.len());
    }

    #[test]
    fn test_extract_http_with_body() {
        let http_message =
            b"POST /api HTTP/1.1\r\nHost: example.com\r\nContent-Length: 12\r\n\r\nHello World!";
        let result = TeeReader::<MockReader>::try_extract_http_message(http_message).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, http_message);
        assert_eq!(consumed, http_message.len());
    }

    #[test]
    fn test_extract_http_incomplete() {
        let incomplete_http = b"GET / HTTP/1.1\r\nHost: example.com\r\n";
        let result = TeeReader::<MockReader>::try_extract_http_message(incomplete_http).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_extract_http_non_http() {
        let non_http = b"Random data that is not HTTP";
        let result = TeeReader::<MockReader>::try_extract_http_message(non_http).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_extract_websocket_text_frame() {
        // WebSocket text frame: FIN=1, opcode=1, masked=1, payload_len=5
        let frame = b"\x81\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58"; // "Hello" masked
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_binary_frame() {
        // WebSocket binary frame: FIN=1, opcode=2, masked=1, payload_len=3
        let frame = b"\x82\x83\x12\x34\x56\x78\x9a\xbc\xde"; // Binary data masked (3 bytes + 4 byte mask)
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_close_frame() {
        // WebSocket close frame: FIN=1, opcode=8, masked=1, payload_len=2
        let frame = b"\x88\x82\x12\x34\x56\x78\x9a\xbc"; // Close code masked (2 bytes + 4 byte mask)
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_ping_frame() {
        // WebSocket ping frame: FIN=1, opcode=9, masked=1, payload_len=4
        let frame = b"\x89\x84\x12\x34\x56\x78\x9a\xbc\xde\xf0"; // Ping data masked (4 bytes + 4 byte mask)
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_pong_frame() {
        // WebSocket pong frame: FIN=1, opcode=10, masked=1, payload_len=4
        let frame = b"\x8a\x84\x12\x34\x56\x78\x9a\xbc\xde\xf0"; // Pong data masked (4 bytes + 4 byte mask)
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_extended_payload() {
        // WebSocket frame with extended payload length (126) - unmasked for simplicity
        let mut frame = Vec::new();
        frame.extend_from_slice(&[0x82, 0x7e]); // FIN=1, opcode=2, masked=0, payload_len=126
        frame.extend_from_slice(&[0x00, 0x7e]); // Extended payload length: 126
        frame.extend_from_slice(&[0u8; 126]); // Payload

        let result = TeeReader::<MockReader>::try_extract_websocket_frame(&frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_incomplete() {
        let incomplete_frame = b"\x81\x85"; // Only header, no payload
        let result =
            TeeReader::<MockReader>::try_extract_websocket_frame(incomplete_frame).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_extract_websocket_invalid_opcode() {
        // Invalid opcode (0x3 is reserved)
        let frame = b"\x83\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_extract_websocket_too_short() {
        let too_short = b"\x81"; // Only 1 byte
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(too_short).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_extract_content_length() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 42\r\n\r\n";
        let result = TeeReader::<MockReader>::extract_content_length(data);

        assert_eq!(result, Some(42));
    }

    #[test]
    fn test_extract_content_length_case_insensitive() {
        let data = b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n";
        let result = TeeReader::<MockReader>::extract_content_length(data);

        assert_eq!(result, Some(100));
    }

    #[test]
    fn test_extract_content_length_no_header() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n";
        let result = TeeReader::<MockReader>::extract_content_length(data);

        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_content_length_invalid_format() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Length: invalid\r\n\r\n";
        let result = TeeReader::<MockReader>::extract_content_length(data);

        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_content_length_with_spaces() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Length:   25   \r\n\r\n";
        let result = TeeReader::<MockReader>::extract_content_length(data);

        assert_eq!(result, Some(25));
    }

    #[tokio::test]
    async fn test_process_buffer_with_http() {
        let capture = Capture::new(100, "debug");
        let tunnel_name = "test_tunnel";
        let direction = "→";
        let http_data = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let mut buffer = http_data.to_vec();

        // This should not panic and should process the HTTP message
        let result =
            TeeReader::<MockReader>::process_buffer(&capture, tunnel_name, direction, &mut buffer);

        assert!(result.is_ok());
        // Buffer should be empty after processing
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    async fn test_process_buffer_with_websocket() {
        let capture = Capture::new(100, "debug");
        let tunnel_name = "test_tunnel";
        let direction = "←";
        let ws_frame = b"\x81\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58"; // "Hello" masked
        let mut buffer = ws_frame.to_vec();

        let result =
            TeeReader::<MockReader>::process_buffer(&capture, tunnel_name, direction, &mut buffer);

        assert!(result.is_ok());
        // Buffer should be empty after processing
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    async fn test_process_buffer_with_mixed_data() {
        let capture = Capture::new(100, "debug");
        let tunnel_name = "test_tunnel";
        let direction = "→";

        // Mix HTTP request and WebSocket frame
        let http_data = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let ws_frame = b"\x81\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58";
        let mut buffer = Vec::new();
        buffer.extend_from_slice(http_data);
        buffer.extend_from_slice(ws_frame);

        let result =
            TeeReader::<MockReader>::process_buffer(&capture, tunnel_name, direction, &mut buffer);

        assert!(result.is_ok());
        // Buffer should be empty after processing both messages
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_process_buffer_with_incomplete_data() {
        let capture = Capture::new(100, "debug");
        let tunnel_name = "test_tunnel";
        let direction = "→";
        let incomplete_http = b"GET / HTTP/1.1\r\nHost: example.com";
        let mut buffer = incomplete_http.to_vec();

        let result =
            TeeReader::<MockReader>::process_buffer(&capture, tunnel_name, direction, &mut buffer);

        assert!(result.is_ok());
        // Buffer should remain unchanged for incomplete data
        assert_eq!(buffer, incomplete_http);
    }

    #[test]
    fn test_process_buffer_with_non_protocol_data() {
        let capture = Capture::new(100, "debug");
        let tunnel_name = "test_tunnel";
        let direction = "→";
        let random_data = b"This is just random data that doesn't match any protocol";
        let mut buffer = random_data.to_vec();

        let result =
            TeeReader::<MockReader>::process_buffer(&capture, tunnel_name, direction, &mut buffer);

        assert!(result.is_ok());
        // Buffer should remain unchanged for non-protocol data
        assert_eq!(buffer, random_data);
    }

    #[test]
    fn test_proxy_creation() {
        let config = TunnelConfig {
            name: "test_tunnel".to_string(),
            socket_path: "/tmp/test.sock".to_string(),
            target_port: 8080,
        };

        let proxy = Proxy::new(config, 100, "debug");

        assert_eq!(proxy.config.name, "test_tunnel");
        assert_eq!(proxy.config.target_port, 8080);
        assert_eq!(proxy.config.socket_path, "/tmp/test.sock");
    }

    #[test]
    fn test_all_http_methods() {
        let methods = ["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"];

        for method in methods.iter() {
            let http_request = format!("{} / HTTP/1.1\r\nHost: example.com\r\n\r\n", method);
            let result =
                TeeReader::<MockReader>::try_extract_http_message(http_request.as_bytes()).unwrap();

            assert!(result.is_some(), "Failed to extract {} request", method);
            let (extracted, consumed) = result.unwrap();
            assert_eq!(extracted, http_request.as_bytes());
            assert_eq!(consumed, http_request.len());
        }
    }

    #[test]
    fn test_websocket_continuation_frame() {
        // WebSocket continuation frame: FIN=1, opcode=0x0, masked=1, payload_len=5
        let frame = b"\x80\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58"; // Continuation data masked
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_websocket_unmasked_frame() {
        // WebSocket frame without masking: FIN=1, opcode=1, masked=0, payload_len=5
        let frame = b"\x81\x05Hello";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();

        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }
}
