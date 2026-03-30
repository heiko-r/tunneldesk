import { SvelteMap } from "svelte/reactivity";
import type { Tunnel, TunneledRequest } from "./types";

/** Global reactive store for all tunnels and their captured requests. */
export const storage: { tunnels: Tunnel[]; requests: SvelteMap<string, TunneledRequest[]> } =
  $state({
    tunnels: [],
    requests: new SvelteMap<string, TunneledRequest[]>(),
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

/** Sets the full request list for a specific tunnel, replacing any existing entries. */
export function updateRequests(tunnelName: string, newRequests: TunneledRequest[]) {
  storage.requests.set(tunnelName, newRequests);
}

/** Prepends new requests to the front of the list for a specific tunnel. */
export function addRequests(tunnelName: string, newRequests: TunneledRequest[]) {
  const current = storage.requests.get(tunnelName) || [];
  storage.requests.set(tunnelName, [...newRequests, ...current]);
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
