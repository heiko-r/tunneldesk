<script lang="ts">
  import Modal from "./Modal.svelte";
  import type { SyncReport } from "$lib/types";
  import { confirmRemoveHosts } from "$lib/api/websocket.svelte";
  import { lastSyncReport } from "$lib/stores.svelte";

  let { report, onclose }: { report: SyncReport; onclose: () => void } = $props();

  let hostsToRemove = $state<string[]>([]);

  function toggleHost(host: string) {
    if (hostsToRemove.includes(host)) {
      hostsToRemove = hostsToRemove.filter((h) => h !== host);
    } else {
      hostsToRemove = [...hostsToRemove, host];
    }
  }

  function handleConfirmRemove() {
    if (hostsToRemove.length > 0) {
      confirmRemoveHosts(hostsToRemove);
    }
    lastSyncReport.value = null;
    onclose();
  }
</script>

<Modal open={true} title="CLOUDFLARE SYNC" {onclose}>
  {#if report.added.length > 0}
    <div class="section">
      <div class="section-title added">ADDED</div>
      {#each report.added as host (host)}
        <div class="entry">{host}</div>
      {/each}
    </div>
  {/if}

  {#if report.removed.length > 0}
    <div class="section">
      <div class="section-title removed">REMOVED</div>
      {#each report.removed as host (host)}
        <div class="entry">{host}</div>
      {/each}
    </div>
  {/if}

  {#if report.unknownHosts.length > 0}
    <div class="section">
      <div class="section-title unknown">UNKNOWN HOSTS</div>
      <p class="hint">
        These hostnames exist in your Cloudflare tunnel but are not in your local config. Select any
        you want to remove.
      </p>
      {#each report.unknownHosts as host (host)}
        <label class="host-row">
          <input
            type="checkbox"
            checked={hostsToRemove.includes(host)}
            onchange={() => toggleHost(host)}
          />
          <span class="entry">{host}</span>
        </label>
      {/each}
    </div>
  {/if}

  {#if report.errors.length > 0}
    <div class="section">
      <div class="section-title error">ERRORS</div>
      {#each report.errors as err (err)}
        <div class="entry error-text">{err}</div>
      {/each}
    </div>
  {/if}

  <div class="modal-actions">
    <button class="btn-sm" onclick={onclose}>CLOSE</button>
    {#if report.unknownHosts.length > 0}
      <button
        class="btn-sm btn-stop"
        disabled={hostsToRemove.length === 0}
        onclick={handleConfirmRemove}
      >
        REMOVE SELECTED ({hostsToRemove.length})
      </button>
    {/if}
  </div>
</Modal>

<style>
  .section {
    margin-bottom: 14px;
  }

  .section-title {
    font-size: 9px;
    letter-spacing: 0.12em;
    font-weight: 700;
    margin-bottom: 6px;
  }
  .section-title.added {
    color: var(--green);
  }
  .section-title.removed {
    color: var(--yellow);
  }
  .section-title.unknown {
    color: var(--dim);
  }
  .section-title.error {
    color: var(--red);
  }

  .hint {
    font-size: 11px;
    color: var(--dim);
    margin-bottom: 8px;
    line-height: 1.5;
  }

  .entry {
    font-size: 12px;
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    padding: 3px 0;
  }

  .error-text {
    color: var(--red);
  }

  .host-row {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    padding: 3px 0;
  }
  .host-row input[type="checkbox"] {
    accent-color: var(--green);
    cursor: pointer;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
</style>
