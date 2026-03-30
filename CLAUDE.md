# TunnelDesk

Local proxy for Cloudflare Tunnels, with request inspection.

## Context

- The `cloudflared` tunnel client is already running separately and forwarding traffic to a local Unix Domain Socket
- Tunnels and all settings are configured in the `config.toml` file
- There can be multiple tunnels configures, and multiple connections in parallel
- This proxy application forwards traffic from the Unix Domain Sockets to the configured local ports
- This proxy application stores all types of HTTP requests
- Websocket messages are stored linked to the upgraded HTTP request
- This proxy application stores the full requests and responses (including headers and bodies) in memory
- Bodies larger than the configured limit are truncated in storage, but proxying continues normally
- The local web UI is served by this proxy application
- The local web UI communicates wth the proxy application via websocket, querying requests for a specific tunnels and receiving new requests as they arrive

## Tech Stack

- Request inspection proxy: Rust, Tokio, Axum
- Local web UI: SvelteKit as static single page application
- Platform: Linux, Windows, MacOS

## Architecture

- `/src` contains the local proxy application in Rust
- `/frontend` contains the local web UI in SvelteKit

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

- Run `prek run` to verify any changes

## Environment

- Node.js is managed via nvm; activate with `source ~/.nvm/nvm.sh && nvm use 24` before any npm command
- Run `npx playwright install chromium` once before running browser tests for the first time

## Frontend Structure

- Components live under `frontend/src/lib/components/{modal,sidebar,details,body}/`
- API/WebSocket client lives under `frontend/src/lib/api/` (`websocket.svelte.ts`, `mappers.ts`)
- Global styles are in `frontend/src/app.css` (not embedded in components)
- In production (built SPA), WebSocket URL uses `window.location.host` — backend serves UI and WS on the same port
- In dev (`npm run dev`), WebSocket URL uses `window.location.hostname` + `VITE_BACKEND_PORT` from `.env.development` (default 8081, matching `config.toml [gui] port`)

## Frontend Testing

- `*.svelte.spec.ts` files run in Chromium via Playwright (browser project)
- Plain `*.spec.ts` files run in Node (server project)
- Import `page` from `vitest/browser` (not the deprecated `@vitest/browser/context`)
