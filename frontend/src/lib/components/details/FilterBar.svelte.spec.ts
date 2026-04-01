import { describe, it, expect } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import FilterBar from "./FilterBar.svelte";

describe("FilterBar", () => {
  it("renders method and status dropdowns and URL input", async () => {
    render(FilterBar, {
      props: {
        filterMethod: "",
        filterStatus: "",
        filterUrl: null,
        count: 10,
      },
    });
    const triggers = page.getByRole("button", { expanded: false });
    await expect.element(triggers.nth(0)).toBeInTheDocument();
    await expect.element(triggers.nth(1)).toBeInTheDocument();
    await expect.element(page.getByPlaceholder("Filter URL…")).toBeInTheDocument();
  });

  it("shows request count", async () => {
    render(FilterBar, {
      props: {
        filterMethod: "",
        filterStatus: "",
        filterUrl: null,
        count: 17,
      },
    });
    await expect.element(page.getByText("17")).toBeInTheDocument();
  });

  it("lists all HTTP methods in the method dropdown", async () => {
    render(FilterBar, {
      props: {
        filterMethod: "",
        filterStatus: "",
        filterUrl: null,
        count: 0,
      },
    });
    // Open the method dropdown (first trigger button)
    await page.getByRole("button", { expanded: false }).nth(0).click();
    for (const method of ["GET", "POST", "DELETE", "PATCH"]) {
      await expect.element(page.getByRole("option", { name: method })).toBeInTheDocument();
    }
  });

  it("shows ALL METHODS and ALL STATUS as default options", async () => {
    render(FilterBar, {
      props: {
        filterMethod: "",
        filterStatus: "",
        filterUrl: null,
        count: 0,
      },
    });
    await expect.element(page.getByText("ALL METHODS")).toBeInTheDocument();
    await expect.element(page.getByText("ALL STATUS")).toBeInTheDocument();
  });
});
