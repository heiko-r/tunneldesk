# TunnelDesk

Local proxy for Cloudflare Tunnels, with request inspection.

## Context

- Tunnels and all settings are configured in the `config.toml` file
- There is one background **core** process per (canonical) config file. It owns the `cloudflared` child process, the proxies, the storage, and one authenticated HTTP endpoint on `127.0.0.1` (web UI, `/ws`, `/api/*`, `/mcp`)
- The native window, browser tabs and MCP clients are thin frontends: they find the core via the info file in the runtime dir, or spawn it detached (`tunneldesk core --detached`)
- The core runs `cloudflared tunnel run` as a supervised child process (unless `manage_cloudflared = false`), which forwards traffic to local Unix Domain Sockets
- There can be multiple tunnels configured, and multiple connections in parallel
- This proxy application forwards traffic from the Unix Domain Sockets to the configured local ports
- A detached core exits after `[core] idle_timeout_secs` without attached clients; `tunneldesk stop` and the UI's quit action stop it explicitly
- Every endpoint except `/api/health` requires `[gui] access_token` (bearer, cookie, or `?token=` on `/` and `/ws`); Host and Origin headers are checked
- This proxy application stores all types of HTTP requests
- Websocket messages are stored linked to the upgraded HTTP request
- This proxy application stores the full requests and responses (including headers and bodies) in memory
- Bodies larger than the configured limit are truncated in storage, but proxying continues normally
- The local web UI is served by this proxy application
- The local web UI communicates wth the proxy application via websocket, querying requests for a specific tunnels and receiving new requests as they arrive

## Tech Stack

- Request inspection proxy: Rust, Tokio, Axum
- Local web UI: SvelteKit as static single page application
- Native GUI window: `wry` (system webview) + `tao` (windowing), enabled by the `gui` Cargo feature (on by default)
- Platform: Linux, Windows, MacOS

## Architecture

- `/src` contains the local proxy application in Rust
  - `main.rs`: CLI (`serve`, `open`, `stop`, `status`, `mcp`, `mcp-config`, hidden `core`) and mode dispatch
  - `core.rs`: `Core::start`/`shutdown`, Cloudflare setup, `ClientRegistry` and the idle watcher
  - `instance.rs`: runtime dir, config hash, lock/info files, `ensure_core` (find or spawn a core)
  - `security.rs`: Host/Origin/token policy used by the web server middleware
  - `web_server.rs`: axum router, `/ws` protocol, `/api/health`, `/api/attach`, `/api/shutdown`, `/mcp`
  - `cloudflared.rs`: supervised `cloudflared` child process and connector state
  - `mcp.rs`: MCP tools, served over Streamable HTTP; `mcp_bridge.rs`: stdio-to-HTTP bridge for `mcp`
- `/src/gui.rs` contains the native GUI window launcher (tao event loop + wry webview), a thin view onto the core
- `/tests` contains integration tests that drive the real binary (`tests/common` has the helpers)
- `/frontend` contains the local web UI in SvelteKit

## GUI Feature

- Default build (`cargo build`) includes a native webview window that opens automatically
- The window attaches to (or starts) the core for its config file; closing it leaves the core running until it is idle
- `cargo build --no-default-features` builds a headless server only (no wry/tao dependency); without a subcommand it runs `serve`
- On Linux, requires system packages: `libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev`, `libjavascriptcoregtk-4.1-dev`, `libgtk-3-dev`

## Conventions

- Use Rust and Svelte 5 idioms where appropriate
- Prefer smaller, reusable Svelte components
- Prefer clarity and correctness over micro-optimization unless in hot paths
- In Svelte 5 `$effect`, only values read **synchronously** in the effect body are tracked as dependencies. Values read inside `setTimeout`/`Promise` callbacks are invisible to the tracker — capture them in a `const` before the callback if the effect must re-run when they change.

## Performance

- Minimise the impact of traffic inspection on the tunneled connection performance

## Testing

- Aim for high test coverage: 90% across the codebase
- Every component must have thorough unit tests
- Write integration tests for the proxy application that simulate real tunnel traffic and verify correct storage and websocket API responses
- Tests must be written alongside new code — never defer testing to later

## Development Validation

- Run `prek run --all-files` to verify any changes

## Environment

- Node.js is managed via nvm; activate with `source ~/.nvm/nvm.sh && nvm use 24` before any npm command
- Run `npx playwright install chromium` once before running browser tests for the first time

## Frontend Structure

- Components live under `frontend/src/lib/components/{modal,sidebar,details,body}/`
- API/WebSocket client lives under `frontend/src/lib/api/` (`websocket.svelte.ts`, `mappers.ts`)
- Global styles are in `frontend/src/app.css` (not embedded in components)
- In production (built SPA), WebSocket URL uses `window.location.host` — backend serves UI and WS on the same port
- In dev (`npm run dev`), WebSocket URL uses `window.location.hostname` + `VITE_BACKEND_PORT` from `.env.development` (default 3013, matching `config.toml [gui] port`) and appends `?token=VITE_BACKEND_TOKEN` (set in the gitignored `.env.development.local`); the core must list `http://localhost:5173` in `[gui] allowed_origins`
- Tunnel changes are broadcast to every client, including the one that made them, so store updates must be idempotent (e.g. `addTunnel` upserts by name)

## Frontend Testing

- `*.svelte.spec.ts` files run in Chromium via Playwright (browser project)
- Plain `*.spec.ts` files run in Node (server project)
- Import `page` from `vitest/browser` (not the deprecated `@vitest/browser/context`)
- `page.locator()` does not exist on `BrowserPage` (vitest-browser-svelte). Use `page.getByRole()`, `page.getByText()`, or `page.getByTestId()` only.                                                                                                                                       - Run `npx playwright install chromium` before the first browser test run on a new machine.