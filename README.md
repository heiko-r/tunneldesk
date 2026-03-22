# TunnelDesk

A local HTTP proxy that forwards requests from Unix domain sockets to TCP ports with comprehensive request inspection and WebSocket support.

## Features

- **Multiple Tunnels**: Configure multiple tunnels with separate Unix domain sockets
- **HTTP & WebSocket Support**: Forward both HTTP requests and WebSocket connections
- **Comprehensive Request Inspection**: Captures all request/response metadata and bodies and logs them in human-readable format
- **Configuration File**: Easy TOML-based configuration
- **Graceful Shutdown**: Clean shutdown with Ctrl+C

## Installation

```bash
cargo build --release
```

## Usage

### Basic Usage

```bash
# Start with default config.toml
cargo run

# Start with custom config file
cargo run -- --config /path/to/config.toml
```

### Configuration

Create a `config.toml` file:

```toml
[logging]
stdout_level = "basic"

[capture]
max_stored_requests = 1000

[[tunnels]]
name = "webapp"
socket_path = "/tmp/webapp.sock"
target_port = 8080

[[tunnels]]
name = "api"
socket_path = "/tmp/api.sock"
target_port = 3000
```

### Making Requests

Use tools like `curl` or HTTP clients that support Unix domain sockets:

```bash
# HTTP request
curl --unix-socket /tmp/webapp.sock http://localhost/api/users

# WebSocket request
wscat -c ws://localhost/ --socket /tmp/webapp.sock
```

## Request inspection

The proxy captures all traffix and logs it in human-readable format:

```
[webapp] → GET /api/users - 573 bytes
[webapp] Headers: {user-agent: curl/7.68.0, accept: */*}
[webapp] Body: (empty)

[webapp] ← 200 - 129 bytes
[webapp] Headers: {content-type: application/json, content-length: 42}
[webapp] Body: {"users": [{"id": 1, "name": "Alice"}]}

[webapp] WS → - opcode: 1, payload: {"type": "message", "data": "hello"}
[webapp] WS ← - opcode: 1, payload: {"type": "response", "data": "world"}
```

## Configuration Options

Each tunnel supports:

- `name`: Identifier for the tunnel (used in logs)
- `socket_path`: Path to the Unix domain socket (will be created if it doesn't exist)
- `target_port`: TCP port to forward requests to

## Development

```bash
# Run in development mode
cargo run

# Run tests
cargo test

# Build for release
cargo build --release
```

## License

MIT
