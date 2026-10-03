import { describe, it, expect } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import CloudflareStatus from "./CloudflareStatus.svelte";
import type { CloudflareStatus as Status, ConnectorState } from "$lib/types";

function status(connector: ConnectorState, extra: Partial<Status> = {}): Status {
  return { configured: true, connector, ...extra };
}

describe("CloudflareStatus", () => {
  const cases: [ConnectorState, string, string][] = [
    ["Connected", "CONNECTED", "green"],
    ["Starting", "STARTING", "yellow"],
    ["External", "EXTERNAL", "blue"],
    ["NotInstalled", "NOT INSTALLED", "red"],
    ["Stopped", "STOPPED", "dim"],
  ];

  for (const [connector, label, tone] of cases) {
    it(`shows ${label} for the ${connector} connector state`, async () => {
      render(CloudflareStatus, { props: { status: status(connector) } });
      await expect.element(page.getByText(label)).toBeInTheDocument();
      await expect.element(page.getByText(label)).toHaveClass(tone);
      await expect.element(page.getByTestId("cf-indicator")).toHaveClass(tone);
    });
  }

  it("explains the external state in a tooltip", async () => {
    render(CloudflareStatus, { props: { status: status("External") } });
    await expect.element(page.getByTitle(/managed outside TunnelDesk/)).toBeInTheDocument();
  });

  it("shows the tunnel name and a shortened tunnel id", async () => {
    render(CloudflareStatus, {
      props: {
        status: status("Connected", {
          tunnelName: "tunneldesk-home",
          tunnelId: "0123456789abcdef",
        }),
      },
    });
    await expect.element(page.getByText("tunneldesk-home")).toBeInTheDocument();
    await expect.element(page.getByText("01234567…")).toBeInTheDocument();
  });

  it("omits tunnel name and id when unknown", async () => {
    render(CloudflareStatus, { props: { status: status("Stopped") } });
    expect(document.querySelector(".cf-tunnel-name")).toBeNull();
    expect(document.querySelector(".cf-tunnel-id")).toBeNull();
  });
});
