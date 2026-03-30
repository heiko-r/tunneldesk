<script lang="ts">
  import type { Tunnel, TunneledRequest } from "$lib/types";

  let {
    tunnel,
    requests,
    onclear,
    ontoggle,
  }: {
    tunnel: Tunnel;
    requests: TunneledRequest[];
    onclear: () => void;
    ontoggle: () => void;
  } = $props();

  /** Estimates total transferred bytes from base64-encoded bodies. */
  function estimateBytes(reqs: TunneledRequest[]): string {
    const bytes = reqs.reduce((sum, r) => {
      const req = r.requestBody ? Math.floor((r.requestBody.length * 3) / 4) : 0;
      const res = r.responseBody ? Math.floor((r.responseBody.length * 3) / 4) : 0;
      return sum + req + res;
    }, 0);
    if (bytes < 1024) return `${bytes}B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
  }
</script>

<div class="tunnel-header">
  <div class="th-left">
    <span class="indicator lg" class:on={tunnel.active}></span>
    <div>
      <div class="th-title">{tunnel.name}</div>
      <div class="th-sub">
        <span class="dim">{tunnel.active ? "https" : "http"}://</span>{tunnel.domain}
        <span class="dim"> → </span>localhost:{tunnel.localPort}
      </div>
    </div>
  </div>
  <div class="th-stats">
    <div class="stat-chip">
      <span class="sc-label">REQUESTS</span>
      <span class="sc-val">{requests.length}</span>
    </div>
    <div class="stat-chip">
      <span class="sc-label">BYTES</span>
      <span class="sc-val">{estimateBytes(requests)}</span>
    </div>
    <div class="stat-chip">
      <span class="sc-label">STATUS</span>
      <span class="sc-val" class:green={tunnel.active} class:dim={!tunnel.active}>
        {tunnel.active ? "ONLINE" : "OFFLINE"}
      </span>
    </div>
  </div>
  <div class="th-actions">
    <button class="btn-sm" onclick={onclear}>CLEAR LOG</button>
    <button class="btn-sm {tunnel.active ? 'btn-stop' : 'btn-start'}" onclick={ontoggle}>
      {tunnel.active ? "STOP" : "START"}
    </button>
  </div>
</div>

<style>
  .tunnel-header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--bg1);
    flex-shrink: 0;
  }
  .th-left {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 0;
  }
  .th-title {
    font-family: "Syne", sans-serif;
    font-weight: 700;
    font-size: 15px;
    color: var(--text);
  }
  .th-sub {
    font-size: 11px;
    color: var(--dim);
    margin-top: 1px;
  }

  .th-stats {
    display: flex;
    gap: 8px;
  }
  .stat-chip {
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    padding: 4px 10px;
    text-align: center;
  }
  .sc-label {
    display: block;
    font-size: 9px;
    color: var(--dim);
    letter-spacing: 0.1em;
  }
  .sc-val {
    display: block;
    font-size: 13px;
    font-weight: 700;
    color: var(--text);
    margin-top: 1px;
  }
  .sc-val.green {
    color: var(--green);
  }

  .th-actions {
    display: flex;
    gap: 6px;
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
  .indicator.lg {
    width: 10px;
    height: 10px;
  }

  .dim {
    color: var(--dim);
  }
  .green {
    color: var(--green) !important;
  }
</style>
