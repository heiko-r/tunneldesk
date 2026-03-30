<script lang="ts">
  import type { Tunnel } from "$lib/types";
  import { queryRequests } from "$lib/api/websocket.svelte";
  import TunnelItem from "./TunnelItem.svelte";

  let {
    tunnels,
    showAddModal = $bindable(),
    selectedTunnelName = $bindable(),
    selectedRequestId = $bindable(),
  }: {
    tunnels: Tunnel[];
    showAddModal: boolean;
    selectedTunnelName: string | null;
    selectedRequestId: string | null;
  } = $props();

  function selectTunnel(name: string) {
    selectedTunnelName = name;
    selectedRequestId = null;
    queryRequests(name);
  }
</script>

<aside class="sidebar">
  <div class="sidebar-header">
    <span class="logo">⬡ TUNNELDESK</span>
    <button
      class="btn-icon"
      title="New tunnel"
      aria-label="New tunnel"
      onclick={() => (showAddModal = true)}>+</button
    >
  </div>

  <div class="tunnel-list">
    {#each tunnels as t (t.name)}
      <TunnelItem
        tunnel={t}
        selected={selectedTunnelName === t.name}
        onclick={() => selectTunnel(t.name)}
      />
    {/each}
  </div>
</aside>

<style>
  .sidebar {
    width: 260px;
    min-width: 260px;
    background: var(--bg1);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .sidebar-header {
    padding: 14px 12px 12px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    border-bottom: 1px solid var(--border);
  }

  .logo {
    font-family: "Syne", sans-serif;
    font-weight: 800;
    font-size: 13px;
    color: var(--green);
    letter-spacing: 0.12em;
  }

  .btn-icon {
    background: var(--bg3);
    border: 1px solid var(--border2);
    color: var(--green);
    width: 22px;
    height: 22px;
    border-radius: 3px;
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.15s;
  }
  .btn-icon:hover {
    background: var(--green);
    color: var(--bg);
  }

  .tunnel-list {
    flex: 1;
    overflow-y: auto;
    padding: 6px;
  }
</style>
