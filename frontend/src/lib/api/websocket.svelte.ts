import {
  getActiveQueryFilter,
  addRequests,
  addTunnel,
  addWsMessage,
  cloudflareStatus,
  lastSyncReport,
  removeTunnel,
  setWsMessages,
  updateRequests,
  updateTunnel,
  updateTunnels,
} from "$lib/stores.svelte";
import { mapToTunnel, mapToTunneledRequest, decodeWsPayload, parseWsDirection } from "./mappers";
import type { RawTunnel } from "./mappers";

// ── Protocol types ────────────────────────────────────────────────────────────

type StatusFilter = { Exact: number } | { Class: number };

type QueryFilter = {
  tunnel_name?: string;
  method?: string;
  status?: StatusFilter;
  url_contains?: string;
  since?: string;
  until?: string;
  sort_field?: "Timestamp" | "ResponseTime";
  sort_direction?: "Asc" | "Desc";
};

type RawRequestData = {
  id: string;
  timestamp: string;
  tunnel_name: string;
  method: string;
  url: string;
  headers: { [key: string]: string };
  body: string;
  raw_request: string;
};

type RawResponseData = {
  request_id: string;
  timestamp: string;
  status: number;
  headers: { [key: string]: string };
  response_time_ms?: number;
  body: string;
  raw_response: string;
};

type WsMessageData = {
  id: string;
  timestamp: string;
  tunnel_name: string;
  upgrade_request_id: string;
  direction: string;
  message_type: "Text" | "Binary";
  payload: string;
};

// ── Response shapes ───────────────────────────────────────────────────────────

type TunnelsResponse = { type: "Tunnels"; data: RawTunnel[] };
type RequestsResponse = {
  type: "Requests";
  data: { request: RawRequestData; response?: RawResponseData }[];
};
type NewRequestResponse = {
  type: "NewRequest";
  data: { request: RawRequestData; response?: RawResponseData };
};
type WebSocketMessagesResponse = { type: "WebSocketMessages"; data: WsMessageData[] };
type NewWebSocketMessageResponse = { type: "NewWebSocketMessage"; data: WsMessageData };

type TunnelCreatedResponse = { type: "TunnelCreated"; data: RawTunnel };
type TunnelUpdatedResponse = { type: "TunnelUpdated"; data: RawTunnel };
type TunnelDeletedResponse = { type: "TunnelDeleted"; data: { name: string } };

type SyncReportResponse = {
  type: "SyncReport";
  data: {
    added: string[];
    removed: string[];
    unknown_hosts: string[];
    errors: string[];
  };
};

type CloudflareStatusResponse = {
  type: "CloudflareStatus";
  data: {
    configured: boolean;
    tunnel_id?: string;
    tunnel_name?: string;
    service_running: boolean;
  };
};

type ErrorResponse = { type: "Error"; data: string };

// ── State ─────────────────────────────────────────────────────────────────────

let ws: WebSocket | null = null;

/** Reactive connection state. Mutate `.connected` rather than reassigning. */
export const connectionState = $state({ connected: false });

// ── Connection ────────────────────────────────────────────────────────────────

function connect() {
  const host = import.meta.env.DEV
    ? `${window.location.hostname}:${import.meta.env.VITE_BACKEND_PORT}`
    : window.location.host;
  const wsUrl = `ws://${host}/ws`;
  ws = new WebSocket(wsUrl);

  ws.onopen = () => {
    connectionState.connected = true;
    console.log("WebSocket connected");
    queryTunnels();
    getCloudflareStatus();
  };

  ws.onmessage = (event) => {
    try {
      const message = JSON.parse(event.data);
      switch (message.type) {
        case "Tunnels":
          handleTunnelsMessage(message as TunnelsResponse);
          break;
        case "Requests":
          handleRequestsMessage(message as RequestsResponse);
          break;
        case "NewRequest":
          handleNewRequestMessage(message as NewRequestResponse);
          break;
        case "WebSocketMessages":
          handleWebSocketMessagesMessage(message as WebSocketMessagesResponse);
          break;
        case "NewWebSocketMessage":
          handleNewWebSocketMessageMessage(message as NewWebSocketMessageResponse);
          break;
        case "TunnelCreated":
          handleTunnelCreatedMessage(message as TunnelCreatedResponse);
          break;
        case "TunnelUpdated":
          handleTunnelUpdatedMessage(message as TunnelUpdatedResponse);
          break;
        case "TunnelDeleted":
          handleTunnelDeletedMessage(message as TunnelDeletedResponse);
          break;
        case "SyncReport":
          handleSyncReportMessage(message as SyncReportResponse);
          break;
        case "CloudflareStatus":
          handleCloudflareStatusMessage(message as CloudflareStatusResponse);
          break;
        case "Error":
          handleErrorMessage(message as ErrorResponse);
          break;
        default:
          console.warn("Unknown message type:", message.type);
      }
    } catch (error) {
      console.error("Error parsing message:", error);
    }
  };

  ws.onclose = () => {
    connectionState.connected = false;
    console.log("WebSocket disconnected");
    setTimeout(() => connect(), 3000);
  };

  ws.onerror = (error) => {
    connectionState.connected = false;
    console.error("WebSocket error:", error);
  };
}

// ── Response handlers ─────────────────────────────────────────────────────────

function handleTunnelsMessage(message: TunnelsResponse) {
  updateTunnels(message.data.map(mapToTunnel));
}

function handleRequestsMessage(message: RequestsResponse) {
  const requestsPerTunnel: { [tunnelName: string]: ReturnType<typeof mapToTunneledRequest>[] } = {};

  if (message.data.length === 0) {
    const tunnelName = getActiveQueryFilter()?.tunnelName;
    if (tunnelName) requestsPerTunnel[tunnelName] = [];
  }

  for (const item of message.data) {
    const tunnelName = item.request.tunnel_name;
    if (!requestsPerTunnel[tunnelName]) requestsPerTunnel[tunnelName] = [];
    requestsPerTunnel[tunnelName].push(mapToTunneledRequest(item.request, item.response));
  }
  for (const tunnelName in requestsPerTunnel) {
    updateRequests(tunnelName, requestsPerTunnel[tunnelName]);
  }
}

function handleNewRequestMessage(message: NewRequestResponse) {
  const { request, response } = message.data;
  const f = getActiveQueryFilter();
  if (f && f.tunnelName === request.tunnel_name) {
    if (f.method && request.method.toUpperCase() !== f.method.toUpperCase()) return;
    if (f.urlContains && !request.url.toLowerCase().includes(f.urlContains.toLowerCase())) return;
    if (f.status) {
      const s = response?.status;
      if (!s) return;
      if ("Exact" in f.status && s !== f.status.Exact) return;
      if ("Class" in f.status && Math.floor(s / 100) !== f.status.Class) return;
    }
  }
  addRequests(request.tunnel_name, [mapToTunneledRequest(request, response)]);
}

function handleWebSocketMessagesMessage(message: WebSocketMessagesResponse) {
  if (message.data.length === 0) return;
  const requestId = message.data[0].upgrade_request_id;
  const messages = message.data
    .map((m) => ({
      dir: parseWsDirection(m.direction),
      ts: new Date(m.timestamp),
      data: decodeWsPayload(m.payload, m.message_type),
    }))
    .sort((a, b) => a.ts.getTime() - b.ts.getTime());
  setWsMessages(requestId, messages);
}

function handleNewWebSocketMessageMessage(message: NewWebSocketMessageResponse) {
  const m = message.data;
  addWsMessage(m.upgrade_request_id, {
    dir: parseWsDirection(m.direction),
    ts: new Date(m.timestamp),
    data: decodeWsPayload(m.payload, m.message_type),
  });
}

function handleTunnelCreatedMessage(message: TunnelCreatedResponse) {
  addTunnel(mapToTunnel(message.data));
}

function handleTunnelUpdatedMessage(message: TunnelUpdatedResponse) {
  updateTunnel(mapToTunnel(message.data));
}

function handleTunnelDeletedMessage(message: TunnelDeletedResponse) {
  removeTunnel(message.data.name);
}

function handleSyncReportMessage(message: SyncReportResponse) {
  lastSyncReport.value = {
    added: message.data.added,
    removed: message.data.removed,
    unknownHosts: message.data.unknown_hosts,
    errors: message.data.errors,
  };
}

function handleCloudflareStatusMessage(message: CloudflareStatusResponse) {
  cloudflareStatus.value = {
    configured: message.data.configured,
    tunnelId: message.data.tunnel_id,
    tunnelName: message.data.tunnel_name,
    serviceRunning: message.data.service_running,
  };
}

function handleErrorMessage(message: ErrorResponse) {
  console.error("Server error:", message.data);
}

// ── Public API ────────────────────────────────────────────────────────────────

function send(payload: unknown) {
  ws?.send(JSON.stringify(payload));
}

/** Requests the current list of configured tunnels from the server. */
export function queryTunnels() {
  send({ type: "ListTunnels" });
}

/**
 * Requests stored requests for a specific tunnel, with optional filters.
 */
export function queryRequests(
  tunnelName: string,
  method?: string,
  status?: StatusFilter,
  urlContains?: string,
  since?: Date,
  until?: Date,
  sortField?: "Timestamp" | "ResponseTime",
  sortDirection?: "Asc" | "Desc",
) {
  const data: QueryFilter = {
    tunnel_name: tunnelName,
    method,
    status,
    url_contains: urlContains,
    since: since?.toISOString(),
    until: until?.toISOString(),
    sort_field: sortField,
    sort_direction: sortDirection,
  };
  send({ type: "QueryRequests", data });
}

/** Requests stored WebSocket messages for a given upgraded HTTP request. */
export function queryWebSocketMessages(requestId: string) {
  send({ type: "QueryWebSocketMessages", data: { upgrade_request_id: requestId } });
}

/** Creates a new tunnel (persists to config.toml and optionally Cloudflare). */
export function createTunnel(
  name: string,
  domain: string,
  targetPort: number,
  socketPath?: string,
) {
  send({
    type: "CreateTunnel",
    data: { name, domain, target_port: targetPort, socket_path: socketPath ?? null },
  });
}

/** Updates an existing tunnel's properties. */
export function updateTunnelRemote(
  name: string,
  updates: { domain?: string; socketPath?: string; targetPort?: number; enabled?: boolean },
) {
  send({
    type: "UpdateTunnel",
    data: {
      name,
      domain: updates.domain ?? null,
      socket_path: updates.socketPath ?? null,
      target_port: updates.targetPort ?? null,
      enabled: updates.enabled ?? null,
    },
  });
}

/** Deletes a tunnel (removes from config.toml and Cloudflare). */
export function deleteTunnel(name: string) {
  send({ type: "DeleteTunnel", data: { name } });
}

/** Triggers a full sync of enabled tunnels to Cloudflare. */
export function syncTunnels() {
  send({ type: "SyncTunnels" });
}

/** Confirms removal of unknown hosts found during sync. */
export function confirmRemoveHosts(hosts: string[]) {
  send({ type: "ConfirmRemoveHosts", data: { hosts } });
}

/** Requests the current Cloudflare integration status. */
export function getCloudflareStatus() {
  send({ type: "GetCloudflareStatus" });
}

connect();
