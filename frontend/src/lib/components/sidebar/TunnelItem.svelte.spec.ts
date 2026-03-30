import { describe, it, expect, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import TunnelItem from "./TunnelItem.svelte";
import type { Tunnel } from "$lib/types";

const activeTunnel: Tunnel = {
  name: "my-api",
  domain: "myapi.example.com",
  localPort: 3000,
  active: true,
};

const inactiveTunnel: Tunnel = { ...activeTunnel, active: false };

describe("TunnelItem", () => {
  it("renders the tunnel name, domain, and port", async () => {
    render(TunnelItem, { props: { tunnel: activeTunnel, selected: false, onclick: vi.fn() } });
    await expect.element(page.getByText("my-api")).toBeInTheDocument();
    await expect.element(page.getByText("myapi.example.com")).toBeInTheDocument();
    await expect.element(page.getByText(":3000")).toBeInTheDocument();
  });

  it("applies selected class when selected is true", async () => {
    render(TunnelItem, { props: { tunnel: activeTunnel, selected: true, onclick: vi.fn() } });
    const btn = page.getByRole("button");
    await expect.element(btn).toHaveClass("selected");
  });

  it("does not apply selected class when selected is false", async () => {
    render(TunnelItem, { props: { tunnel: activeTunnel, selected: false, onclick: vi.fn() } });
    const btn = page.getByRole("button");
    await expect.element(btn).not.toHaveClass("selected");
  });

  it("applies inactive class when tunnel is not active", async () => {
    render(TunnelItem, { props: { tunnel: inactiveTunnel, selected: false, onclick: vi.fn() } });
    const btn = page.getByRole("button");
    await expect.element(btn).toHaveClass("inactive");
  });

  it("does not apply inactive class when tunnel is active", async () => {
    render(TunnelItem, { props: { tunnel: activeTunnel, selected: false, onclick: vi.fn() } });
    const btn = page.getByRole("button");
    await expect.element(btn).not.toHaveClass("inactive");
  });

  it("calls onclick when clicked", async () => {
    const onclick = vi.fn();
    render(TunnelItem, { props: { tunnel: activeTunnel, selected: false, onclick } });
    await page.getByRole("button").click();
    expect(onclick).toHaveBeenCalledOnce();
  });

  it("renders as a button element for keyboard accessibility", async () => {
    render(TunnelItem, { props: { tunnel: activeTunnel, selected: false, onclick: vi.fn() } });
    await expect.element(page.getByRole("button")).toBeInTheDocument();
  });
});
