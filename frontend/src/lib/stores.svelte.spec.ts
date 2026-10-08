import { describe, it, expect, beforeEach } from "vitest";
import {
  addTunnel,
  addWsMessage,
  appendResponseBody,
  removeTunnel,
  replaceRequest,
  storage,
  updateRequests,
  updateTunnels,
  upsertRequest,
} from "./stores.svelte";
import type { Tunnel, TunneledRequest } from "./types";
import { decodeBase64, encodeBase64 } from "./utils";

const tunnel: Tunnel = {
  name: "api",
  domain: "api.example.com",
  localPort: 3000,
  active: true,
  enabled: true,
  socketPath: "/tmp/api.sock",
};

describe("tunnel store", () => {
  beforeEach(() => updateTunnels([]));

  it("adds a new tunnel", () => {
    addTunnel(tunnel);
    expect(storage.tunnels).toEqual([tunnel]);
  });

  it("replaces an existing tunnel with the same name instead of duplicating it", () => {
    addTunnel(tunnel);
    addTunnel({ ...tunnel, localPort: 4000 });
    expect(storage.tunnels).toHaveLength(1);
    expect(storage.tunnels[0].localPort).toBe(4000);
  });

  it("keeps the position of a replaced tunnel", () => {
    addTunnel(tunnel);
    addTunnel({ ...tunnel, name: "web" });
    addTunnel({ ...tunnel, domain: "new.example.com" });
    expect(storage.tunnels.map((t) => t.name)).toEqual(["api", "web"]);
    expect(storage.tunnels[0].domain).toBe("new.example.com");
  });

  it("ignores removal of an unknown tunnel", () => {
    addTunnel(tunnel);
    removeTunnel("missing");
    removeTunnel("api");
    removeTunnel("api");
    expect(storage.tunnels).toEqual([]);
  });
});

function request(id: string, overrides: Partial<TunneledRequest> = {}): TunneledRequest {
  return {
    id,
    tunnelName: "api",
    timestamp: new Date(0),
    method: "GET",
    url: `/${id}`,
    status: 200,
    requestHeaders: {},
    requestBody: null,
    wsMessages: [],
    ...overrides,
  };
}

function requests(): TunneledRequest[] {
  return storage.requests.get("api") ?? [];
}

function bodyText(id: string): string {
  const body = requests().find((r) => r.id === id)?.responseBody ?? "";
  return new TextDecoder().decode(decodeBase64(body));
}

describe("request store", () => {
  beforeEach(() => updateRequests("api", []));

  it("prepends new requests", () => {
    upsertRequest("api", request("a"));
    upsertRequest("api", request("b"));
    expect(requests().map((r) => r.id)).toEqual(["b", "a"]);
  });

  it("replaces a known request in place, keeping its WebSocket messages", () => {
    upsertRequest("api", request("a", { streaming: true }));
    upsertRequest("api", request("b"));
    addWsMessage("a", { dir: "in", ts: new Date(0), data: "hi" });
    upsertRequest("api", request("a", { streaming: false, status: 201 }));
    expect(requests().map((r) => r.id)).toEqual(["b", "a"]);
    expect(requests()[1]).toMatchObject({ streaming: false, status: 201 });
    expect(requests()[1].wsMessages).toHaveLength(1);
  });

  it("only replaces requests that are already known", () => {
    expect(replaceRequest("api", request("a"))).toBe(false);
    expect(requests()).toEqual([]);
  });
});

describe("appendResponseBody", () => {
  beforeEach(() => updateRequests("api", [request("sse", { streaming: true, responseBody: "" })]));

  const chunk = (text: string) => encodeBase64(text);

  it("appends bytes in order across base64 padding boundaries", () => {
    expect(appendResponseBody("api", "sse", 0, chunk("a"))).toBe("applied");
    expect(appendResponseBody("api", "sse", 1, chunk("bc"))).toBe("applied");
    expect(appendResponseBody("api", "sse", 3, chunk("data: 1\n\n"))).toBe("applied");
    expect(appendResponseBody("api", "sse", 12, chunk("ü"))).toBe("applied");
    expect(bodyText("sse")).toBe("abcdata: 1\n\nü");
  });

  it("ignores bytes that were already applied", () => {
    appendResponseBody("api", "sse", 0, chunk("abc"));
    expect(appendResponseBody("api", "sse", 0, chunk("abc"))).toBe("ignored");
    expect(bodyText("sse")).toBe("abc");
  });

  it("applies only the new part of an overlapping append", () => {
    appendResponseBody("api", "sse", 0, chunk("abc"));
    expect(appendResponseBody("api", "sse", 1, chunk("bcde"))).toBe("applied");
    expect(bodyText("sse")).toBe("abcde");
  });

  it("reports missing bytes as a gap", () => {
    expect(appendResponseBody("api", "sse", 5, chunk("late"))).toBe("gap");
    expect(bodyText("sse")).toBe("");
  });

  it("ignores appends for unknown or completed responses", () => {
    upsertRequest("api", request("done", { streaming: false, responseBody: "" }));
    expect(appendResponseBody("api", "missing", 0, chunk("x"))).toBe("ignored");
    expect(appendResponseBody("api", "done", 0, chunk("x"))).toBe("ignored");
    expect(appendResponseBody("other", "sse", 0, chunk("x"))).toBe("ignored");
  });

  it("treats a missing body as empty", () => {
    updateRequests("api", [request("sse", { streaming: true })]);
    expect(appendResponseBody("api", "sse", 0, chunk("x"))).toBe("applied");
    expect(bodyText("sse")).toBe("x");
  });
});
