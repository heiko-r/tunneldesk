<script lang="ts">
  import { decodeBase64, bytesToHex, bytesToUtf, formatJson, formatXml } from "$lib/utils";

  let {
    body,
    mimeType,
  }: {
    body: string;
    mimeType?: string;
  } = $props();

  type ViewType = "preview" | "json" | "xml" | "utf" | "hex" | "base64";

  let selectedView: ViewType | null = $derived(autoSelectView(mimeType));

  /** Derives the best default view based on MIME type or body content. */
  function autoSelectView(mime: string | undefined): ViewType {
    const decoded = bytesToUtf(decodeBase64(body));
    if (mime) {
      if (mime.includes("json")) return "json";
      if (mime.includes("xml")) return "xml";
      if (mime.includes("text/html")) return "preview";
      if (mime.includes("text/")) return "utf";
      return "preview";
    }
    if (decoded.trim().startsWith("{") || decoded.trim().startsWith("[")) return "json";
    if (decoded.trim().startsWith("<")) return "xml";
    return "utf";
  }

  let content = $derived.by(() => {
    if (body == null) return null;
    const decoded = decodeBase64(body);
    const utf = bytesToUtf(decoded);
    switch (selectedView) {
      case "preview":
        return body;
      case "json":
        return formatJson(utf);
      case "xml":
        return formatXml(utf);
      case "utf":
        return utf;
      case "hex":
        return bytesToHex(decoded);
      case "base64":
        return body;
      default:
        return null;
    }
  });

  function isImageMime(): boolean {
    return mimeType?.includes("image/") ?? false;
  }

  function isHtmlMime(): boolean {
    return mimeType?.includes("text/html") ?? false;
  }
</script>

{#if content}
  <div class="body-preview">
    <div class="view-selector">
      <select bind:value={selectedView} class="view-select">
        <option value="preview">Preview</option>
        <option value="json">Formatted JSON</option>
        <option value="xml">Formatted XML</option>
        <option value="utf">Raw UTF-8</option>
        <option value="hex">Raw Hex</option>
        <option value="base64">Raw Base64</option>
      </select>
    </div>

    <div class="content-display">
      {#if selectedView === "preview"}
        {#if isImageMime()}
          <div class="image-preview">
            <img src={`data:${mimeType || "image/png"};base64,${content}`} alt="Preview" />
          </div>
        {:else if isHtmlMime()}
          <iframe
            class="html-preview"
            srcdoc={bytesToUtf(decodeBase64(content))}
            sandbox=""
            title="HTML preview"
          ></iframe>
        {:else}
          <pre class="code-block">{bytesToUtf(decodeBase64(content))}</pre>
        {/if}
      {:else}
        <pre class="code-block">{content}</pre>
      {/if}
    </div>
  </div>
{:else}
  <div class="empty-state">No body content.</div>
{/if}

<style>
  .body-preview {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .view-selector {
    display: flex;
    justify-content: flex-end;
    margin-bottom: 4px;
  }

  .view-select {
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 10px;
    padding: 4px 8px;
    cursor: pointer;
  }
  .view-select:hover {
    border-color: var(--green);
  }
  .view-select:focus {
    outline: none;
    border-color: var(--green);
    box-shadow: 0 0 0 1px var(--green);
  }

  .content-display {
    width: 100%;
  }

  .image-preview {
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 8px;
    background: var(--bg2);
    text-align: center;
  }
  .image-preview img {
    max-width: 100%;
    max-height: 400px;
    object-fit: contain;
  }

  .html-preview {
    width: 100%;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: var(--bg2);
    max-height: 400px;
  }
</style>
