<script lang="ts">
  import type { CloudflareStatus } from "$lib/types";

  let { status }: { status: CloudflareStatus } = $props();
</script>

<div class="cf-status">
  <div class="cf-row">
    <span class="cf-indicator" class:on={status.serviceRunning}></span>
    <span class="cf-label">CLOUDFLARE</span>
    <span class="cf-val" class:green={status.serviceRunning} class:dim={!status.serviceRunning}>
      {status.serviceRunning ? "RUNNING" : "STOPPED"}
    </span>
  </div>
  {#if status.tunnelName}
    <div class="cf-tunnel-name">{status.tunnelName}</div>
  {/if}
  {#if status.tunnelId}
    <div class="cf-tunnel-id">{status.tunnelId.slice(0, 8)}…</div>
  {/if}
</div>

<style>
  .cf-status {
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }

  .cf-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .cf-indicator {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--dim);
    flex-shrink: 0;
  }
  .cf-indicator.on {
    background: var(--green);
    box-shadow: 0 0 5px var(--green);
  }

  .cf-label {
    font-size: 9px;
    color: var(--dim);
    letter-spacing: 0.1em;
    flex: 1;
  }

  .cf-val {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.08em;
  }
  .cf-val.green {
    color: var(--green);
  }
  .cf-val.dim {
    color: var(--dim);
  }

  .cf-tunnel-name {
    font-size: 10px;
    color: var(--text);
    margin-top: 3px;
    padding-left: 12px;
  }

  .cf-tunnel-id {
    font-size: 9px;
    color: var(--dim);
    font-family: "JetBrains Mono", monospace;
    padding-left: 12px;
    margin-top: 1px;
  }
</style>
