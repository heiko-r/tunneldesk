import { describe, it, expect, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import TunnelItem from "./TunnelItem.svelte";
import type { Tunnel } from "$lib/types";

const enabledTunnel: Tunnel = {
  name: "my-api",
  domain: "myapi.example.com",
  localPort: 3000,
  active: true,
  enabled: true,
  socketPath: "/tmp/my-api.sock",
};

const disabledTunnel: Tunnel = { ...enabledTunnel, enabled: false };

const defaultProps = {
  onclick: vi.fn(),
  onedit: vi.fn(),
  ondelete: vi.fn(),
};

describe("TunnelItem", () => {
  it("renders the tunnel name, domain, and port", async () => {
    render(TunnelItem, { props: { tunnel: enabledTunnel, selected: false, ...defaultProps } });
    await expect.element(page.getByText("my-api")).toBeInTheDocument();
    await expect.element(page.getByText("myapi.example.com")).toBeInTheDocument();
    await expect.element(page.getByText(":3000")).toBeInTheDocument();
  });

  it("applies selected class on wrapper when selected is true", async () => {
    render(TunnelItem, { props: { tunnel: enabledTunnel, selected: true, ...defaultProps } });
    // The wrapper div has the selected class
    const el = document.querySelector(".tunnel-item-wrap");
    expect(el?.classList.contains("selected")).toBe(true);
  });

  it("does not apply selected class when selected is false", async () => {
    render(TunnelItem, { props: { tunnel: enabledTunnel, selected: false, ...defaultProps } });
    const el = document.querySelector(".tunnel-item-wrap");
    expect(el?.classList.contains("selected")).toBe(false);
  });

  it("applies inactive class on inner button when tunnel is disabled", async () => {
    render(TunnelItem, { props: { tunnel: disabledTunnel, selected: false, ...defaultProps } });
    const el = document.querySelector(".tunnel-item");
    expect(el?.classList.contains("inactive")).toBe(true);
  });

  it("does not apply inactive class when tunnel is enabled", async () => {
    render(TunnelItem, { props: { tunnel: enabledTunnel, selected: false, ...defaultProps } });
    const el = document.querySelector(".tunnel-item");
    expect(el?.classList.contains("inactive")).toBe(false);
  });

  it("shows DISABLED badge when tunnel is disabled", async () => {
    render(TunnelItem, { props: { tunnel: disabledTunnel, selected: false, ...defaultProps } });
    await expect.element(page.getByText("DISABLED")).toBeInTheDocument();
  });

  it("does not show DISABLED badge when tunnel is enabled", async () => {
    render(TunnelItem, { props: { tunnel: enabledTunnel, selected: false, ...defaultProps } });
    expect(document.querySelector(".badge-disabled")).toBeNull();
  });

  it("calls onclick when the main tunnel button is clicked", async () => {
    const onclick = vi.fn();
    render(TunnelItem, {
      props: {
        tunnel: enabledTunnel,
        selected: false,
        onclick,
        onedit: vi.fn(),
        ondelete: vi.fn(),
      },
    });
    await page.getByRole("button", { name: "my-api" }).first().click();
    expect(onclick).toHaveBeenCalledOnce();
  });

  it("calls onedit when edit button is clicked", async () => {
    const onedit = vi.fn();
    render(TunnelItem, {
      props: {
        tunnel: enabledTunnel,
        selected: false,
        onclick: vi.fn(),
        onedit,
        ondelete: vi.fn(),
      },
    });
    await page.getByRole("button", { name: "Edit my-api" }).click();
    expect(onedit).toHaveBeenCalledOnce();
  });

  it("calls ondelete when delete button is clicked", async () => {
    const ondelete = vi.fn();
    render(TunnelItem, {
      props: {
        tunnel: enabledTunnel,
        selected: false,
        onclick: vi.fn(),
        onedit: vi.fn(),
        ondelete,
      },
    });
    await page.getByRole("button", { name: "Delete my-api" }).click();
    expect(ondelete).toHaveBeenCalledOnce();
  });
});
