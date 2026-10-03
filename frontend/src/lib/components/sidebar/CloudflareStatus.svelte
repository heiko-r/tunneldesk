<script lang="ts">
  import type { CloudflareStatus, ConnectorState } from "$lib/types";

  let { status }: { status: CloudflareStatus } = $props();

  const CONNECTOR_DISPLAY: Record<
    ConnectorState,
    { label: string; tone: "green" | "yellow" | "blue" | "red" | "dim"; hint: string }
  > = {
    Connected: { label: "CONNECTED", tone: "green", hint: "cloudflared is connected" },
    Starting: { label: "STARTING", tone: "yellow", hint: "cloudflared is connecting" },
    External: {
      label: "EXTERNAL",
      tone: "blue",
      hint: "cloudflared is managed outside TunnelDesk (manage_cloudflared = false)",
    },
    NotInstalled: {
      label: "NOT INSTALLED",
      tone: "red",
      hint: "cloudflared was not found on PATH",
    },
    Stopped: { label: "STOPPED", tone: "dim", hint: "cloudflared is not running" },
  };

  let display = $derived(CONNECTOR_DISPLAY[status.connector] ?? CONNECTOR_DISPLAY.Stopped);
</script>

<div class="cf-status">
  <div class="cf-row" title={display.hint}>
    <span class="cf-indicator {display.tone}" data-testid="cf-indicator"></span>
    <span class="cf-label">CLOUDFLARE</span>
    <span class="cf-val {display.tone}">{display.label}</span>
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
  .cf-indicator.green {
    background: var(--green);
    box-shadow: 0 0 5px var(--green);
  }
  .cf-indicator.yellow {
    background: var(--yellow);
  }
  .cf-indicator.blue {
    background: var(--blue);
  }
  .cf-indicator.red {
    background: var(--red);
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
  .cf-val.yellow {
    color: var(--yellow);
  }
  .cf-val.blue {
    color: var(--blue);
  }
  .cf-val.red {
    color: var(--red);
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
