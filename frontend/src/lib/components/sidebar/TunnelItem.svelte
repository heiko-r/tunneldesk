<script lang="ts">
  import type { Tunnel } from "$lib/types";

  let {
    tunnel,
    selected,
    onclick,
  }: {
    tunnel: Tunnel;
    selected: boolean;
    onclick: () => void;
  } = $props();
</script>

<button class="tunnel-item" class:selected class:inactive={!tunnel.active} {onclick}>
  <div class="tunnel-row1">
    <span class="indicator" class:on={tunnel.active}></span>
    <span class="tunnel-name">{tunnel.name}</span>
  </div>
  <div class="tunnel-row2">
    <span class="tunnel-domain">{tunnel.domain}</span>
  </div>
  <div class="tunnel-row3">
    <span class="tunnel-port">:{tunnel.localPort}</span>
  </div>
</button>

<style>
  .tunnel-item {
    display: block;
    width: 100%;
    text-align: left;
    padding: 8px 10px;
    border-radius: 4px;
    cursor: pointer;
    border: 1px solid transparent;
    margin-bottom: 4px;
    transition: all 0.1s;
    position: relative;
    background: transparent;
    color: inherit;
    font-family: inherit;
    font-size: inherit;
  }
  .tunnel-item:hover {
    background: var(--bg2);
    border-color: var(--border);
  }
  .tunnel-item.selected {
    background: var(--bg3);
    border-color: var(--green2);
  }
  .tunnel-item.inactive {
    opacity: 0.55;
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
