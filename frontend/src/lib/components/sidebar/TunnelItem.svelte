<script lang="ts">
  import type { Tunnel } from "$lib/types";

  let {
    tunnel,
    selected,
    onclick,
    onedit,
    ondelete,
  }: {
    tunnel: Tunnel;
    selected: boolean;
    onclick: () => void;
    onedit: () => void;
    ondelete: () => void;
  } = $props();
</script>

<div class="tunnel-item-wrap" class:selected>
  <button class="tunnel-item" class:inactive={!tunnel.enabled} {onclick}>
    <div class="tunnel-row1">
      <span class="indicator" class:on={tunnel.enabled}></span>
      <span class="tunnel-name">{tunnel.name}</span>
      {#if !tunnel.enabled}
        <span class="badge-disabled">DISABLED</span>
      {/if}
    </div>
    <div class="tunnel-row2">
      <span class="tunnel-domain">{tunnel.domain}</span>
    </div>
    <div class="tunnel-row3">
      <span class="tunnel-port">:{tunnel.localPort}</span>
    </div>
  </button>
  <div class="tunnel-actions">
    <button
      class="act-btn"
      title="Edit tunnel"
      aria-label="Edit {tunnel.name}"
      onclick={(e) => {
        e.stopPropagation();
        onedit();
      }}>✎</button
    >
    <button
      class="act-btn act-del"
      title="Delete tunnel"
      aria-label="Delete {tunnel.name}"
      onclick={(e) => {
        e.stopPropagation();
        ondelete();
      }}>✕</button
    >
  </div>
</div>

<style>
  .tunnel-item-wrap {
    display: flex;
    align-items: stretch;
    border-radius: 4px;
    border: 1px solid transparent;
    margin-bottom: 4px;
    transition: all 0.1s;
  }
  .tunnel-item-wrap:hover {
    background: var(--bg2);
    border-color: var(--border);
  }
  .tunnel-item-wrap.selected {
    background: var(--bg3);
    border-color: var(--green2);
  }

  .tunnel-item {
    flex: 1;
    text-align: left;
    padding: 8px 10px;
    cursor: pointer;
    border: none;
    background: transparent;
    color: inherit;
    font-family: inherit;
    font-size: inherit;
    min-width: 0;
  }
  .tunnel-item.inactive {
    opacity: 0.55;
  }

  .tunnel-actions {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 2px;
    padding: 4px 4px 4px 0;
    opacity: 0;
    transition: opacity 0.1s;
  }
  .tunnel-item-wrap:hover .tunnel-actions,
  .tunnel-item-wrap.selected .tunnel-actions {
    opacity: 1;
  }

  .act-btn {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--dim);
    width: 18px;
    height: 18px;
    border-radius: 2px;
    cursor: pointer;
    font-size: 11px;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all 0.1s;
    padding: 0;
  }
  .act-btn:hover {
    background: var(--bg3);
    color: var(--text);
    border-color: var(--border2);
  }
  .act-del:hover {
    background: var(--red);
    color: var(--bg);
    border-color: var(--red);
  }

  .tunnel-row1,
  .tunnel-row2,
  .tunnel-row3 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 2px;
  }
  .tunnel-name {
    font-weight: 700;
    color: var(--text);
    font-size: 12px;
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tunnel-domain {
    font-size: 10px;
    color: var(--dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tunnel-port {
    font-size: 10px;
    color: var(--yellow);
  }

  .badge-disabled {
    font-size: 8px;
    letter-spacing: 0.08em;
    color: var(--dim);
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 2px;
    padding: 1px 4px;
    flex-shrink: 0;
  }

  /* ── Indicator dot ── */
  .indicator {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--dim);
    flex-shrink: 0;
    transition: all 0.3s;
  }
  .indicator.on {
    background: var(--green);
    box-shadow: 0 0 6px var(--green);
  }
</style>
