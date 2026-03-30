import { describe, it, expect } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import FilterBar from "./FilterBar.svelte";

describe("FilterBar", () => {
  it("renders method and status dropdowns and URL input", async () => {
    render(FilterBar, {
      props: {
        filterMethod: null,
        filterStatus: null,
        filterUrl: null,
        count: 10,
      },
    });
    await expect.element(page.getByRole("combobox", { name: /method/i })).not.toBeInTheDocument();
    // Verify both selects are present by checking for their default options
    const selects = document.querySelectorAll("select");
    expect(selects).toHaveLength(2);
    await expect.element(page.getByPlaceholder("Filter URL…")).toBeInTheDocument();
  });

  it("shows request count", async () => {
    render(FilterBar, {
      props: {
        filterMethod: null,
        filterStatus: null,
        filterUrl: null,
        count: 17,
      },
    });
    await expect.element(page.getByText("17")).toBeInTheDocument();
  });

  it("lists all HTTP methods in the method dropdown", async () => {
    render(FilterBar, {
      props: {
        filterMethod: null,
        filterStatus: null,
        filterUrl: null,
        count: 0,
      },
    });
    const methodSelect = document.querySelectorAll("select")[0];
    const options = Array.from(methodSelect.querySelectorAll("option")).map((o) => o.value);
    expect(options).toContain("GET");
    expect(options).toContain("POST");
    expect(options).toContain("DELETE");
    expect(options).toContain("PATCH");
  });

  it("shows ALL METHODS and ALL STATUS as default options", async () => {
    render(FilterBar, {
      props: {
        filterMethod: null,
        filterStatus: null,
        filterUrl: null,
        count: 0,
      },
    });
    await expect.element(page.getByText("ALL METHODS")).toBeInTheDocument();
    await expect.element(page.getByText("ALL STATUS")).toBeInTheDocument();
  });
});
