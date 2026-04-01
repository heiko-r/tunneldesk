import { describe, it, expect } from "vitest";
import { render } from "vitest-browser-svelte";
import { page } from "vitest/browser";
import BodyPreview from "./BodyPreview.svelte";

/** base64-encode a UTF-8 string (browser-compatible) */
function b64(text: string): string {
  return btoa(new TextEncoder().encode(text).reduce((s, b) => s + String.fromCharCode(b), ""));
}

describe("BodyPreview", () => {
  it("shows empty-state message when body is null", async () => {
    render(BodyPreview, { props: { body: null as unknown as string } });
    await expect.element(page.getByText("No body content.")).toBeInTheDocument();
  });

  it("renders a view selector dropdown", async () => {
    render(BodyPreview, { props: { body: b64("hello") } });
    const trigger = document.querySelector('button[aria-haspopup="listbox"]');
    expect(trigger).not.toBeNull();
  });

  it("auto-selects JSON view for application/json mime type", async () => {
    const json = b64('{"key":"value"}');
    render(BodyPreview, { props: { body: json, mimeType: "application/json" } });
    await expect.element(page.getByRole("button", { name: /formatted json/i })).toBeInTheDocument();
  });

  it("auto-selects XML view for application/xml mime type", async () => {
    const xml = b64("<root><item>1</item></root>");
    render(BodyPreview, { props: { body: xml, mimeType: "application/xml" } });
    await expect.element(page.getByRole("button", { name: /formatted xml/i })).toBeInTheDocument();
  });

  it("renders formatted JSON in the code block", async () => {
    const json = b64('{"name":"alice","age":30}');
    render(BodyPreview, { props: { body: json, mimeType: "application/json" } });
    const pre = document.querySelector("pre.code-block");
    expect(pre?.textContent).toContain('"name"');
    expect(pre?.textContent).toContain('"alice"');
  });

  it("renders plain UTF-8 text for text/plain mime type", async () => {
    const text = b64("plain text content");
    render(BodyPreview, { props: { body: text, mimeType: "text/plain" } });
    const pre = document.querySelector("pre.code-block");
    expect(pre?.textContent).toContain("plain text content");
  });

  it("renders a sandboxed iframe for text/html mime type — XSS safety", async () => {
    const html = b64("<html><body><script>alert(1)</script></body></html>");
    render(BodyPreview, { props: { body: html, mimeType: "text/html" } });
    // Must use a sandboxed iframe, not {@html ...}
    const iframe = document.querySelector("iframe.html-preview") as HTMLIFrameElement | null;
    expect(iframe).not.toBeNull();
    expect(iframe?.getAttribute("sandbox")).toBe("");
    // No bare html-preview div (the old {@html} pattern)
    expect(document.querySelector("div.html-preview")).toBeNull();
  });

  it("renders an image tag for image/* mime type", async () => {
    const fakeImg = b64("\x89PNG\r\n");
    render(BodyPreview, { props: { body: fakeImg, mimeType: "image/png" } });
    await expect.element(page.getByRole("img", { name: "Preview" })).toBeInTheDocument();
  });

  it("auto-detects JSON from content when no mime type provided", async () => {
    render(BodyPreview, { props: { body: b64('{"auto":true}') } });
    await expect.element(page.getByRole("button", { name: /formatted json/i })).toBeInTheDocument();
  });
});
