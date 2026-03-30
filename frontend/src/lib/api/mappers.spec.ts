import { describe, it, expect } from "vitest";
import { mapToTunneledRequest, decodeWsPayload, parseWsDirection } from "./mappers";
import type { RawRequest } from "./mappers";

const baseRequest: RawRequest = {
  id: "req-1",
  timestamp: "2024-01-01T12:00:00Z",
  tunnel_name: "my-api",
  method: "GET",
  url: "/api/data",
  headers: { "Content-Type": "application/json" },
  body: "",
};

describe("mapToTunneledRequest", () => {
  it("maps required request fields correctly", () => {
    const result = mapToTunneledRequest(baseRequest);
    expect(result.id).toBe("req-1");
    expect(result.tunnelName).toBe("my-api");
    expect(result.method).toBe("GET");
    expect(result.url).toBe("/api/data");
    expect(result.requestHeaders).toEqual({ "Content-Type": "application/json" });
    expect(result.wsMessages).toEqual([]);
  });

  it("converts timestamp string to Date", () => {
    const result = mapToTunneledRequest(baseRequest);
    expect(result.timestamp).toBeInstanceOf(Date);
    expect(result.timestamp.toISOString()).toBe("2024-01-01T12:00:00.000Z");
  });

  it("maps response fields when provided", () => {
    const result = mapToTunneledRequest(baseRequest, {
      status: 200,
      headers: { "Content-Length": "42" },
      response_time_ms: 123,
      body: "cmVzcG9uc2U=",
    });
    expect(result.status).toBe(200);
    expect(result.responseTime).toBe(123);
    expect(result.responseHeaders).toEqual({ "Content-Length": "42" });
    expect(result.responseBody).toBe("cmVzcG9uc2U=");
  });

  it("leaves optional response fields undefined when no response given", () => {
    const result = mapToTunneledRequest(baseRequest);
    expect(result.status).toBeUndefined();
    expect(result.responseTime).toBeUndefined();
    expect(result.responseHeaders).toBeUndefined();
    expect(result.responseBody).toBeUndefined();
  });
});

describe("decodeWsPayload", () => {
  it("decodes Text payloads from base64 to UTF-8", () => {
    // "hello" base64-encoded
    const payload = btoa("hello");
    expect(decodeWsPayload(payload, "Text")).toBe("hello");
  });

  it("returns Binary payloads as-is (base64)", () => {
    const payload = "SGVsbG8=";
    expect(decodeWsPayload(payload, "Binary")).toBe("SGVsbG8=");
  });
});

describe("parseWsDirection", () => {
  it("maps → to out", () => {
    expect(parseWsDirection("→")).toBe("out");
  });

  it("maps any other value to in", () => {
    expect(parseWsDirection("←")).toBe("in");
    expect(parseWsDirection("")).toBe("in");
    expect(parseWsDirection("in")).toBe("in");
  });
});
