<script lang="ts">
  import type { RequestTab, TunneledRequest } from "$lib/types";
  import { fmtMs, fmtTime, methodClass, statusClass } from "$lib/utils";
  import { queryWebSocketMessages } from "$lib/api/websocket.svelte";
  import BodyPreview from "$lib/components/body/BodyPreview.svelte";
  import HeadersTab from "./HeadersTab.svelte";
  import WsMessagesTab from "./WsMessagesTab.svelte";
  import ReplayModal from "$lib/components/modal/ReplayModal.svelte";

  let {
    request,
    activeRequestTab = $bindable(),
    onclose,
    onselectrequest,
  }: {
    request: TunneledRequest;
    activeRequestTab: RequestTab;
    onclose: () => void;
    onselectrequest: (id: string) => void;
  } = $props();

  let showReplayModal = $state(false);

  /** Returns the value of a header case-insensitively. */
  function getHeaderValue(headers: Record<string, string>, name: string): string | undefined {
    const lower = name.toLowerCase();
    return Object.entries(headers).find(([k]) => k.toLowerCase() === lower)?.[1];
  }

  function isWebsocket(req: TunneledRequest): boolean {
    return getHeaderValue(req.requestHeaders, "Upgrade")?.toLowerCase() === "websocket";
  }

  let lastQueriedId: string | null = null;

  $effect(() => {
    if (request && isWebsocket(request) && request.id !== lastQueriedId) {
      lastQueriedId = request.id;
      queryWebSocketMessages(request.id);
    }
  });
</script>

<div class="detail-pane">
  <div class="detail-header">
    <span class="badge {methodClass(request.method)}">{request.method}</span>
    <span class="detail-url">{request.url}</span>
    <span class="status-badge {statusClass(request.status)}">{request.status ?? "—"}</span>
    <button class="replay-btn" aria-label="Replay request" onclick={() => (showReplayModal = true)}
      >↩ REPLAY</button
    >
    <button class="close-btn" aria-label="Close" onclick={onclose}>✕</button>
  </div>

  {#if showReplayModal}
    <ReplayModal
      {request}
      onclose={() => (showReplayModal = false)}
      onselectrequest={(id) => {
        showReplayModal = false;
        onselectrequest(id);
      }}
    />
  {/if}

  <div class="detail-meta">
    <span
      >FROM <b>{getHeaderValue(request.requestHeaders, "Cf-Connecting-Ip") || "Unknown"}</b></span
    >
    <span>{fmtTime(request.timestamp)}</span>
    <span>{fmtMs(request.responseTime)}</span>
    {#if isWebsocket(request)}<span class="ws-badge">WebSocket</span>{/if}
  </div>

  <div class="detail-tabs">
    {#each isWebsocket(request) ? ["headers", "request", "response", "ws"] : ["headers", "request", "response"] as tab (tab)}
      <button
        class="dtab"
        class:active={activeRequestTab === tab}
        onclick={() => (activeRequestTab = tab as RequestTab)}>{tab.toUpperCase()}</button
      >
    {/each}
  </div>

  <div class="detail-body">
    {#if activeRequestTab === "headers"}
      <HeadersTab
        requestHeaders={request.requestHeaders}
        responseHeaders={request.responseHeaders}
      />
    {:else if activeRequestTab === "request"}
      {#if request.requestBody}
        <div class="section-label">REQUEST BODY</div>
        <BodyPreview
          body={request.requestBody}
          mimeType={getHeaderValue(request.requestHeaders, "content-type")}
        />
      {:else}
        <div class="empty-state">No request body.</div>
      {/if}
    {:else if activeRequestTab === "response"}
      {#if request.responseBody}
        <div class="section-label">RESPONSE BODY</div>
        <BodyPreview
          body={request.responseBody}
          mimeType={request.responseHeaders
            ? getHeaderValue(request.responseHeaders, "content-type")
            : undefined}
        />
      {:else}
        <div class="empty-state">No response body.</div>
      {/if}
    {:else if activeRequestTab === "ws"}
      <WsMessagesTab messages={request.wsMessages} />
    {/if}
  </div>
</div>

<style>
  .detail-pane {
    width: 800px;
    min-width: 380px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg1);
  }

  .detail-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    background: var(--bg2);
  }
  .detail-url {
    flex: 1;
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text);
  }
  .replay-btn {
    background: none;
    border: 1px solid var(--border);
    color: var(--dim);
    cursor: pointer;
    font-family: "JetBrains Mono", monospace;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.08em;
    padding: 3px 7px;
    border-radius: 3px;
    white-space: nowrap;
    transition: all 0.1s;
  }
  .replay-btn:hover {
    color: var(--green);
    border-color: var(--green2);
  }

  .close-btn {
    background: none;
    border: none;
    color: var(--dim);
    cursor: pointer;
    font-size: 13px;
    margin-left: 4px;
    padding: 2px 4px;
    border-radius: 2px;
  }
  .close-btn:hover {
    color: var(--red);
  }

  .detail-meta {
    display: flex;
    gap: 14px;
    padding: 6px 14px;
    font-size: 10px;
    color: var(--dim);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    background: var(--bg1);
  }
  .detail-meta :global(b) {
    color: var(--text);
  }

  .detail-tabs {
    display: flex;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    background: var(--bg2);
  }
  .dtab {
    background: none;
    border: none;
    color: var(--dim);
    font-family: "JetBrains Mono", monospace;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.1em;
    padding: 8px 14px;
    cursor: pointer;
    border-bottom: 2px solid transparent;
    transition: all 0.1s;
    margin-bottom: -1px;
  }
  .dtab:hover {
    color: var(--text);
  }
  .dtab.active {
    color: var(--green);
    border-bottom-color: var(--green);
    background: rgba(61, 220, 132, 0.04);
  }

  .detail-body {
    flex: 1;
    overflow-y: auto;
    padding: 12px 14px;
  }

  .section-label {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.14em;
    color: var(--dim);
    margin-bottom: 6px;
    text-transform: uppercase;
  }
</style>
