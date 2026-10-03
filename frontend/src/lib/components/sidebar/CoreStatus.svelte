<script lang="ts">
  import type { CoreStatus } from "$lib/types";
  import Modal from "$lib/components/modal/Modal.svelte";

  let { status, onquit }: { status: CoreStatus; onquit: () => void } = $props();

  let confirming = $state(false);

  let clientsLabel = $derived(
    `${status.attachedClients} ${status.attachedClients === 1 ? "CLIENT" : "CLIENTS"}`,
  );
  let othersWarning = $derived.by(() => {
    const others = status.attachedClients - 1;
    if (others <= 0) return "";
    return others === 1
      ? "1 other client is attached and will be disconnected."
      : `${others} other clients are attached and will be disconnected.`;
  });

  function confirm() {
    confirming = false;
    onquit();
  }
</script>

<div class="core-status" title="Shared core for {status.configPath} (pid {status.pid})">
  <span class="core-indicator" class:stopping={status.shuttingDown}></span>
  <span class="core-label">CORE :{status.port}</span>
  <span class="core-val" data-testid="core-clients">
    {status.shuttingDown ? "STOPPING" : clientsLabel}
  </span>
  <button
    class="btn-quit"
    aria-label="Stop tunnels and quit"
    title="Stop tunnels and quit"
    disabled={status.shuttingDown}
    onclick={() => (confirming = true)}>⏻</button
  >
</div>

<Modal open={confirming} title="STOP TUNNELS?" size="sm" onclose={() => (confirming = false)}>
  <p class="modal-body">
    This stops all tunnels and the Cloudflare connector for this configuration.
  </p>
  {#if othersWarning}
    <p class="modal-body">{othersWarning}</p>
  {/if}
  <div class="modal-actions">
    <button class="btn-sm" onclick={() => (confirming = false)}>CANCEL</button>
    <button class="btn-sm btn-stop" onclick={confirm}>STOP &amp; QUIT</button>
  </div>
</Modal>

<style>
  .core-status {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-top: 1px solid var(--border);
    background: var(--bg);
  }

  .core-indicator {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--green);
    flex-shrink: 0;
  }
  .core-indicator.stopping {
    background: var(--yellow);
  }

  .core-label {
    font-size: 9px;
    color: var(--dim);
    letter-spacing: 0.1em;
    flex: 1;
  }

  .core-val {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--text);
  }

  .btn-quit {
    background: none;
    border: 1px solid var(--border2);
    color: var(--dim);
    width: 20px;
    height: 20px;
    border-radius: 3px;
    cursor: pointer;
    font-size: 11px;
    line-height: 1;
    transition:
      color 0.15s,
      border-color 0.15s;
  }
  .btn-quit:hover:not(:disabled) {
    color: var(--red);
    border-color: var(--red);
  }
  .btn-quit:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .modal-body {
    font-size: 12px;
    color: var(--dim);
    margin-bottom: 4px;
    line-height: 1.5;
  }
  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
</style>
