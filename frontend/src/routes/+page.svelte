<script lang="ts">
  import Sidebar from "$lib/components/sidebar/Sidebar.svelte";
  import TunnelDetails from "$lib/components/details/TunnelDetails.svelte";
  import Modal from "$lib/components/modal/Modal.svelte";
  import type { Tunnel } from "$lib/types";
  import { storage, updateTunnels } from "$lib/stores.svelte";

  // ─── UI State ─────────────────────────────────────────────────────────────────
  let selectedTunnelName = $state("t1");
  let selectedRequestId: string | null = $state(null);

  // Modals
  let showAddModal = $state(false);
  let showEditModal = $state(false);
  let editTunnel: Tunnel | null = $state(null);
  let newTunnel = $state({ name: "", domain: "", localPort: "", protocol: "https" });
  let confirmDelete: string | null = $state(null);

  // ─── Derived ──────────────────────────────────────────────────────────────────
  let selectedTunnel = $derived(storage.tunnels.find((t) => t.name === selectedTunnelName));

  function saveEdit() {
    if (editTunnel) {
      updateTunnels(storage.tunnels.map((t) => (t.name === editTunnel!.name ? editTunnel! : t)));
    }
    showEditModal = false;
    editTunnel = null;
  }
</script>

<!-- ═══════════════════════════════════════════════════════════════ TEMPLATE -->
<div class="app">
  <Sidebar
    tunnels={storage.tunnels}
    bind:showAddModal
    bind:selectedTunnelName
    bind:selectedRequestId
  />

  <main class="main">
    {#if selectedTunnel}
      <TunnelDetails bind:selectedTunnel bind:selectedRequestId />
    {:else}
      <div class="empty-main">No tunnel selected.</div>
    {/if}
  </main>
</div>

<!-- ── Add Modal ──────────────────────────────────────────────────────────── -->
<Modal open={showAddModal} title="NEW TUNNEL" onclose={() => (showAddModal = false)}>
  <label class="mlabel"
    >NAME<input class="minput" bind:value={newTunnel.name} placeholder="my-api" /></label
  >
  <label class="mlabel"
    >DOMAIN<input
      class="minput"
      bind:value={newTunnel.domain}
      placeholder="myapi.tunnel.sh"
    /></label
  >
  <label class="mlabel"
    >LOCAL PORT<input
      class="minput"
      type="number"
      bind:value={newTunnel.localPort}
      placeholder="3000"
    /></label
  >
  <label class="mlabel"
    >PROTOCOL
    <select class="minput" bind:value={newTunnel.protocol}>
      <option>https</option><option>http</option><option>wss</option>
    </select>
  </label>
  <div class="modal-actions">
    <button class="btn-sm" onclick={() => (showAddModal = false)}>CANCEL</button>
    <button
      class="btn-sm btn-start"
      onclick={() => {
        /* TODO: implement addTunnel */
      }}>CREATE</button
    >
  </div>
</Modal>

<!-- ── Edit Modal ─────────────────────────────────────────────────────────── -->
{#if editTunnel}
  <Modal open={showEditModal} title="EDIT TUNNEL" onclose={() => (showEditModal = false)}>
    <label class="mlabel">NAME<input class="minput" bind:value={editTunnel.name} /></label>
    <label class="mlabel">DOMAIN<input class="minput" bind:value={editTunnel.domain} /></label>
    <label class="mlabel"
      >LOCAL PORT<input class="minput" type="number" bind:value={editTunnel.localPort} /></label
    >
    <label class="mlabel"
      >PROTOCOL
      <select class="minput" bind:value={(editTunnel as Tunnel & { protocol?: string }).protocol}>
        <option>https</option><option>http</option><option>wss</option>
      </select>
    </label>
    <div class="modal-actions">
      <button class="btn-sm" onclick={() => (showEditModal = false)}>CANCEL</button>
      <button class="btn-sm btn-start" onclick={saveEdit}>SAVE</button>
    </div>
  </Modal>
{/if}

<!-- ── Confirm Delete ─────────────────────────────────────────────────────── -->
<Modal
  open={!!confirmDelete}
  title="DELETE TUNNEL?"
  size="sm"
  onclose={() => (confirmDelete = null)}
>
  <p class="modal-body">This will remove the tunnel and all recorded requests.</p>
  <div class="modal-actions">
    <button class="btn-sm" onclick={() => (confirmDelete = null)}>CANCEL</button>
    <button
      class="btn-sm btn-stop"
      onclick={() => {
        /* TODO: implement deleteTunnel */ confirmDelete = null;
      }}>DELETE</button
    >
  </div>
</Modal>

<!-- ═══════════════════════════════════════════════════════════════ STYLES -->
<style>
  /* ── Layout ── */
  .app {
    display: flex;
    height: 100vh;
    overflow: hidden;
    background: var(--bg);
  }
  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .empty-main {
    color: var(--dim);
    font-size: 12px;
    margin: auto;
  }

  /* ── Modal form fields ── */
  .mlabel {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 9px;
    color: var(--dim);
    letter-spacing: 0.1em;
    margin-bottom: 10px;
  }
  .minput {
    background: var(--bg2);
    border: 1px solid var(--border);
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 12px;
    padding: 7px 10px;
    border-radius: 3px;
    outline: none;
    width: 100%;
    transition: border-color 0.1s;
  }
  .minput:focus {
    border-color: var(--green2);
  }
  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .modal-body {
    font-size: 12px;
    color: var(--dim);
    margin-bottom: 4px;
    line-height: 1.5;
  }
</style>
