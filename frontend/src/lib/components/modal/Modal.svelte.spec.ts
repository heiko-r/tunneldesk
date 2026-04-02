import { describe, it, expect, vi } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import Modal from "./Modal.svelte";

describe("Modal", () => {
  it("renders nothing when open is false", async () => {
    render(Modal, { props: { open: false, title: "TEST", onclose: vi.fn() } });
    await expect.element(page.getByRole("dialog")).not.toBeInTheDocument();
  });

  it("renders the title and dialog when open is true", async () => {
    render(Modal, { props: { open: true, title: "MY MODAL", onclose: vi.fn() } });
    await expect.element(page.getByRole("dialog")).toBeInTheDocument();
    await expect.element(page.getByText("MY MODAL")).toBeInTheDocument();
  });

  it("calls onclose when the overlay backdrop is clicked", async () => {
    const onclose = vi.fn();
    render(Modal, { props: { open: true, title: "TEST", onclose } });

    // Click the backdrop overlay (aria-hidden wrapper)
    const overlay = document.querySelector(".overlay") as HTMLElement;
    overlay.click();

    expect(onclose).toHaveBeenCalledOnce();
  });

  it("does not call onclose when the modal content is clicked", async () => {
    const onclose = vi.fn();
    render(Modal, { props: { open: true, title: "TEST", onclose } });

    // Click the dialog itself — stopPropagation should block the overlay handler
    const dialog = page.getByRole("dialog");
    await dialog.click();

    expect(onclose).not.toHaveBeenCalled();
  });

  it("applies modal-sm class when size is sm", async () => {
    render(Modal, { props: { open: true, title: "SMALL", size: "sm", onclose: vi.fn() } });
    const dialog = page.getByRole("dialog");
    await expect.element(dialog).toHaveClass("modal-sm");
  });

  it("does not apply modal-sm class when size is not provided", async () => {
    render(Modal, { props: { open: true, title: "NORMAL", onclose: vi.fn() } });
    const dialog = page.getByRole("dialog");
    await expect.element(dialog).not.toHaveClass("modal-sm");
  });

  it("applies modal-lg class when size is lg", async () => {
    render(Modal, { props: { open: true, title: "LARGE", size: "lg", onclose: vi.fn() } });
    const dialog = page.getByRole("dialog");
    await expect.element(dialog).toHaveClass("modal-lg");
  });
});
