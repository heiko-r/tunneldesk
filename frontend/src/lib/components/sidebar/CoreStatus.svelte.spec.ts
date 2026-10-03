import { describe, it, expect, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import CoreStatus from "./CoreStatus.svelte";
import type { CoreStatus as Status } from "$lib/types";

function status(extra: Partial<Status> = {}): Status {
  return {
    attachedClients: 1,
    pid: 4242,
    port: 3013,
    configPath: "/home/me/config.toml",
    idleTimeoutSecs: 30,
    shuttingDown: false,
    ...extra,
  };
}

describe("CoreStatus", () => {
  it("shows the port and a singular client count", async () => {
    render(CoreStatus, { props: { status: status(), onquit: vi.fn() } });
    await expect.element(page.getByText("CORE :3013")).toBeInTheDocument();
    await expect.element(page.getByTestId("core-clients")).toHaveTextContent("1 CLIENT");
  });

  it("pluralises the client count", async () => {
    render(CoreStatus, { props: { status: status({ attachedClients: 3 }), onquit: vi.fn() } });
    await expect.element(page.getByTestId("core-clients")).toHaveTextContent("3 CLIENTS");
  });

  it("shows the config path and pid in the tooltip", async () => {
    render(CoreStatus, { props: { status: status(), onquit: vi.fn() } });
    await expect
      .element(page.getByTitle("Shared core for /home/me/config.toml (pid 4242)"))
      .toBeInTheDocument();
  });

  it("asks for confirmation before quitting", async () => {
    const onquit = vi.fn();
    render(CoreStatus, { props: { status: status(), onquit } });
    await page.getByRole("button", { name: "Stop tunnels and quit" }).click();
    await expect.element(page.getByRole("dialog")).toBeInTheDocument();
    expect(onquit).not.toHaveBeenCalled();

    await page.getByRole("button", { name: "STOP & QUIT" }).click();
    expect(onquit).toHaveBeenCalledOnce();
    await expect.element(page.getByRole("dialog")).not.toBeInTheDocument();
  });

  it("does not quit when the confirmation is cancelled", async () => {
    const onquit = vi.fn();
    render(CoreStatus, { props: { status: status(), onquit } });
    await page.getByRole("button", { name: "Stop tunnels and quit" }).click();
    await page.getByRole("button", { name: "CANCEL" }).click();
    await expect.element(page.getByRole("dialog")).not.toBeInTheDocument();
    expect(onquit).not.toHaveBeenCalled();
  });

  it("warns about other attached clients", async () => {
    render(CoreStatus, { props: { status: status({ attachedClients: 3 }), onquit: vi.fn() } });
    await page.getByRole("button", { name: "Stop tunnels and quit" }).click();
    await expect.element(page.getByText(/2 other clients are attached/)).toBeInTheDocument();
  });

  it("uses the singular for one other client", async () => {
    render(CoreStatus, { props: { status: status({ attachedClients: 2 }), onquit: vi.fn() } });
    await page.getByRole("button", { name: "Stop tunnels and quit" }).click();
    await expect.element(page.getByText(/1 other client is attached/)).toBeInTheDocument();
  });

  it("does not warn when this is the only client", async () => {
    render(CoreStatus, { props: { status: status(), onquit: vi.fn() } });
    await page.getByRole("button", { name: "Stop tunnels and quit" }).click();
    await expect.element(page.getByRole("dialog")).toBeInTheDocument();
    await expect.element(page.getByText(/other client/)).not.toBeInTheDocument();
  });

  it("shows STOPPING and disables the quit button while shutting down", async () => {
    render(CoreStatus, { props: { status: status({ shuttingDown: true }), onquit: vi.fn() } });
    await expect.element(page.getByTestId("core-clients")).toHaveTextContent("STOPPING");
    await expect
      .element(page.getByRole("button", { name: "Stop tunnels and quit" }))
      .toBeDisabled();
  });
});
