import type { HttpMethod, TunneledRequest } from "$lib/types";

type RawRequest = {
  id: string;
  timestamp: string;
  tunnel_name: string;
  method: string;
  url: string;
  headers: { [key: string]: string };
  body: string;
};

type RawResponse = {
  status: number;
  headers: { [key: string]: string };
  response_time_ms?: number;
  body: string;
};

type RawWsMessage = {
  timestamp: string;
  direction: string;
  message_type: "Text" | "Binary";
  payload: string;
};

/**
 * Maps a raw request/response pair from the WebSocket API to a TunneledRequest.
 * @param raw - Raw request object from the server
 * @param response - Optional raw response object
 */
export function mapToTunneledRequest(raw: RawRequest, response?: RawResponse): TunneledRequest {
  return {
    id: raw.id,
    tunnelName: raw.tunnel_name,
    timestamp: new Date(raw.timestamp),
    method: raw.method as HttpMethod,
    url: raw.url,
    status: response?.status,
    responseTime: response?.response_time_ms,
    requestHeaders: raw.headers,
    responseHeaders: response?.headers,
    requestBody: raw.body,
    responseBody: response?.body,
    wsMessages: [],
  };
}

/**
 * Decodes a WebSocket message payload from base64.
 * Text payloads are decoded to a UTF-8 string; binary payloads remain as base64.
 * @param payload - Base64-encoded payload string
 * @param messageType - Whether the payload is text or binary
 */
export function decodeWsPayload(payload: string, messageType: "Text" | "Binary"): string {
  if (messageType === "Text") {
    const bytes = Uint8Array.from(atob(payload), (c) => c.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  }
  return payload;
}

/**
 * Converts a WebSocket direction arrow character to a canonical direction string.
 * @param direction - Arrow character from the server ('→' for outgoing, anything else for incoming)
 */
export function parseWsDirection(direction: string): "in" | "out" {
  return direction === "→" ? "out" : "in";
}

export type { RawRequest, RawResponse, RawWsMessage };
