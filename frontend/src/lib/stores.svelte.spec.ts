import { describe, it, expect, beforeEach } from "vitest";
import { addTunnel, removeTunnel, storage, updateTunnels } from "./stores.svelte";
import type { Tunnel } from "./types";

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
