<script lang="ts">
  import { untrack } from "svelte";
  import Modal from "./Modal.svelte";
  import type { TunneledRequest } from "$lib/types";
  import { sendReplay } from "$lib/api/websocket.svelte";
  import Select from "$lib/components/Select.svelte";
  import { lastReplayedId } from "$lib/stores.svelte";
  import { storage } from "$lib/stores.svelte";
  import { bytesToUtf, decodeBase64, encodeBase64 } from "$lib/utils";

  const HTTP_METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"];

  const METHOD_OPTIONS = [
    ...HTTP_METHODS.map((m) => ({
      value: m,
      label: m,
    })),
  ];

  let {
    request,
    onclose,
    onselectrequest,
  }: {
    request: TunneledRequest;
    onclose: () => void;
    onselectrequest: (id: string) => void;
  } = $props();

  // ── Form state (initialized once from props, then independently editable) ─

  let method = $state(untrack(() => request.method as string));
  let url = $state(untrack(() => request.url));
  let headers = $state<{ key: string; value: string }[]>(
    untrack(() => Object.entries(request.requestHeaders).map(([key, value]) => ({ key, value }))),
  );
  let bodyText = $state(
    untrack(() => (request.requestBody ? bytesToUtf(decodeBase64(request.requestBody)) : "")),
  );

  // ── Send state ────────────────────────────────────────────────────────────

  let sending = $state(false);
  let replayedId = $state<string | null>(null);
  let replayError = $state<string | null>(null);

  $effect(() => {
    const id = lastReplayedId.value;
    const err = lastReplayedId.error;
    if ((id !== null || err !== null) && sending) {
      replayedId = id;
      replayError = err;
      sending = false;
    }
  });

  // ── Derived ───────────────────────────────────────────────────────────────

  const tunnel = $derived(storage.tunnels.find((t) => t.name === request.tunnelName));
  const baseUrl = $derived(tunnel ? `http://localhost:${tunnel.localPort}` : "");

  // ── Actions ───────────────────────────────────────────────────────────────

  function addHeader() {
    headers = [...headers, { key: "", value: "" }];
  }

  function removeHeader(i: number) {
    headers = headers.filter((_, idx) => idx !== i);
  }

  function handleSend() {
    sending = true;
    replayedId = null;
    replayError = null;
    lastReplayedId.value = null;
    lastReplayedId.error = null;

    const headerMap: Record<string, string> = {};
    for (const { key, value } of headers) {
      if (key.trim()) headerMap[key.trim()] = value;
    }

    sendReplay(request.tunnelName, method, url, headerMap, bodyText ? encodeBase64(bodyText) : "");
  }

  function handleViewReplayed() {
    if (replayedId) {
      onselectrequest(replayedId);
      handleClose();
    }
  }

  function handleClose() {
    lastReplayedId.value = null;
    lastReplayedId.error = null;
    onclose();
  }
</script>

<Modal open={true} title="REPLAY REQUEST" size="lg" onclose={handleClose}>
  <!-- Request editor -->
  <div class="section-label">REQUEST</div>

  <div class="url-row">
    <div class="method-select">
      <Select bind:value={method} options={METHOD_OPTIONS} />
    </div>
    <div class="url-field">
      <span class="base-url">{baseUrl}</span>
      <input class="url-input" type="text" bind:value={url} placeholder="/path?query=value" />
    </div>
  </div>

  <div class="subsection-label">HEADERS</div>
  <div class="headers-list">
    {#each headers as header, i (i)}
      <div class="header-row">
        <input class="header-key" type="text" bind:value={header.key} placeholder="Header name" />
        <input
          class="header-value"
          type="text"
          bind:value={header.value}
          placeholder="Header value"
        />
        <button class="remove-btn" aria-label="Remove header" onclick={() => removeHeader(i)}
          >✕</button
        >
      </div>
    {/each}
    <button class="btn-sm add-header-btn" onclick={addHeader}>+ ADD HEADER</button>
  </div>

  <div class="subsection-label">BODY</div>
  <textarea class="body-textarea" bind:value={bodyText} placeholder="Request body (optional)"
  ></textarea>

  <div class="modal-actions">
    <button class="btn-sm" onclick={handleClose}>CANCEL</button>

    {#if replayedId}
      <button class="btn-sm btn-view" onclick={handleViewReplayed}>VIEW REPLAYED REQUEST ↗</button>
    {/if}

    <button class="btn-sm btn-start send-btn" onclick={handleSend} disabled={sending}>
      {sending ? "SENDING…" : "SEND"}
    </button>
  </div>

  {#if replayError}
    <div class="error-msg">{replayError}</div>
  {/if}
</Modal>

<style>
  .section-label {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.14em;
    color: var(--dim);
    margin-bottom: 8px;
    text-transform: uppercase;
  }

  .subsection-label {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.12em;
    color: var(--dim);
    margin-top: 12px;
    margin-bottom: 6px;
    text-transform: uppercase;
  }

  .url-row {
    display: flex;
    align-items: stretch;
    gap: 6px;
    margin-bottom: 2px;
  }

  .method-select {
    flex-shrink: 0;
  }

  .url-field {
    flex: 1;
    display: flex;
    align-items: center;
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .url-field:focus-within {
    border-color: var(--green2);
  }

  .base-url {
    font-size: 10px;
    color: var(--dim);
    padding: 4px 6px;
    white-space: nowrap;
    border-right: 1px solid var(--border);
    flex-shrink: 0;
  }

  .url-input {
    flex: 1;
    background: transparent;
    border: none;
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 11px;
    padding: 4px 8px;
    outline: none;
  }
  .url-input::placeholder {
    color: var(--dim);
  }

  .headers-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .header-row {
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .header-key {
    flex: 0 0 38%;
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 11px;
    padding: 4px 7px;
    outline: none;
  }
  .header-key:focus {
    border-color: var(--green2);
  }
  .header-key::placeholder {
    color: var(--dim);
  }

  .header-value {
    flex: 1;
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 11px;
    padding: 4px 7px;
    outline: none;
  }
  .header-value:focus {
    border-color: var(--green2);
  }
  .header-value::placeholder {
    color: var(--dim);
  }

  .remove-btn {
    background: none;
    border: none;
    color: var(--dim);
    cursor: pointer;
    font-size: 12px;
    padding: 2px 4px;
    border-radius: 2px;
    flex-shrink: 0;
  }
  .remove-btn:hover {
    color: var(--red);
  }

  .add-header-btn {
    align-self: flex-start;
    margin-top: 2px;
    font-size: 9px;
    letter-spacing: 0.1em;
  }

  .body-textarea {
    width: 100%;
    min-height: 80px;
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 11px;
    padding: 7px;
    outline: none;
    resize: vertical;
  }
  .body-textarea:focus {
    border-color: var(--green2);
  }
  .body-textarea::placeholder {
    color: var(--dim);
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }

  .send-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn-view {
    color: var(--blue);
    border-color: var(--blue);
  }
  .btn-view:hover {
    background: var(--blue);
    color: var(--bg);
  }

  .error-msg {
    margin-top: 10px;
    font-size: 11px;
    color: var(--red);
    background: rgba(224, 85, 85, 0.08);
    border: 1px solid rgba(224, 85, 85, 0.3);
    border-radius: 3px;
    padding: 8px 10px;
  }
</style>
