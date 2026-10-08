import { describe, it, expect, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import RequestTable from "./RequestTable.svelte";
import type { TunneledRequest } from "$lib/types";

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

function renderTable(requests: TunneledRequest[], onRowSelect = vi.fn()) {
  return render(RequestTable, {
    props: {
      requests,
      selectedRequestId: null,
      sortField: "Timestamp",
      sortDir: "desc",
      onRowSelect,
    },
  });
}

describe("RequestTable", () => {
  it("shows an empty state without requests", async () => {
    renderTable([]);
    await expect.element(page.getByText(/No requests yet/)).toBeInTheDocument();
  });

  it("marks responses that are still streaming as live", async () => {
    const screen = renderTable([
      request("events", { streaming: true }),
      request("done", { streaming: false }),
    ]);
    await expect.element(page.getByText("LIVE")).toBeInTheDocument();
    expect(page.getByText("LIVE").elements()).toHaveLength(1);

    await screen.rerender({ requests: [request("events", { streaming: false })] });
    await expect.element(page.getByText("LIVE")).not.toBeInTheDocument();
  });

  it("selects a row on click", async () => {
    const onRowSelect = vi.fn();
    renderTable([request("a")], onRowSelect);
    await page.getByText("/a").click();
    expect(onRowSelect).toHaveBeenCalledWith("a");
  });
});
