import {
  getActiveQueryFilter,
  addRequests,
  addWsMessage,
  setWsMessages,
  updateRequests,
  updateTunnels,
} from "$lib/stores.svelte";
import { mapToTunneledRequest, decodeWsPayload, parseWsDirection } from "./mappers";

type QueryType =
  | "ListTunnels"
  | "QueryRequests"
  | "QueryWebSocketMessages"
  | "Subscribe"
  | "Unsubscribe";
const RESPONSE_TYPES = [
  "Tunnels",
  "Requests",
  "NewRequest",
  "WebSocketMessages",
  "NewWebSocketMessage",
] as const;
type ResponseType = (typeof RESPONSE_TYPES)[number];

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

type QueryPayload = {
  type: QueryType;
  data?: QueryFilter;
};

type ResponseTunnel = {
  name: string;
  domain: string;
  socket_path: string;
  destination: number;
};

type TunnelsResponse = { type: "Tunnels"; data: ResponseTunnel[] };

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

type RequestsResponse = {
  type: "Requests";
  data: { request: RawRequestData; response?: RawResponseData }[];
};

type NewRequestResponse = {
  type: "NewRequest";
  data: { request: RawRequestData; response?: RawResponseData };
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

type WebSocketMessagesResponse = { type: "WebSocketMessages"; data: WsMessageData[] };
type NewWebSocketMessageResponse = { type: "NewWebSocketMessage"; data: WsMessageData };

let ws: WebSocket | null = null;

/** Reactive connection state. Mutate `.connected` rather than reassigning. */
export const connectionState = $state({ connected: false });

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
  };

  ws.onmessage = (event) => {
    try {
      const message = JSON.parse(event.data);
      console.log("Received message:", message);
      const messageType: string | undefined = message["type"];
      if (!messageType || !RESPONSE_TYPES.includes(messageType as ResponseType)) return;

      switch (messageType) {
        case "Tunnels":
          handleTunnelsMessage(message);
          break;
        case "Requests":
          handleRequestsMessage(message);
          break;
        case "NewRequest":
          handleNewRequestMessage(message);
          break;
        case "WebSocketMessages":
          handleWebSocketMessagesMessage(message);
          break;
        case "NewWebSocketMessage":
          handleNewWebSocketMessageMessage(message);
          break;
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

function handleTunnelsMessage(message: TunnelsResponse) {
  updateTunnels(
    message.data.map((t) => ({
      name: t.name,
      domain: t.domain,
      localPort: t.destination,
      active: true,
    })),
  );
}

function handleRequestsMessage(message: RequestsResponse) {
  const requestsPerTunnel: { [tunnelName: string]: ReturnType<typeof mapToTunneledRequest>[] } = {};

  // When the server returns an empty result, ensure the queried tunnel's store is cleared.
  // Without this, an empty response would leave stale entries visible in the UI.
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

/**
 * Requests the current list of configured tunnels from the server.
 */
export function queryTunnels() {
  const query: QueryPayload = { type: "ListTunnels" };
  ws?.send(JSON.stringify(query));
}

/**
 * Requests stored requests for a specific tunnel, with optional filters.
 * @param tunnelName - Name of the tunnel to query
 * @param method - Filter by HTTP method
 * @param status - Filter by status: exact code or hundred-class (e.g. `{ Class: 2 }` for 2xx)
 * @param urlContains - Filter by URL substring
 * @param since - Return only requests after this date
 * @param until - Return only requests before this date
 * @param sortField - Field to sort by ('Timestamp' or 'ResponseTime')
 * @param sortDirection - Sort order ('Asc' or 'Desc')
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
  const query: QueryPayload = {
    type: "QueryRequests",
    data: {
      tunnel_name: tunnelName,
      method,
      status,
      url_contains: urlContains,
      since: since?.toISOString(),
      until: until?.toISOString(),
      sort_field: sortField,
      sort_direction: sortDirection,
    },
  };
  ws?.send(JSON.stringify(query));
}

/**
 * Requests stored WebSocket messages for a given upgraded HTTP request.
 * @param requestId - The ID of the WebSocket upgrade request
 */
export function queryWebSocketMessages(requestId: string) {
  const query = {
    type: "QueryWebSocketMessages",
    data: { upgrade_request_id: requestId },
  };
  ws?.send(JSON.stringify(query));
}

connect();
