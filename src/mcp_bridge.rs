//! `tunneldesk mcp`: bridges an MCP client speaking stdio to the core's
//! Streamable HTTP endpoint at `/mcp`, starting the core if necessary.
//!
//! While the bridge runs it holds a presence connection (`/api/attach`), so
//! the core does not shut down for being idle.

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use futures_util::StreamExt as _;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use crate::config::Config;
use crate::instance::{self, CoreInfo};

const SESSION_HEADER: &str = "Mcp-Session-Id";
const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";
const EVENT_STREAM: &str = "text/event-stream";
/// How long to wait for the presence connection. A core that is alive but
/// stuck still has its TCP handshakes completed by the kernel, so without a
/// limit the bridge would wait forever for the WebSocket upgrade.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs the bridge on the process's stdin/stdout until stdin closes.
pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    let info = instance::ensure_core(config_path).await?;
    let token = read_access_token(&info)?;
    let presence = connect_presence(&info, &token).await?;
    let bridge = Bridge::new(format!("{}/mcp", info.base_url()), token);
    run_bridge(tokio::io::stdin(), tokio::io::stdout(), bridge, presence).await
}

fn read_access_token(info: &CoreInfo) -> anyhow::Result<String> {
    Config::from_file(&info.config_path)
        .with_context(|| format!("failed to read {}", info.config_path.display()))?
        .gui
        .access_token
        .context("the config has no gui.access_token; start the core once to generate it")
}

/// Opens the presence connection and returns a future that resolves when it
/// closes, i.e. when the core goes away.
async fn connect_presence(
    info: &CoreInfo,
    token: &str,
) -> anyhow::Result<impl Future<Output = ()> + use<>> {
    let mut request = format!("ws://127.0.0.1:{}/api/attach", info.port).into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse()?);
    let (mut socket, _) =
        tokio::time::timeout(ATTACH_TIMEOUT, tokio_tungstenite::connect_async(request))
            .await
            .context("timed out attaching to the TunnelDesk core")?
            .context("failed to attach to the TunnelDesk core")?;
    Ok(async move {
        while let Some(msg) = socket.next().await {
            if matches!(
                msg,
                Err(_) | Ok(tokio_tungstenite::tungstenite::Message::Close(_))
            ) {
                break;
            }
        }
    })
}

/// Forwards JSON-RPC lines from `input` to the core and responses to `output`.
///
/// Returns an error if `core_gone` resolves before `input` ends.
pub async fn run_bridge<R, W>(
    input: R,
    output: W,
    bridge: Bridge,
    core_gone: impl Future<Output = ()>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let bridge = Arc::new(bridge);
    let (out_tx, out_rx) = mpsc::channel::<String>(64);
    let writer = tokio::spawn(write_lines(output, out_rx));
    let mut lines = BufReader::new(input).lines();
    let mut in_flight = tokio::task::JoinSet::new();
    // The server waits for `notifications/initialized` before any other request
    // and drops the session if something else arrives first. Later messages may
    // overlap; these must not.
    let mut handshake_done = false;
    tokio::pin!(core_gone);

    let result = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) if line.trim().is_empty() => {}
                Ok(Some(line)) => {
                    let finishes_handshake = is_initialized_notification(&line);
                    let forward = bridge.clone().forward(line, out_tx.clone());
                    if handshake_done {
                        in_flight.spawn(forward);
                    } else {
                        forward.await;
                        if finishes_handshake {
                            handshake_done = true;
                        }
                    }
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(anyhow::Error::from(e).context("failed to read stdin")),
            },
            _ = &mut core_gone => {
                break Err(anyhow::anyhow!("the TunnelDesk core shut down"));
            }
        }
    };

    if result.is_ok() {
        while in_flight.join_next().await.is_some() {}
        bridge.close_session().await;
    } else {
        in_flight.abort_all();
    }
    drop(out_tx);
    let _ = writer.await;
    result
}

async fn write_lines<W: AsyncWrite + Unpin>(mut output: W, mut rx: mpsc::Receiver<String>) {
    while let Some(line) = rx.recv().await {
        if output.write_all(line.as_bytes()).await.is_err()
            || output.write_all(b"\n").await.is_err()
            || output.flush().await.is_err()
        {
            break;
        }
    }
}

/// HTTP side of the bridge: one MCP session against the core's `/mcp`.
pub struct Bridge {
    client: reqwest::Client,
    url: String,
    token: String,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
}

impl Bridge {
    pub fn new(url: String, token: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            url,
            token,
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(None),
        }
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().unwrap().clone()
    }

    /// POSTs one JSON-RPC message and writes every resulting message to `out`.
    /// A request that fails or ends without a response gets a JSON-RPC error,
    /// so the client never waits for an answer that will not come.
    async fn forward(self: Arc<Self>, line: String, out: mpsc::Sender<String>) {
        let request_id = request_id(&line);
        let result = self.try_forward(line, request_id.as_ref(), &out).await;
        let Some(id) = request_id else {
            return;
        };
        let error = match result {
            Ok(true) => return,
            Ok(false) => anyhow::anyhow!("the TunnelDesk core sent no response"),
            Err(e) => e,
        };
        let _ = out.send(error_response(&id, &format!("{error:#}"))).await;
    }

    /// Returns whether a response to `request_id` was written to `out`.
    async fn try_forward(
        &self,
        line: String,
        request_id: Option<&Value>,
        out: &mpsc::Sender<String>,
    ) -> anyhow::Result<bool> {
        let mut request = self
            .client
            .post(&self.url)
            .bearer_auth(&self.token)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(line);
        if let Some(sid) = self.session_id() {
            request = request.header(SESSION_HEADER, sid);
        }
        if let Some(version) = self.protocol_version.lock().unwrap().clone() {
            request = request.header(PROTOCOL_VERSION_HEADER, version);
        }

        let mut response = request
            .send()
            .await
            .context("TunnelDesk core unreachable")?;
        if let Some(sid) = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }

        let status = response.status();
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok(false);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("TunnelDesk core returned {status}: {}", body.trim());
        }

        let is_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with(EVENT_STREAM));
        let mut answered = false;
        if is_stream {
            let mut parser = SseParser::default();
            while let Some(chunk) = response.chunk().await? {
                for data in parser.feed(&chunk) {
                    answered |= self.emit(data, request_id, out).await;
                }
            }
        } else {
            let body = response.text().await?;
            answered = self.emit(body.trim().to_string(), request_id, out).await;
        }
        Ok(answered)
    }

    /// Writes `message` to `out` and returns whether it answers `request_id`.
    async fn emit(
        &self,
        message: String,
        request_id: Option<&Value>,
        out: &mpsc::Sender<String>,
    ) -> bool {
        if message.trim().is_empty() {
            return false;
        }
        if let Some(version) = negotiated_protocol_version(&message) {
            self.protocol_version.lock().unwrap().get_or_insert(version);
        }
        let answers = request_id.is_some() && response_id(&message).as_ref() == request_id;
        let _ = out.send(message).await;
        answers
    }

    /// Ends the MCP session on the core (best effort).
    async fn close_session(&self) {
        if let Some(sid) = self.session_id() {
            let _ = self
                .client
                .delete(&self.url)
                .bearer_auth(&self.token)
                .header(SESSION_HEADER, sid)
                .send()
                .await;
        }
    }
}

/// `notifications/initialized`, which must reach the server before later requests.
fn is_initialized_notification(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .is_some_and(|value| {
            value.get("method").and_then(|method| method.as_str())
                == Some("notifications/initialized")
        })
}

/// The `id` of a JSON-RPC request (responses and notifications yield `None`).
fn request_id(line: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(line).ok()?;
    value.get("method")?;
    value.get("id").cloned()
}

/// The `id` of a JSON-RPC response or error (requests and notifications yield `None`).
fn response_id(line: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("method").is_some() {
        return None;
    }
    value.get("id").cloned()
}

/// Protocol version from an `initialize` result, if `message` is one.
fn negotiated_protocol_version(message: &str) -> Option<String> {
    let value: Value = serde_json::from_str(message).ok()?;
    value
        .get("result")?
        .get("protocolVersion")?
        .as_str()
        .map(str::to_string)
}

fn error_response(id: &Value, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32603, "message": message },
    })
    .to_string()
}

/// Incremental parser for `text/event-stream` bodies that yields the `data`
/// of each complete event.
#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data
                    .push(value.strip_prefix(' ').unwrap_or(value).to_string());
            }
            // Comments (":") and other fields (id, event, retry) are ignored.
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Core, CoreOptions};

    #[test]
    fn sse_parser_yields_complete_events() {
        let mut parser = SseParser::default();
        assert!(parser.feed(b"data: {\"a\"").is_empty());
        assert!(parser.feed(b":1}\n").is_empty());
        assert_eq!(parser.feed(b"\n"), vec!["{\"a\":1}"]);
    }

    #[test]
    fn sse_parser_handles_crlf_multiline_and_comments() {
        let mut parser = SseParser::default();
        let events = parser
            .feed(b": ping\r\n\r\nid: 0\r\nretry: 3000\r\ndata:\r\n\r\ndata: a\r\ndata: b\r\n\r\n");
        assert_eq!(events, vec!["", "a\nb"]);
    }

    #[test]
    fn initialized_notification_is_recognized() {
        assert!(is_initialized_notification(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        ));
        assert!(!is_initialized_notification(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#
        ));
        assert!(!is_initialized_notification("not json"));
    }

    #[test]
    fn request_id_only_for_requests() {
        assert_eq!(
            request_id(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#),
            Some(Value::from(7))
        );
        assert_eq!(
            request_id(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        assert_eq!(request_id(r#"{"jsonrpc":"2.0","id":7,"result":{}}"#), None);
        assert_eq!(request_id("not json"), None);
    }

    #[test]
    fn response_id_only_for_responses() {
        assert_eq!(
            response_id(r#"{"jsonrpc":"2.0","id":7,"result":{}}"#),
            Some(Value::from(7))
        );
        assert_eq!(
            response_id(r#"{"jsonrpc":"2.0","id":"x","error":{"code":1,"message":"m"}}"#),
            Some(Value::from("x"))
        );
        assert_eq!(
            response_id(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#),
            None
        );
        assert_eq!(
            response_id(r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#),
            None
        );
        assert_eq!(response_id("not json"), None);
    }

    #[test]
    fn error_response_is_jsonrpc_error() {
        let v: Value = serde_json::from_str(&error_response(&Value::from("x"), "boom")).unwrap();
        assert_eq!(v["id"], "x");
        assert_eq!(v["error"]["message"], "boom");
        assert_eq!(v["jsonrpc"], "2.0");
    }

    #[test]
    fn protocol_version_is_read_from_initialize_result() {
        assert_eq!(
            negotiated_protocol_version(
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#
            ),
            Some("2025-06-18".to_string())
        );
        assert_eq!(
            negotiated_protocol_version(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#),
            None
        );
    }

    async fn start_core(dir: &tempfile::TempDir) -> (Core, String) {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gui]\nport = 0\n").unwrap();
        let core = Core::start(
            &path,
            CoreOptions {
                idle_shutdown: false,
            },
        )
        .await
        .unwrap();
        let token = Config::from_file(&path).unwrap().gui.access_token.unwrap();
        (core, token)
    }

    async fn run_session(url: String, token: String, input: &str) -> (anyhow::Result<()>, String) {
        let (mut client_in, bridge_in) = tokio::io::duplex(64 * 1024);
        let (bridge_out, mut client_out) = tokio::io::duplex(64 * 1024);
        client_in.write_all(input.as_bytes()).await.unwrap();
        drop(client_in);
        let result = run_bridge(
            bridge_in,
            bridge_out,
            Bridge::new(url, token),
            std::future::pending(),
        )
        .await;
        let mut output = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut client_out, &mut output)
            .await
            .unwrap();
        (result, output)
    }

    const SESSION: &str = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
    );

    #[tokio::test]
    async fn bridge_delivers_initialized_before_the_next_request() {
        let dir = tempfile::tempdir().unwrap();
        let (core, token) = start_core(&dir).await;
        let url = format!("http://127.0.0.1:{}/mcp", core.port());

        // The server drops the session if a request overtakes the initialized
        // notification. Repeat so a reordering race cannot pass unnoticed.
        for _ in 0..15 {
            let (result, output) = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                run_session(url.clone(), token.clone(), SESSION),
            )
            .await
            .expect("bridge hung; initialized notification was overtaken");
            result.unwrap();
            let responses: Vec<Value> = output
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            assert_eq!(responses.len(), 2, "output: {output}");
            assert!(responses[1]["result"]["tools"].is_array(), "{output}");
        }
        core.shutdown().await;
    }

    #[tokio::test]
    async fn bridge_relays_a_full_session() {
        let dir = tempfile::tempdir().unwrap();
        let (core, token) = start_core(&dir).await;
        let url = format!("http://127.0.0.1:{}/mcp", core.port());

        let (result, output) = run_session(url, token, SESSION).await;
        result.unwrap();
        let responses: Vec<Value> = output
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(responses.len(), 2, "output: {output}");
        assert_eq!(responses[0]["id"], 1);
        assert!(responses[0]["result"]["serverInfo"].is_object());
        assert_eq!(responses[1]["id"], 2);
        let tools = responses[1]["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "query_requests"));
        core.shutdown().await;
    }

    #[tokio::test]
    async fn bridge_reports_rejected_token_as_jsonrpc_error() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _token) = start_core(&dir).await;
        let url = format!("http://127.0.0.1:{}/mcp", core.port());

        let (result, output) = run_session(url, "wrong".into(), SESSION).await;
        result.unwrap();
        let first: Value = serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(first["id"], 1);
        assert!(first["error"]["message"].as_str().unwrap().contains("401"));
        core.shutdown().await;
    }

    #[tokio::test]
    async fn bridge_reports_requests_left_without_a_response() {
        // A server whose event streams end without the response, as happens
        // when the core drops the MCP session mid-request.
        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post(|| async {
                (
                    [(CONTENT_TYPE, EVENT_STREAM)],
                    ": keep-alive\n\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\n",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let (result, output) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_session(format!("http://127.0.0.1:{port}/mcp"), "t".into(), SESSION),
        )
        .await
        .expect("bridge hung");
        result.unwrap();
        let errors: Vec<Value> = output
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|m| m.get("error").is_some())
            .collect();
        assert_eq!(errors.len(), 2, "output: {output}");
        assert_eq!(errors[0]["id"], 1);
        assert_eq!(errors[1]["id"], 2);
        assert!(
            errors[1]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no response"),
            "{output}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn bridge_fails_when_core_goes_away() {
        let (_client_in, bridge_in) = tokio::io::duplex(1024);
        let (bridge_out, _client_out) = tokio::io::duplex(1024);
        let result = run_bridge(
            bridge_in,
            bridge_out,
            Bridge::new("http://127.0.0.1:1/mcp".into(), "t".into()),
            std::future::ready(()),
        )
        .await;
        assert!(result.is_err());
    }
}
