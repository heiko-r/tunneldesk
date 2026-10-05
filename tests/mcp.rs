//! MCP over the core's Streamable HTTP endpoint and through the stdio bridge,
//! including real tunnel traffic captured by the core.

#![cfg(feature = "mcp")]

mod common;

use std::process::Stdio;
use std::time::Duration;

use common::*;
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"integration-test","version":"0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// A minimal MCP client for the core's `/mcp` endpoint.
struct McpHttp {
    client: reqwest::Client,
    url: String,
    token: String,
    session: Option<String>,
}

impl McpHttp {
    fn new(info: &CoreInfo, token: String) -> Self {
        Self {
            client: http_client(),
            url: format!("{}/mcp", info.base_url()),
            token,
            session: None,
        }
    }

    fn post(&self, body: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(&self.url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(body.to_string());
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session);
        }
        request
    }

    /// Sends a request and returns the JSON-RPC response from the SSE stream.
    async fn call(&mut self, body: &str) -> Value {
        let response = self.post(body).send().await.unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        if let Some(session) = response.headers().get("mcp-session-id") {
            self.session = Some(session.to_str().unwrap().to_string());
        }
        let text = response.text().await.unwrap();
        text.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
            .find(|message| message.get("id").is_some())
            .unwrap_or_else(|| panic!("no JSON-RPC response in {text:?}"))
    }

    async fn notify(&self, body: &str) {
        let response = self.post(body).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    }

    async fn initialize(&mut self) -> Value {
        let result = self.call(INITIALIZE).await;
        assert!(self.session.is_some(), "the server must assign a session");
        self.notify(INITIALIZED).await;
        result
    }

    /// Ends the MCP session. This must not shut the core down.
    async fn close_session(&self) {
        let session = self.session.as_deref().expect("session");
        let response = self
            .client
            .delete(&self.url)
            .bearer_auth(&self.token)
            .header("Mcp-Session-Id", session)
            .header("MCP-Protocol-Version", "2025-06-18")
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "closing the MCP session failed: {}",
            response.status()
        );
    }

    async fn call_tool(&mut self, id: u32, name: &str, arguments: Value) -> Value {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let response = self.call(&body.to_string()).await;
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("unexpected tool result: {response}"));
        serde_json::from_str(text).unwrap()
    }
}

// ── Streamable HTTP ───────────────────────────────────────────────────────────

#[tokio::test]
async fn http_endpoint_requires_the_access_token() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let _core = env.spawn_serve(&config);
    let info = env.wait_for_info(&config).await;

    let mut mcp = McpHttp::new(&info, "wrong-token".into());
    let denied = mcp.post(INITIALIZE).send().await.unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);

    mcp.token = read_token(&config);
    let result = mcp.initialize().await;
    assert!(result["result"]["protocolVersion"].is_string(), "{result}");

    env.run(&config, &["stop"]).await;
}

#[tokio::test]
async fn http_endpoint_rejects_foreign_origins() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let _core = env.spawn_serve(&config);
    let info = env.wait_for_info(&config).await;

    let mcp = McpHttp::new(&info, read_token(&config));
    let response = mcp
        .post(INITIALIZE)
        .header("Origin", "https://evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    env.run(&config, &["stop"]).await;
}

#[tokio::test]
async fn http_endpoint_lists_tools() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let _core = env.spawn_serve(&config);
    let info = env.wait_for_info(&config).await;

    let mut mcp = McpHttp::new(&info, read_token(&config));
    mcp.initialize().await;
    let tools = mcp
        .call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
        .await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for expected in [
        "list_tunnels",
        "query_requests",
        "get_request",
        "send_request",
    ] {
        assert!(names.contains(&expected), "{names:?}");
    }

    env.run(&config, &["stop"]).await;
}

// ── mcp-config ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mcp_config_prints_stdio_and_http_snippets_for_a_fixed_port() {
    let env = TestEnv::new();
    let config = env.dir.path().join("fixed.toml");
    std::fs::write(
        &config,
        "[gui]\nport = 45813\naccess_token = \"secret-token\"\n",
    )
    .unwrap();
    let config = std::fs::canonicalize(config).unwrap();

    let (status, stdout, _) = env.run(&config, &["mcp-config"]).await;
    assert!(status.success());
    assert!(stdout.contains("\"mcp\""), "{stdout}");
    assert!(stdout.contains(&config.display().to_string()), "{stdout}");
    assert!(stdout.contains("http://127.0.0.1:45813/mcp"), "{stdout}");
    assert!(stdout.contains("Bearer secret-token"), "{stdout}");
}

#[tokio::test]
async fn mcp_config_recommends_stdio_for_an_ephemeral_port() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let (status, stdout, _) = env.run(&config, &["mcp-config"]).await;
    assert!(status.success());
    assert!(stdout.contains("\"mcp\""), "{stdout}");
    assert!(stdout.contains("port = 0"), "{stdout}");
    assert!(!stdout.contains("/mcp\""), "{stdout}");
}

// ── stdio bridge ──────────────────────────────────────────────────────────────

struct Bridge {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl Bridge {
    fn spawn(env: &TestEnv, config: &std::path::Path) -> Self {
        let mut child = env
            .command(config, &["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            stdin,
            stdout,
        }
    }

    async fn send(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.write_all(b"\n").await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(20), self.stdout.next_line())
            .await
            .expect("bridge response timed out")
            .unwrap()
            .expect("bridge closed stdout");
        serde_json::from_str(&line).unwrap()
    }
}

#[tokio::test]
async fn stdio_bridge_starts_a_core_on_demand() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let mut bridge = Bridge::spawn(&env, &config);

    bridge.send(INITIALIZE).await;
    let init = bridge.recv().await;
    assert_eq!(init["id"], 1);
    assert!(init["result"]["protocolVersion"].is_string(), "{init}");
    bridge.send(INITIALIZED).await;
    bridge
        .send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
        .await;
    let tools = bridge.recv().await;
    assert_eq!(tools["id"], 2);
    assert!(!tools["result"]["tools"].as_array().unwrap().is_empty());

    let info = env
        .find_info(&config)
        .expect("the bridge must have started a core");
    assert_ne!(
        info.pid,
        bridge.child.id().unwrap(),
        "the core is a separate process"
    );

    // Closing stdin ends the bridge; the unused core then exits on its own.
    drop(bridge.stdin);
    let status = wait_for_exit(&mut bridge.child, Duration::from_secs(10))
        .await
        .expect("bridge must exit on EOF");
    assert!(status.success());
    assert!(
        env.wait_for_info_removed(&config).await,
        "the detached core must exit once idle"
    );
}

#[tokio::test]
async fn stdio_bridges_share_a_running_core() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let _core = env.spawn_serve(&config);
    let info = env.wait_for_info(&config).await;

    let mut bridges = [Bridge::spawn(&env, &config), Bridge::spawn(&env, &config)];
    for bridge in &mut bridges {
        bridge.send(INITIALIZE).await;
        assert_eq!(bridge.recv().await["id"], 1);
        bridge.send(INITIALIZED).await;
    }
    assert_eq!(
        env.find_info(&config).unwrap().pid,
        info.pid,
        "bridges must attach to the existing core"
    );

    // A change through one bridge is visible through the other.
    let socket = env.dir.path().join("shared.sock");
    let create = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "create_tunnel", "arguments": {
            "name": "shared", "domain": "shared.example.com",
            "target_port": 9, "socket_path": socket,
        }},
    });
    bridges[0].send(&create.to_string()).await;
    let created = bridges[0].recv().await;
    assert!(created["result"]["isError"] != true, "{created}");

    bridges[1]
        .send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_tunnels","arguments":{}}}"#)
        .await;
    let listed = bridges[1].recv().await;
    let text = listed["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("shared.example.com"), "{text}");

    drop(bridges);
    env.run(&config, &["stop"]).await;
}

#[tokio::test]
async fn stdio_bridge_reports_http_errors_as_json_rpc_errors() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let _core = env.spawn_serve(&config);
    env.wait_for_info(&config).await;

    let mut bridge = Bridge::spawn(&env, &config);
    // tools/list before initialize has no session; the server rejects it.
    bridge
        .send(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#)
        .await;
    let response = bridge.recv().await;
    assert_eq!(response["id"], 7);
    assert!(response["error"]["message"].is_string(), "{response}");

    drop(bridge);
    env.run(&config, &["stop"]).await;
}

/// A GUI WebSocket client: the "other client" an MCP session must not evict.
struct UiClient {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl UiClient {
    async fn connect(info: &CoreInfo, token: &str) -> Self {
        let mut request = format!("ws://127.0.0.1:{}/ws", info.port)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        Self { socket }
    }

    /// Current core status. Pushes already buffered on the socket are discarded
    /// first, so the result is not a client-count update from earlier.
    async fn core_status(&mut self) -> Value {
        let drained = tokio::time::Instant::now() + Duration::from_millis(50);
        while tokio::time::Instant::now() < drained {
            match tokio::time::timeout(Duration::from_millis(15), self.socket.next()).await {
                Ok(Some(Ok(msg))) => reject_shutdown(&msg),
                Ok(Some(Err(e))) => panic!("UI connection failed: {e}"),
                Ok(None) => panic!("UI connection closed"),
                Err(_) => break,
            }
        }
        self.socket
            .send(Message::Text(r#"{"type":"GetCoreStatus"}"#.into()))
            .await
            .expect("the UI connection must stay open");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let msg = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .expect("UI response timed out")
                .expect("UI connection closed")
                .unwrap();
            reject_shutdown(&msg);
            let Message::Text(text) = msg else { continue };
            let value: Value = serde_json::from_str(&text).unwrap();
            if value["type"] == "CoreStatus" {
                return value["data"].clone();
            }
        }
    }
}

fn reject_shutdown(msg: &Message) {
    if let Message::Text(text) = msg
        && serde_json::from_str::<Value>(text).is_ok_and(|v| v["type"] == "ShuttingDown")
    {
        panic!("the core started shutting down while a UI client is attached");
    }
}

#[tokio::test]
async fn mcp_client_cannot_stop_the_core_while_another_client_is_attached() {
    let env = TestEnv::new();
    let config = env.write_config("config.toml", "");
    let mut core = env
        .command(&config, &["core", "--detached"])
        .spawn()
        .unwrap();
    let info = env.wait_for_info(&config).await;
    let token = read_token(&config);

    // Idle timeout is 1s. This client is what must keep the core alive.
    let mut ui = UiClient::connect(&info, &token).await;

    let mut mcp = McpHttp::new(&info, token.clone());
    mcp.initialize().await;
    let tools = mcp
        .call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
        .await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for forbidden in ["shutdown", "shutdown_core", "stop_core", "quit"] {
        assert!(
            !names.contains(&forbidden),
            "MCP must not expose a core shutdown tool, got {names:?}"
        );
    }
    // Ending the HTTP session must not cancel the core's shutdown token.
    mcp.close_session().await;

    let mut bridge = Bridge::spawn(&env, &config);
    bridge.send(INITIALIZE).await;
    assert_eq!(bridge.recv().await["id"], 1);
    bridge.send(INITIALIZED).await;
    drop(bridge.stdin);
    wait_for_exit(&mut bridge.child, Duration::from_secs(10))
        .await
        .expect("the MCP bridge must exit when its client disconnects");

    // Longer than idle_timeout_secs. If disconnecting MCP had dropped the UI's
    // slot or cancelled the core, the process would be gone by now.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let health = http_client()
        .get(format!("{}/api/health", info.base_url()))
        .send()
        .await
        .expect("the core must still answer while the UI client is attached");
    assert!(health.status().is_success(), "{}", health.status());
    assert_eq!(env.find_info(&config).map(|i| i.pid), Some(info.pid));

    let status = ui.core_status().await;
    assert_eq!(status["pid"].as_u64(), Some(u64::from(info.pid)));
    assert_eq!(
        status["attached_clients"].as_u64(),
        Some(1),
        "only the UI client should remain attached: {status}"
    );

    env.run(&config, &["stop"]).await;
    wait_for_exit(&mut core, Duration::from_secs(10)).await;
}

// ── Tunnel traffic end to end ─────────────────────────────────────────────────

/// Serves `hello` to every HTTP request on an ephemeral port.
async fn spawn_http_target() -> u16 {
    spawn_http_target_with(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_vec(),
        false,
    )
    .await
}

/// Serves the raw `response` to every HTTP request on an ephemeral port,
/// closing the connection afterwards unless `keep_open`.
async fn spawn_http_target_with(response: Vec<u8>, keep_open: bool) -> u16 {
    use tokio::io::AsyncReadExt as _;
    let response = std::sync::Arc::new(response);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let response = response.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let mut request = Vec::new();
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = stream.write_all(&response).await;
                while keep_open && matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
                let _ = stream.shutdown().await;
            });
        }
    });
    port
}

#[cfg(unix)]
#[tokio::test]
async fn tunnel_traffic_is_captured_and_queryable_over_mcp() {
    use tokio::io::AsyncReadExt as _;

    let env = TestEnv::new();
    let target_port = spawn_http_target().await;
    let socket = env.dir.path().join("e2e.sock");
    let config = env.write_config(
        "config.toml",
        &format!(
            "[[tunnels]]\nname = \"e2e\"\ndomain = \"e2e.example.com\"\nsocket_path = \"{}\"\ntarget_port = {target_port}\n",
            socket.display()
        ),
    );
    let _core = env.spawn_serve(&config);
    let info = env.wait_for_info(&config).await;
    assert!(
        wait_until(Duration::from_secs(10), || socket.exists().then_some(()))
            .await
            .is_some(),
        "the tunnel socket must be created"
    );

    // What cloudflared would do: send the request into the tunnel socket.
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(
            b"GET /hello?from=test HTTP/1.1\r\nHost: e2e.example.com\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.ends_with("hello"), "{reply}");

    let mut mcp = McpHttp::new(&info, read_token(&config));
    mcp.initialize().await;

    let mut id = 1;
    let summary = loop {
        id += 1;
        let requests = mcp
            .call_tool(id, "query_requests", json!({ "tunnel_name": "e2e" }))
            .await;
        if let Some(first) = requests.as_array().and_then(|r| r.first())
            && first["status"] == 200
        {
            break first.clone();
        }
        assert!(id < 100, "the exchange was never stored: {requests}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(summary["method"], "GET");
    assert_eq!(summary["url"], "/hello?from=test");

    let details = mcp
        .call_tool(id + 1, "get_request", json!({ "id": summary["id"] }))
        .await;
    assert!(details.to_string().contains("hello"), "{details}");

    env.run(&config, &["stop"]).await;
    assert!(env.wait_for_info_removed(&config).await);
    assert!(
        !socket.exists(),
        "stopping the core removes the tunnel socket"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn chunked_compressed_response_is_stored_decoded() {
    use std::io::Write as _;
    use tokio::io::AsyncReadExt as _;

    let json = br#"{"access_token":"abc","token_type":"bearer","expires_in":3600}"#;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(json).unwrap();
    let compressed = gzip.finish().unwrap();
    let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for chunk in compressed.chunks(16) {
        response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        response.extend_from_slice(chunk);
        response.extend_from_slice(b"\r\n");
    }
    response.extend_from_slice(b"0\r\n\r\n");

    let env = TestEnv::new();
    // The connection stays open, so the capture must find the end of the
    // message from its chunked framing.
    let target_port = spawn_http_target_with(response.clone(), true).await;
    let socket = env.dir.path().join("chunked.sock");
    let config = env.write_config(
        "config.toml",
        &format!(
            "[[tunnels]]\nname = \"chunked\"\ndomain = \"chunked.example.com\"\nsocket_path = \"{}\"\ntarget_port = {target_port}\n",
            socket.display()
        ),
    );
    let _core = env.spawn_serve(&config);
    let info = env.wait_for_info(&config).await;
    assert!(
        wait_until(Duration::from_secs(10), || socket.exists().then_some(()))
            .await
            .is_some(),
        "the tunnel socket must be created"
    );

    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(b"POST /auth/v1/token?grant_type=password HTTP/1.1\r\nHost: chunked.example.com\r\nAccept-Encoding: gzip, br\r\nContent-Length: 2\r\n\r\n{}")
        .await
        .unwrap();
    let mut reply = vec![0u8; response.len()];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply, response, "the response is proxied unchanged");

    let mut mcp = McpHttp::new(&info, read_token(&config));
    mcp.initialize().await;

    let mut id = 1;
    let summary = loop {
        id += 1;
        let requests = mcp
            .call_tool(id, "query_requests", json!({ "tunnel_name": "chunked" }))
            .await;
        if let Some(first) = requests.as_array().and_then(|r| r.first())
            && first["status"] == 200
        {
            break first.clone();
        }
        assert!(id < 100, "the response was never stored: {requests}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let details = mcp
        .call_tool(id + 1, "get_request", json!({ "id": summary["id"] }))
        .await;
    assert_eq!(details["body"], "{}");
    assert_eq!(
        details["response"]["body"],
        std::str::from_utf8(json).unwrap(),
        "{details}"
    );

    drop(stream);
    env.run(&config, &["stop"]).await;
}
