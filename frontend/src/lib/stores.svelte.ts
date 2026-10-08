import { SvelteMap } from "svelte/reactivity";
import type { CloudflareStatus, CoreStatus, SyncReport, Tunnel, TunneledRequest } from "./types";
import { appendBase64, base64ByteLength, decodeBase64 } from "./utils";

/** Global reactive store for all tunnels and their captured requests. */
export const storage: { tunnels: Tunnel[]; requests: SvelteMap<string, TunneledRequest[]> } =
  $state({
    tunnels: [],
    requests: new SvelteMap<string, TunneledRequest[]>(),
  });

/** Cloudflare integration status, populated by GetCloudflareStatus responses. */
export const cloudflareStatus: { value: CloudflareStatus | null } = $state({ value: null });

/** Status of the shared core, populated by CoreStatus and ShuttingDown messages. */
export const coreStatus: { value: CoreStatus | null } = $state({ value: null });

/** Latest sync report, populated by SyncReport responses. */
export const lastSyncReport: { value: SyncReport | null } = $state({ value: null });

/** ID of the most recently completed replay, populated by ReplayResponse responses. */
export const lastReplayedId: { value: string | null; error: string | null } = $state({
  value: null,
  error: null,
});

type StatusFilter = { Exact: number } | { Class: number };
type ActiveQueryFilter = {
  tunnelName: string;
  method?: string;
  urlContains?: string;
  status?: StatusFilter;
} | null;

const _activeQueryFilter: { value: ActiveQueryFilter } = $state({ value: null });

/**
 * The active server-side query filter for the currently viewed tunnel.
 * Used by the WebSocket handler to pre-filter incoming NewRequest push events
 * so only matching requests are added to the store.
 */
export function getActiveQueryFilter(): ActiveQueryFilter {
  return _activeQueryFilter.value;
}

export function setActiveQueryFilter(filter: ActiveQueryFilter) {
  _activeQueryFilter.value = filter;
}

/** Replaces the full list of tunnels (e.g., after receiving a Tunnels message). */
export function updateTunnels(newTunnels: Tunnel[]) {
  storage.tunnels = newTunnels;
}

/** Updates a single tunnel in the list by name. */
export function updateTunnel(updated: Tunnel) {
  storage.tunnels = storage.tunnels.map((t) => (t.name === updated.name ? updated : t));
}

/**
 * Adds a new tunnel to the list, or replaces the existing entry with the same name.
 * Tunnel changes are broadcast to every attached client, so the client that made
 * the change receives the same tunnel twice.
 */
export function addTunnel(tunnel: Tunnel) {
  if (storage.tunnels.some((t) => t.name === tunnel.name)) {
    updateTunnel(tunnel);
  } else {
    storage.tunnels = [...storage.tunnels, tunnel];
  }
}

/** Removes a tunnel from the list by name. */
export function removeTunnel(name: string) {
  storage.tunnels = storage.tunnels.filter((t) => t.name !== name);
  storage.requests.delete(name);
}

/** Sets the full request list for a specific tunnel, replacing any existing entries. */
export function updateRequests(tunnelName: string, newRequests: TunneledRequest[]) {
  storage.requests.set(tunnelName, newRequests);
}

/**
 * Replaces the request with the same ID in place, or prepends it when it is
 * new. A streaming response is pushed when its head arrives and again when it
 * completes, so the same request can arrive more than once.
 */
export function upsertRequest(tunnelName: string, request: TunneledRequest) {
  if (!replaceRequest(tunnelName, request)) {
    storage.requests.set(tunnelName, [request, ...(storage.requests.get(tunnelName) || [])]);
  }
}

/**
 * Replaces the request with the same ID in place, keeping its WebSocket
 * messages. Returns `false` when the tunnel has no such request.
 */
export function replaceRequest(tunnelName: string, request: TunneledRequest): boolean {
  const current = storage.requests.get(tunnelName) || [];
  const idx = current.findIndex((r) => r.id === request.id);
  if (idx === -1) return false;
  const updated = [...current];
  updated[idx] = { ...request, wsMessages: current[idx].wsMessages };
  storage.requests.set(tunnelName, updated);
  return true;
}

/**
 * Outcome of applying a streamed body append: `"gap"` means bytes before
 * `offset` are missing, so the request must be fetched again.
 */
export type AppendResult = "applied" | "ignored" | "gap";

/**
 * Appends base64 `data` at byte `offset` to the body of a streaming response.
 * Appends for unknown or completed requests and already-applied bytes are ignored.
 */
export function appendResponseBody(
  tunnelName: string,
  requestId: string,
  offset: number,
  data: string,
): AppendResult {
  const requests = storage.requests.get(tunnelName);
  const idx = requests?.findIndex((r) => r.id === requestId) ?? -1;
  if (!requests || idx === -1 || !requests[idx].streaming) return "ignored";
  const body = requests[idx].responseBody ?? "";
  const length = base64ByteLength(body);
  if (offset > length) return "gap";
  const bytes = decodeBase64(data);
  if (offset + bytes.length <= length) return "ignored";
  const updated = [...requests];
  updated[idx] = {
    ...requests[idx],
    responseBody: appendBase64(body, bytes.subarray(length - offset)),
  };
  storage.requests.set(tunnelName, updated);
  return "applied";
}

/**
 * Replaces the wsMessages array on a specific request identified by ID.
 * Searches across all tunnels to locate the request.
 * @param requestId - ID of the request to update
 * @param messages - New WebSocket message list
 */
export function setWsMessages(requestId: string, messages: TunneledRequest["wsMessages"]) {
  for (const [tunnelName, requests] of storage.requests.entries()) {
    const idx = requests.findIndex((r) => r.id === requestId);
    if (idx !== -1) {
      const updated = [...requests];
      updated[idx] = { ...requests[idx], wsMessages: messages };
      storage.requests.set(tunnelName, updated);
      return;
    }
  }
}

/**
 * Appends a single WebSocket message to the end of a request's wsMessages array.
 * Searches across all tunnels to locate the request.
 * @param requestId - ID of the request to update
 * @param message - WebSocket message to append
 */
export function addWsMessage(requestId: string, message: TunneledRequest["wsMessages"][0]) {
  for (const [tunnelName, requests] of storage.requests.entries()) {
    const idx = requests.findIndex((r) => r.id === requestId);
    if (idx !== -1) {
      const updated = [...requests];
      updated[idx] = { ...requests[idx], wsMessages: [...requests[idx].wsMessages, message] };
      storage.requests.set(tunnelName, updated);
      return;
    }
  }
}
