import { afterAll, beforeAll, describe, it, expect } from "vitest";
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

  it("keeps the selected view while a streaming body grows", async () => {
    const screen = render(BodyPreview, {
      props: { body: b64("data: 1\n\n"), mimeType: "text/event-stream", live: true },
    });
    await page.getByRole("button", { name: /raw utf-8/i }).click();
    await page.getByRole("option", { name: "Raw Hex" }).click();

    await screen.rerender({ body: b64("data: 1\n\ndata: 2\n\n") });
    await expect.element(page.getByRole("button", { name: /raw hex/i })).toBeInTheDocument();
    await expect.element(page.getByTestId("body-content")).toHaveTextContent(/32 0a 0a$/);
  });

  it("keeps the selected view while a body without mime type grows", async () => {
    const screen = render(BodyPreview, { props: { body: b64('{"a":'), live: true } });
    await expect.element(page.getByRole("button", { name: /formatted json/i })).toBeInTheDocument();
    await page.getByRole("button", { name: /formatted json/i }).click();
    await page.getByRole("option", { name: "Raw UTF-8" }).click();

    await screen.rerender({ body: b64('{"a":1}') });
    await expect.element(page.getByRole("button", { name: /raw utf-8/i })).toBeInTheDocument();
    await expect.element(page.getByTestId("body-content")).toHaveTextContent('{"a":1}');
  });

  describe("live bodies", () => {
    // The global stylesheet, which limits the code block height, is not loaded here.
    const style = document.createElement("style");
    style.textContent = ".code-block { max-height: 100px; overflow-y: auto; }";
    beforeAll(() => document.head.append(style));
    afterAll(() => style.remove());

    const lines = (n: number) => b64(Array.from({ length: n }, (_, i) => `data: ${i}\n`).join(""));
    const scroller = () => document.querySelector("pre.code-block") as HTMLElement;
    const atEnd = (el: HTMLElement) => el.scrollTop + el.clientHeight >= el.scrollHeight - 2;

    it("start scrolled to the end and follow new data", async () => {
      const screen = render(BodyPreview, {
        props: { body: lines(100), mimeType: "text/plain", live: true },
      });
      await expect.poll(() => scroller().scrollTop).toBeGreaterThan(0);
      expect(atEnd(scroller())).toBe(true);

      await screen.rerender({ body: lines(200) });
      await expect.poll(() => atEnd(scroller())).toBe(true);
    });

    it("stop following once scrolled away from the end", async () => {
      const screen = render(BodyPreview, {
        props: { body: lines(100), mimeType: "text/plain", live: true },
      });
      await expect.poll(() => scroller().scrollTop).toBeGreaterThan(0);
      scroller().scrollTop = 0;

      await screen.rerender({ body: lines(200) });
      await expect.element(page.getByTestId("body-content")).toHaveTextContent(/data: 199/);
      expect(scroller().scrollTop).toBe(0);
    });

    it("are not scrolled when complete", async () => {
      render(BodyPreview, { props: { body: lines(100), mimeType: "text/plain" } });
      await expect.element(page.getByTestId("body-content")).toHaveTextContent(/data: 99/);
      expect(scroller().scrollTop).toBe(0);
    });
  });
});
