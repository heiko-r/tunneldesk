use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

#[cfg(unix)]
use tokio::net::UnixListener;

use crate::capture::Capture;
use crate::config::TunnelConfig;

/// Streaming HTTP/WebSocket capture state machine.
///
/// The TeeReader wraps an inner `AsyncRead` and forwards all bytes unchanged to
/// the caller (used as a source for `tokio::io::copy`). As a side-channel it
/// accumulates bytes needed to reconstruct and capture HTTP messages or
/// WebSocket frames, respecting the configured body-size cap.
#[derive(Debug)]
enum TeeReaderState {
    /// Scanning the accumulation buffer for the HTTP header terminator
    /// (`\r\n\r\n`). `scan_pos` is the earliest buffer index not yet scanned,
    /// so each poll resumes where it left off rather than rescanning from the
    /// beginning (avoids O(n²) work on large responses).
    FindingHeaders { scan_pos: usize },

    /// HTTP headers have been parsed.  Accumulating body bytes until
    /// `capture_limit` bytes are available, then the capture task is dispatched.
    CollectingBody {
        /// Offset into `buf` where the body begins.
        header_end: usize,
        /// Declared `Content-Length` value (0 if absent).
        content_length: usize,
        /// `min(content_length, max_body_size)` – the number of body bytes to
        /// capture and buffer.
        capture_limit: usize,
    },

    /// Capture has been dispatched.  The message body extends beyond
    /// `max_body_size`; we are counting down the uncaptured tail bytes without
    /// buffering them so that the connection continues to be proxied normally.
    DrainBody { bytes_remaining: usize },
}

struct TeeReader<R> {
    inner: R,
    capture: Capture,
    tunnel_name: String,
    connection_id: String,
    direction: String,
    /// Maximum body bytes to store, from `config.capture.max_request_body_size`.
    max_body_size: usize,
    /// Scratch buffer used only in `FindingHeaders` and `CollectingBody`.
    /// Cleared when entering `DrainBody` to release memory promptly.
    buf: Vec<u8>,
    state: TeeReaderState,
    /// Set to `true` once a WebSocket upgrade handshake is detected on this
    /// connection.  Before the upgrade, bytes are only ever interpreted as HTTP.
    /// After the upgrade, bytes are only ever interpreted as WebSocket frames.
    websocket_upgraded: bool,
}

impl<R> TeeReader<R> {
    fn new(
        reader: R,
        capture: Capture,
        tunnel_name: String,
        connection_id: String,
        direction: String,
        max_body_size: usize,
    ) -> Self {
        Self {
            inner: reader,
            capture,
            tunnel_name,
            connection_id,
            direction,
            max_body_size,
            buf: Vec::new(),
            state: TeeReaderState::FindingHeaders { scan_pos: 0 },
            websocket_upgraded: false,
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
                    // Capture side-channel: does not affect forwarded bytes.
                    this.advance_state(new_data);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<R: AsyncRead + Unpin> TeeReader<R> {
    /// Advance the capture state machine with the bytes that were just read.
    ///
    /// All bytes have already been placed in the `ReadBuf` by `poll_read` and
    /// will be forwarded to `tokio::io::copy` regardless of what happens here.
    fn advance_state(&mut self, new_data: &[u8]) {
        // `remaining` tracks the portion of `new_data` not yet consumed by the
        // current state.  In `FindingHeaders`/`CollectingBody` the bytes are
        // appended to `self.buf` and `remaining` is reset to an empty slice.
        // In `DrainBody` the bytes are counted without allocation.
        let mut remaining = new_data;

        loop {
            match self.state {
                // ── Finding HTTP header terminator ────────────────────────
                TeeReaderState::FindingHeaders { scan_pos } => {
                    self.buf.extend_from_slice(remaining);
                    remaining = b"";

                    if self.websocket_upgraded {
                        // Post-upgrade: the stream carries WebSocket frames only.
                        while let Ok(Some((frame, consumed))) =
                            Self::try_extract_websocket_frame(&self.buf)
                        {
                            self.dispatch_websocket_frame(frame);
                            self.buf.drain(0..consumed);
                        }
                        self.state = TeeReaderState::FindingHeaders {
                            scan_pos: self.buf.len().saturating_sub(3),
                        };
                        break;
                    }

                    // Pre-upgrade: the stream carries HTTP messages only.
                    // If the buffer starts with non-HTTP bytes (e.g. stale bytes
                    // left from a previous message), skip forward to the next HTTP
                    // start so that the following request is not missed.
                    let effective_scan_pos = if Self::starts_with_http(&self.buf) {
                        scan_pos
                    } else if let Some(pos) = Self::find_http_start(&self.buf) {
                        self.buf.drain(0..pos);
                        0 // rescan from new start
                    } else {
                        // No HTTP data yet – wait for more bytes.
                        self.state = TeeReaderState::FindingHeaders { scan_pos: 0 };
                        break;
                    };

                    // Scan for \r\n\r\n starting at `effective_scan_pos` (overlap
                    // by 3 bytes so the delimiter is caught when it spans reads).
                    let search_from = effective_scan_pos.saturating_sub(3);
                    let found = self.buf[search_from..]
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n");

                    match found {
                        None => {
                            self.state = TeeReaderState::FindingHeaders {
                                scan_pos: self.buf.len().saturating_sub(3),
                            };
                            break;
                        }
                        Some(rel_pos) => {
                            let header_end = search_from + rel_pos + 4;
                            let content_length =
                                Self::extract_content_length(&self.buf[..header_end]).unwrap_or(0);
                            let capture_limit = content_length.min(self.max_body_size);
                            self.state = TeeReaderState::CollectingBody {
                                header_end,
                                content_length,
                                capture_limit,
                            };
                            // Fall through to CollectingBody on the next
                            // iteration – the buffer may already hold enough
                            // body bytes to dispatch immediately.
                        }
                    }
                }

                // ── Collecting body bytes up to capture_limit ─────────────
                TeeReaderState::CollectingBody {
                    header_end,
                    content_length,
                    capture_limit,
                } => {
                    self.buf.extend_from_slice(remaining);
                    remaining = b"";

                    let body_in_buf = self.buf.len().saturating_sub(header_end);

                    if content_length == 0 {
                        // No body (or unknown length): capture headers only.
                        let headers = &self.buf[..header_end];
                        self.dispatch_http_message(headers.to_vec());
                        // Detect WebSocket upgrade so future bytes are parsed as
                        // frames rather than HTTP.  The upgrade request (client →
                        // server) and the 101 response (server → client) are each
                        // seen by their respective TeeReader, so both independently
                        // set this flag at the right moment.
                        if Self::is_websocket_upgrade_headers(headers)
                            || Self::is_101_switching_protocols(headers)
                        {
                            self.websocket_upgraded = true;
                        }
                        self.buf.drain(0..header_end);
                        self.state = TeeReaderState::FindingHeaders { scan_pos: 0 };
                        // Loop: the buffer may contain the next message.
                    } else if body_in_buf < capture_limit {
                        // Still waiting for more body bytes.
                        break;
                    } else {
                        // Enough body to dispatch.
                        let capture_end = header_end + capture_limit;
                        self.dispatch_http_message(self.buf[..capture_end].to_vec());

                        let total_message = header_end + content_length;
                        if self.buf.len() >= total_message {
                            // Full message already in buffer; drain and continue.
                            self.buf.drain(0..total_message);
                            self.state = TeeReaderState::FindingHeaders { scan_pos: 0 };
                            // Loop: leftover bytes may start the next message.
                        } else {
                            // Body tail not yet received; switch to drain mode.
                            let bytes_remaining = total_message - self.buf.len();
                            self.buf.clear(); // Release memory – no longer needed.
                            self.state = TeeReaderState::DrainBody { bytes_remaining };
                            break;
                        }
                    }
                }

                // ── Counting remaining body bytes without buffering ────────
                TeeReaderState::DrainBody { bytes_remaining } => {
                    let consumed = remaining.len().min(bytes_remaining);
                    remaining = &remaining[consumed..];
                    let new_remaining = bytes_remaining - consumed;

                    if new_remaining == 0 {
                        self.state = TeeReaderState::FindingHeaders { scan_pos: 0 };
                        // Loop: leftover bytes in `remaining` start the next message.
                    } else {
                        self.state = TeeReaderState::DrainBody {
                            bytes_remaining: new_remaining,
                        };
                        break;
                    }
                }
            }
        }
    }

    fn dispatch_http_message(&self, message: Vec<u8>) {
        tokio::spawn({
            let capture = self.capture.clone();
            let tunnel_name = self.tunnel_name.clone();
            let connection_id = self.connection_id.clone();
            let direction = self.direction.clone();
            async move {
                if let Err(e) = capture
                    .capture_raw_message(&tunnel_name, &connection_id, &direction, &message)
                    .await
                {
                    debug!("Failed to capture HTTP message: {}", e);
                }
            }
        });
    }

    fn dispatch_websocket_frame(&self, frame: Vec<u8>) {
        tokio::spawn({
            let capture = self.capture.clone();
            let tunnel_name = self.tunnel_name.clone();
            let connection_id = self.connection_id.clone();
            let direction = self.direction.clone();
            async move {
                if let Err(e) = capture
                    .capture_websocket_message_raw(&tunnel_name, &connection_id, &direction, &frame)
                    .await
                {
                    debug!("Failed to capture WebSocket frame: {}", e);
                }
            }
        });
    }

    /// Returns `true` if `buf` starts with an HTTP method token or `HTTP/`.
    fn starts_with_http(buf: &[u8]) -> bool {
        let first_line = String::from_utf8_lossy(buf)
            .lines()
            .next()
            .map(|s| s.to_string())
            .unwrap_or_default();
        crate::capture::is_http_request_line(&first_line) || first_line.starts_with("HTTP/")
    }

    /// Scans `buf` for the first byte position where an HTTP request line or
    /// response line starts.  Returns `None` when no such position exists.
    ///
    /// Used to skip stale non-HTTP bytes that may precede the next HTTP message
    /// (e.g. residual bytes from a previous message with unknown body length).
    fn find_http_start(buf: &[u8]) -> Option<usize> {
        const PREFIXES: &[&[u8]] = &[
            b"GET ",
            b"POST ",
            b"PUT ",
            b"DELETE ",
            b"HEAD ",
            b"OPTIONS ",
            b"PATCH ",
            b"HTTP/",
        ];
        for i in 0..buf.len() {
            for prefix in PREFIXES {
                if buf[i..].starts_with(prefix) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Returns `true` if `headers` contain both `Upgrade: websocket` and
    /// `Connection: … upgrade …` (case-insensitive), indicating a WebSocket
    /// upgrade request.
    fn is_websocket_upgrade_headers(headers: &[u8]) -> bool {
        let text = String::from_utf8_lossy(headers);
        let has_upgrade = text.lines().any(|l| {
            let lower = l.to_lowercase();
            lower.starts_with("upgrade:") && lower.contains("websocket")
        });
        let has_connection = text.lines().any(|l| {
            let lower = l.to_lowercase();
            lower.starts_with("connection:") && lower.contains("upgrade")
        });
        has_upgrade && has_connection
    }

    /// Returns `true` if `headers` is an HTTP 101 Switching Protocols response.
    fn is_101_switching_protocols(headers: &[u8]) -> bool {
        String::from_utf8_lossy(headers)
            .lines()
            .next()
            .and_then(|l| l.split(' ').nth(1))
            .map(|code| code == "101")
            .unwrap_or(false)
    }

    fn try_extract_websocket_frame(buffer: &[u8]) -> anyhow::Result<Option<(Vec<u8>, usize)>> {
        // WebSocket frame format: https://tools.ietf.org/html/rfc6455#section-5.2
        if buffer.len() < 2 {
            return Ok(None);
        }

        let first_byte = buffer[0];
        let second_byte = buffer[1];

        let opcode = first_byte & 0x0F;
        let masked = (second_byte & 0x80) != 0;

        // Valid opcodes: 0x0 (continuation), 0x1 (text), 0x2 (binary),
        //               0x8 (close), 0x9 (ping), 0xA (pong)
        let valid_opcode = matches!(opcode, 0x0 | 0x1 | 0x2 | 0x8 | 0x9 | 0xA);

        if !valid_opcode {
            return Ok(None);
        }

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

        if buffer.len() >= total_len {
            return Ok(Some((buffer[..total_len].to_vec(), total_len)));
        }

        Ok(None)
    }

    fn extract_content_length(data: &[u8]) -> Option<usize> {
        let data_str = String::from_utf8_lossy(data);
        for line in data_str.lines() {
            if line.to_lowercase().starts_with("content-length:") {
                let parts: Vec<&str> = line.splitn(2, ':').collect();
                if parts.len() == 2 {
                    return parts[1].trim().parse().ok();
                }
            }
        }
        None
    }
}

/// Listens on a Unix domain socket and forwards each connection to a local TCP
/// port, capturing HTTP and WebSocket traffic in both directions.
pub struct Proxy {
    config: TunnelConfig,
    capture: Capture,
    max_body_size: usize,
}

impl Proxy {
    /// Creates a new `Proxy` for the tunnel described by `config`.
    ///
    /// * `max_body_size` — bodies larger than this are truncated before storage.
    /// * `log_body_limit` — bodies larger than this are truncated before logging.
    pub fn new(
        config: TunnelConfig,
        request_storage: Arc<crate::storage::RequestStorage>,
        websocket_storage: Arc<crate::storage::WebSocketMessageStorage>,
        stdout_level: &str,
        max_body_size: usize,
        log_body_limit: usize,
    ) -> Self {
        Self {
            capture: Capture::new(
                (*request_storage).clone(),
                (*websocket_storage).clone(),
                stdout_level,
                log_body_limit,
            ),
            config,
            max_body_size,
        }
    }

    /// Binds to the Unix socket path in `config`, accepts connections in a loop,
    /// and stops when `cancel_token` is cancelled.
    pub async fn start(&self, cancel_token: CancellationToken) -> anyhow::Result<()> {
        let socket_path = &self.config.socket_path;

        if Path::new(socket_path).exists() {
            std::fs::remove_file(socket_path)?;
        }

        #[cfg(unix)]
        {
            let listener = UnixListener::bind(socket_path)?;
            info!("Tunnel '{}' listening on {}", self.config.name, socket_path);
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, _)) => self.spawn_connection(stream),
                            Err(e) => error!("Failed to accept connection: {}", e),
                        }
                    }
                    _ = cancel_token.cancelled() => {
                        info!("Shutting down tunnel '{}'", self.config.name);
                        break;
                    }
                }
            }
        }

        #[cfg(windows)]
        {
            let mut stream_rx = windows_accept_thread(socket_path)?;
            info!("Tunnel '{}' listening on {}", self.config.name, socket_path);
            loop {
                tokio::select! {
                    result = stream_rx.recv() => {
                        match result {
                            Some(Ok(stream)) => self.spawn_connection(stream),
                            Some(Err(e)) => error!("Failed to accept connection: {}", e),
                            None => break,
                        }
                    }
                    _ = cancel_token.cancelled() => {
                        info!("Shutting down tunnel '{}'", self.config.name);
                        break;
                    }
                }
            }
        }

        if Path::new(socket_path).exists() {
            std::fs::remove_file(socket_path)?;
        }

        Ok(())
    }

    fn spawn_connection<S>(&self, stream: S)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    {
        let config = self.config.clone();
        let capture = self.capture.clone();
        let max_body_size = self.max_body_size;
        tokio::spawn(async move {
            if let Err(e) = Self::handle_connection(stream, config, capture, max_body_size).await {
                error!("Connection error: {}", e);
            }
        });
    }

    async fn handle_connection<S>(
        unix_stream: S,
        config: TunnelConfig,
        capture: Capture,
        max_body_size: usize,
    ) -> anyhow::Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
    {
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

        let (unix_reader, unix_writer) = tokio::io::split(unix_stream);
        let (tcp_reader, tcp_writer) = tcp_stream.into_split();

        let connection_id = format!("{}-{}", config.name, uuid::Uuid::new_v4());

        let client_to_server_tee = TeeReader::new(
            unix_reader,
            capture.clone(),
            config.name.clone(),
            connection_id.clone(),
            "→".to_string(),
            max_body_size,
        );

        let server_to_client_tee = TeeReader::new(
            tcp_reader,
            capture.clone(),
            config.name.clone(),
            connection_id,
            "←".to_string(),
            max_body_size,
        );

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

/// Spawns a background OS thread that calls `accept()` in a blocking loop and
/// sends each accepted connection as a [`tokio::io::DuplexStream`] through the
/// returned channel.  Dropping the receiver signals the thread to stop.
///
/// We use this on Windows because `tokio::net::UnixListener` is not available
/// there (`cfg_net_unix!` is `#[cfg(all(unix, …))]`).  `socket2` supports
/// AF_UNIX sockets on Windows 10 1803+ / Windows 11.
#[cfg(windows)]
fn windows_accept_thread(
    socket_path: &str,
) -> std::io::Result<tokio::sync::mpsc::Receiver<std::io::Result<tokio::io::DuplexStream>>> {
    use socket2::{Domain, SockAddr, Socket, Type};

    let listener = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    listener.bind(&SockAddr::unix(socket_path)?)?;
    listener.listen(128)?;

    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let handle = tokio::runtime::Handle::current();

    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((stream, _)) => match windows_bridge(stream, handle.clone()) {
                    Ok(duplex) => {
                        if handle.block_on(tx.send(Ok(duplex))).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        if handle.block_on(tx.send(Err(e))).is_err() {
                            break;
                        }
                    }
                },
                Err(e) => {
                    let _ = handle.block_on(tx.send(Err(e)));
                    break;
                }
            }
        }
    });

    Ok(rx)
}

/// Wraps a blocking [`socket2::Socket`] (an accepted AF_UNIX connection) in a
/// [`tokio::io::DuplexStream`] so that it can be used with the generic async
/// proxy code.
///
/// Two OS threads are spawned per connection:
/// * **Thread 1** reads from the socket and writes into the async duplex end.
/// * **Thread 2** reads from the async duplex end and writes to the socket.
///
/// `Handle::block_on` is called from the *OS* threads (not from within tokio),
/// which is explicitly supported — it parks the calling thread until the async
/// future completes, using tokio's runtime for scheduling.
#[cfg(windows)]
fn windows_bridge(
    stream: socket2::Socket,
    handle: tokio::runtime::Handle,
) -> std::io::Result<tokio::io::DuplexStream> {
    use std::io::{Read, Write};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (client, server) = tokio::io::duplex(65536);
    let (mut server_read, mut server_write) = tokio::io::split(server);

    let read_sock = stream.try_clone()?;
    let write_sock = stream;

    // Thread 1: socket → async (fills the client's read buffer)
    let h1 = handle.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        let mut sock = read_sock;
        loop {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if h1.block_on(server_write.write_all(&buf[..n])).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Thread 2: async → socket (drains the client's write buffer)
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        let mut sock = write_sock;
        loop {
            match handle.block_on(server_read.read(&mut buf)) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sock.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use tokio::io::AsyncReadExt;

    struct MockReader {
        data: Vec<u8>,
        position: usize,
        /// If set, return at most this many bytes per poll.
        chunk_size: Option<usize>,
    }

    impl MockReader {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data,
                position: 0,
                chunk_size: None,
            }
        }

        fn with_chunk_size(data: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                data,
                position: 0,
                chunk_size: Some(chunk_size),
            }
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
            let to_copy = match self.chunk_size {
                Some(max) => remaining.min(max).min(buf.remaining()),
                None => remaining.min(buf.remaining()),
            };

            let end_pos = self.position + to_copy;
            buf.put_slice(&self.data[self.position..end_pos]);
            self.position = end_pos;

            Poll::Ready(Ok(()))
        }
    }

    fn make_capture() -> (Capture, Arc<crate::storage::RequestStorage>) {
        let storage = Arc::new(crate::storage::RequestStorage::new(100));
        let websocket_storage = Arc::new(crate::storage::WebSocketMessageStorage::new(1000));
        let capture = Capture::new(
            (*storage).clone(),
            (*websocket_storage).clone(),
            "debug",
            1024,
        );
        (capture, storage)
    }

    fn make_tee_reader(data: Vec<u8>, max_body_size: usize) -> TeeReader<MockReader> {
        let (capture, _) = make_capture();
        TeeReader::new(
            MockReader::new(data),
            capture,
            "test".to_string(),
            "test-connection".to_string(),
            "→".to_string(),
            max_body_size,
        )
    }

    fn make_tee_reader_chunked(
        data: Vec<u8>,
        max_body_size: usize,
        chunk_size: usize,
    ) -> TeeReader<MockReader> {
        let (capture, _) = make_capture();
        TeeReader::new(
            MockReader::with_chunk_size(data, chunk_size),
            capture,
            "test".to_string(),
            "test-connection".to_string(),
            "→".to_string(),
            max_body_size,
        )
    }

    /// Drive a TeeReader to completion and return all forwarded bytes.
    async fn read_all(reader: &mut TeeReader<MockReader>) -> Vec<u8> {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).await.unwrap();
        output
    }

    // ── TeeReader construction ────────────────────────────────────────────

    #[tokio::test]
    async fn test_tee_reader_creation() {
        let (capture, _) = make_capture();
        let tee_reader = TeeReader::new(
            MockReader::new(vec![1, 2, 3]),
            capture,
            "test_tunnel".to_string(),
            "test-connection".to_string(),
            "→".to_string(),
            65536,
        );

        assert_eq!(tee_reader.tunnel_name, "test_tunnel");
        assert_eq!(tee_reader.direction, "→");
        assert_eq!(tee_reader.max_body_size, 65536);
        assert!(tee_reader.buf.is_empty());
        assert!(matches!(
            tee_reader.state,
            TeeReaderState::FindingHeaders { scan_pos: 0 }
        ));
    }

    #[tokio::test]
    async fn test_tee_reader_basic_reading() {
        let data = b"Hello, World!".to_vec();
        let mut tee_reader = make_tee_reader(data.clone(), 65536);
        let output = read_all(&mut tee_reader).await;
        // All bytes must be forwarded regardless of whether they are HTTP.
        assert_eq!(output, data);
    }

    // ── Forwarding correctness ────────────────────────────────────────────

    #[tokio::test]
    async fn test_all_bytes_forwarded_small_body() {
        let body = b"Hello";
        let msg = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let mut reader = make_tee_reader(msg.as_bytes().to_vec(), 65536);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded, msg.as_bytes());
    }

    #[tokio::test]
    async fn test_all_bytes_forwarded_large_body() {
        // Body exceeds max_body_size – all bytes must still be forwarded.
        let body = vec![0xAB_u8; 2 * 1024 * 1024]; // 2 MB
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        let mut data = header.as_bytes().to_vec();
        data.extend_from_slice(&body);
        let total_len = data.len();

        let mut reader = make_tee_reader(data, 64 * 1024); // 64 KB cap
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded.len(), total_len);
    }

    // ── State machine: body size limiting ────────────────────────────────

    #[tokio::test]
    async fn test_body_below_limit_state() {
        // After reading a complete short response the state machine should have
        // reset to FindingHeaders (ready for the next message).
        let body = b"Hello";
        let msg = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let mut reader = make_tee_reader(msg.as_bytes().to_vec(), 65536);
        read_all(&mut reader).await;
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
        assert!(reader.buf.is_empty());
    }

    #[tokio::test]
    async fn test_body_at_limit_state() {
        let body = vec![b'X'; 100];
        let msg_bytes = {
            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
            let mut v = header.as_bytes().to_vec();
            v.extend_from_slice(&body);
            v
        };
        let mut reader = make_tee_reader(msg_bytes, 100);
        read_all(&mut reader).await;
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
        assert!(reader.buf.is_empty());
    }

    #[tokio::test]
    async fn test_body_exceeds_limit_state() {
        // Body is larger than max_body_size; after fully reading the connection
        // the state machine should have reset to FindingHeaders.
        let body = vec![b'Z'; 500];
        let msg_bytes = {
            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n");
            let mut v = header.as_bytes().to_vec();
            v.extend_from_slice(&body);
            v
        };
        let mut reader = make_tee_reader(msg_bytes, 100); // cap at 100 bytes
        read_all(&mut reader).await;
        // All body bytes drained; state reset.
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
        assert!(reader.buf.is_empty());
    }

    #[tokio::test]
    async fn test_buf_capped_at_max_body_size_during_drain() {
        // While draining the body tail the buf should have been cleared.
        let body = vec![b'Y'; 1000];
        let _ = {
            let header = "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n";
            let mut v = header.as_bytes().to_vec();
            v.extend_from_slice(&body);
            v
        };
        // Use a chunked reader so we can inspect intermediate state.
        let cap = 200_usize;
        // Feed header + cap bytes → should dispatch capture and enter DrainBody.
        let header = b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n";
        let partial: Vec<u8> = header
            .iter()
            .chain(vec![b'Y'; cap].iter())
            .copied()
            .collect();

        let (capture, _) = make_capture();
        let mut reader = TeeReader::new(
            MockReader::new(partial.clone()),
            capture,
            "test".to_string(),
            "conn".to_string(),
            "→".to_string(),
            cap,
        );
        read_all(&mut reader).await;

        // After reading header + exactly cap body bytes, capture dispatched and
        // state should be DrainBody (800 more bytes expected).
        match reader.state {
            TeeReaderState::DrainBody { bytes_remaining } => {
                assert_eq!(bytes_remaining, 1000 - cap);
            }
            other => panic!("Expected DrainBody, got {:?}", other),
        }
        // buf must be empty – no accumulation during drain.
        assert!(reader.buf.is_empty());
    }

    // ── State machine: pipelined messages ────────────────────────────────

    #[tokio::test]
    async fn test_pipelined_http_requests() {
        // Two complete requests in one buffer; both should be captured and the
        // state machine should reset cleanly after each.
        let req1 = b"GET /a HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let req2 = b"GET /b HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let mut data = req1.to_vec();
        data.extend_from_slice(req2);

        let mut reader = make_tee_reader(data.clone(), 65536);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded, data);
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
        assert!(reader.buf.is_empty());
    }

    #[tokio::test]
    async fn test_pipelined_requests_with_bodies() {
        let req1 = b"POST /a HTTP/1.1\r\nContent-Length: 4\r\n\r\ndata";
        let req2 = b"POST /b HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc";
        let mut data = req1.to_vec();
        data.extend_from_slice(req2);

        let mut reader = make_tee_reader(data.clone(), 65536);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded, data);
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
        assert!(reader.buf.is_empty());
    }

    // ── State machine: multi-read scenarios ──────────────────────────────

    #[tokio::test]
    async fn test_header_delimiter_split_across_reads() {
        // The \r\n\r\n delimiter is split: first chunk ends with \r\n\r,
        // second chunk starts with \n.  scan_pos overlap must catch this.
        let msg = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        // Split just before the last \n of the double-CRLF.
        // The header terminator spans: ...r\r\n\r  |  \nhello
        // Find \r\n\r\n manually
        let delim_pos = msg
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("should contain \\r\\n\\r\\n");
        let split = delim_pos + 3; // split in the middle of \r\n\r\n

        let part1 = &msg[..split];
        let part2 = &msg[split..];

        // Build a reader that delivers part1 then part2 in separate poll_reads.
        let mut combined = part1.to_vec();
        combined.extend_from_slice(part2);

        let mut reader = make_tee_reader_chunked(combined.clone(), 65536, split);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded, msg as &[u8]);
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
    }

    #[tokio::test]
    async fn test_large_body_multiple_reads_all_forwarded() {
        let body = vec![0xCD_u8; 10_000];
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n");
        let mut data = header.as_bytes().to_vec();
        data.extend_from_slice(&body);
        let total_len = data.len();

        // 128-byte chunks
        let mut reader = make_tee_reader_chunked(data, 500, 128);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded.len(), total_len);
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
    }

    #[tokio::test]
    async fn test_incremental_scan_pos_no_rescan() {
        // Verify that scan_pos advances and we don't re-scan old bytes.
        // We feed a long header (no body) one byte at a time.
        let msg = b"GET /path HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let mut reader = make_tee_reader_chunked(msg.to_vec(), 65536, 1);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded, msg as &[u8]);
        // scan_pos should have advanced close to the end (or reset after capture).
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
    }

    // ── No Content-Length ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_no_content_length_captures_headers_only() {
        let msg = b"HTTP/1.1 204 No Content\r\nDate: Mon, 01 Jan 2024\r\n\r\n";
        let mut reader = make_tee_reader(msg.to_vec(), 65536);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded, msg as &[u8]);
        assert!(matches!(
            reader.state,
            TeeReaderState::FindingHeaders { .. }
        ));
        assert!(reader.buf.is_empty());
    }

    #[tokio::test]
    async fn test_all_http_methods() {
        let methods = ["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"];
        for method in methods {
            let msg = format!("{} / HTTP/1.1\r\nHost: example.com\r\n\r\n", method);
            let mut reader = make_tee_reader(msg.as_bytes().to_vec(), 65536);
            let forwarded = read_all(&mut reader).await;
            assert_eq!(
                forwarded,
                msg.as_bytes(),
                "All bytes should be forwarded for {}",
                method
            );
        }
    }

    // ── starts_with_http helper ───────────────────────────────────────────

    #[test]
    fn test_starts_with_http_request_methods() {
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"GET / HTTP/1.1\r\n"
        ));
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"POST /x HTTP/1.1\r\n"
        ));
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"PUT /y HTTP/1.1\r\n"
        ));
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"DELETE /z HTTP/1.1\r\n"
        ));
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"HEAD / HTTP/1.1\r\n"
        ));
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"OPTIONS * HTTP/1.1\r\n"
        ));
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"PATCH /p HTTP/1.1\r\n"
        ));
    }

    #[test]
    fn test_starts_with_http_response() {
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"HTTP/1.1 200 OK\r\n"
        ));
        assert!(TeeReader::<MockReader>::starts_with_http(
            b"HTTP/1.0 404 Not Found\r\n"
        ));
    }

    #[test]
    fn test_starts_with_http_websocket_frame() {
        // A WebSocket frame should NOT be identified as HTTP.
        let ws_frame = b"\x81\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58";
        assert!(!TeeReader::<MockReader>::starts_with_http(ws_frame));
    }

    #[test]
    fn test_starts_with_http_empty() {
        assert!(!TeeReader::<MockReader>::starts_with_http(b""));
    }

    // ── extract_content_length ───────────────────────────────────────────

    #[test]
    fn test_extract_content_length() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 42\r\n\r\n";
        assert_eq!(
            TeeReader::<MockReader>::extract_content_length(data),
            Some(42)
        );
    }

    #[test]
    fn test_extract_content_length_case_insensitive() {
        let data = b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n";
        assert_eq!(
            TeeReader::<MockReader>::extract_content_length(data),
            Some(100)
        );
    }

    #[test]
    fn test_extract_content_length_no_header() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n";
        assert_eq!(TeeReader::<MockReader>::extract_content_length(data), None);
    }

    #[test]
    fn test_extract_content_length_invalid_format() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Length: invalid\r\n\r\n";
        assert_eq!(TeeReader::<MockReader>::extract_content_length(data), None);
    }

    #[test]
    fn test_extract_content_length_with_spaces() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Length:   25   \r\n\r\n";
        assert_eq!(
            TeeReader::<MockReader>::extract_content_length(data),
            Some(25)
        );
    }

    // ── WebSocket frame extraction ────────────────────────────────────────

    #[test]
    fn test_extract_websocket_text_frame() {
        let frame = b"\x81\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_binary_frame() {
        let frame = b"\x82\x83\x12\x34\x56\x78\x9a\xbc\xde";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_close_frame() {
        let frame = b"\x88\x82\x12\x34\x56\x78\x9a\xbc";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_ping_frame() {
        let frame = b"\x89\x84\x12\x34\x56\x78\x9a\xbc\xde\xf0";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_pong_frame() {
        let frame = b"\x8a\x84\x12\x34\x56\x78\x9a\xbc\xde\xf0";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_extended_payload() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&[0x82, 0x7e]);
        frame.extend_from_slice(&[0x00, 0x7e]);
        frame.extend_from_slice(&[0u8; 126]);
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(&frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_extract_websocket_incomplete() {
        let incomplete_frame = b"\x81\x85";
        let result =
            TeeReader::<MockReader>::try_extract_websocket_frame(incomplete_frame).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_websocket_invalid_opcode() {
        let frame = b"\x83\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_websocket_too_short() {
        let too_short = b"\x81";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(too_short).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_websocket_continuation_frame() {
        let frame = b"\x80\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn test_websocket_unmasked_frame() {
        let frame = b"\x81\x05Hello";
        let result = TeeReader::<MockReader>::try_extract_websocket_frame(frame).unwrap();
        assert!(result.is_some());
        let (extracted, consumed) = result.unwrap();
        assert_eq!(extracted, frame);
        assert_eq!(consumed, frame.len());
    }

    // ── WebSocket forwarding ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_websocket_frames_forwarded() {
        // Two WebSocket frames back-to-back; all bytes must pass through.
        let frame1 = b"\x81\x05Hello";
        let frame2 = b"\x82\x03abc";
        let mut data = frame1.to_vec();
        data.extend_from_slice(frame2);

        let mut reader = make_tee_reader(data.clone(), 65536);
        let forwarded = read_all(&mut reader).await;
        assert_eq!(forwarded, data);
    }

    // ── Proxy struct ──────────────────────────────────────────────────────

    #[test]
    fn test_proxy_creation() {
        let config = TunnelConfig {
            name: "test_tunnel".to_string(),
            domain: "test.tunnel.example.com".to_string(),
            socket_path: "/tmp/test.sock".to_string(),
            target_port: 8080,
            enabled: true,
        };

        let storage = Arc::new(crate::storage::RequestStorage::new(100));
        let websocket_storage = Arc::new(crate::storage::WebSocketMessageStorage::new(1000));
        let proxy = Proxy::new(config, storage, websocket_storage, "debug", 1_048_576, 1024);

        assert_eq!(proxy.config.name, "test_tunnel");
        assert_eq!(proxy.config.target_port, 8080);
        assert_eq!(proxy.config.socket_path, "/tmp/test.sock");
        assert_eq!(proxy.max_body_size, 1_048_576);
    }
}
