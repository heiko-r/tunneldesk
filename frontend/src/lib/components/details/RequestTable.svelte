<script lang="ts">
  import type { TunneledRequest } from "$lib/types";
  import { fmtMs, fmtTime, methodClass, statusClass } from "$lib/utils";

  let {
    requests,
    selectedRequestId = $bindable(),
    sortField = $bindable(),
    sortDir = $bindable(),
    onRowSelect,
  }: {
    requests: TunneledRequest[];
    selectedRequestId: string | null;
    sortField: "Timestamp" | "ResponseTime";
    sortDir: "asc" | "desc";
    onRowSelect: (id: string) => void;
  } = $props();

  function toggleSort(field: "Timestamp" | "ResponseTime") {
    if (sortField === field) sortDir = sortDir === "asc" ? "desc" : "asc";
    else {
      sortField = field;
      sortDir = "desc";
    }
  }

  function sortIndicator(field: "Timestamp" | "ResponseTime"): string {
    if (sortField !== field) return "";
    return sortDir === "asc" ? "↑" : "↓";
  }
</script>

<div class="req-table-head">
  <span class="col-method">METHOD</span>
  <span class="col-url">URL</span>
  <span class="col-status">STATUS</span>
  <button class="col-time sortable" onclick={() => toggleSort("ResponseTime")}>
    TIME {sortIndicator("ResponseTime")}
  </button>
  <button class="col-ts sortable" onclick={() => toggleSort("Timestamp")}>
    WHEN {sortIndicator("Timestamp")}
  </button>
</div>

<div class="req-list">
  {#if requests.length === 0}
    <div class="empty-state">No requests yet. Traffic will appear here.</div>
  {/if}
  {#each requests as req (req.id)}
    <div
      class="req-row"
      class:selected={selectedRequestId === req.id}
      role="row"
      tabindex="0"
      onclick={() => onRowSelect(req.id)}
      onkeydown={(e) => e.key === "Enter" && onRowSelect(req.id)}
    >
      <span class="col-method"
        ><span class="badge {methodClass(req.method)}">{req.method}</span></span
      >
      <span class="col-url url-cell">
        {#if req.isWebSocket}<span class="ws-badge">WS</span>{/if}
        {#if req.replayed}<span class="replay-badge">↩</span>{/if}
        {req.url}
      </span>
      <span class="col-status"
        ><span class="status-badge {statusClass(req.status)}">{req.status ?? "—"}</span></span
      >
      <span class="col-time">{fmtMs(req.responseTime)}</span>
      <span class="col-ts">{fmtTime(req.timestamp)}</span>
    </div>
  {/each}
</div>

<style>
  .req-table-head {
    display: grid;
    grid-template-columns: 80px 1fr 70px 70px 80px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg2);
    color: var(--dim);
    font-size: 10px;
    letter-spacing: 0.1em;
    flex-shrink: 0;
  }

  .req-list {
    flex: 1;
    overflow-y: auto;
  }

  .req-row {
    display: grid;
    grid-template-columns: 80px 1fr 70px 70px 80px;
    padding: 7px 12px;
    border-bottom: 1px solid rgba(30, 45, 61, 0.6);
    cursor: pointer;
    transition: background 0.08s;
    align-items: center;
  }
  .req-row:hover {
    background: var(--bg2);
  }
  .req-row.selected {
    background: var(--bg3);
  }

  .sortable {
    background: none;
    border: none;
    color: inherit;
    font-family: inherit;
    font-size: inherit;
    letter-spacing: inherit;
    text-align: left;
    padding: 0;
    cursor: pointer;
    user-select: none;
  }
  .sortable:hover {
    color: var(--text);
  }

  .col-url {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
  }
  .col-time {
    color: var(--dim);
  }
  .col-ts {
    color: var(--dim);
  }
  .url-cell {
    display: flex;
    align-items: center;
    gap: 5px;
  }

  .replay-badge {
    font-size: 10px;
    color: var(--blue);
    flex-shrink: 0;
  }
</style>
