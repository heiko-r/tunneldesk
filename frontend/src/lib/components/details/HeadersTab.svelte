<script lang="ts">
  let {
    requestHeaders,
    responseHeaders,
  }: {
    requestHeaders: { [key: string]: string };
    responseHeaders?: { [key: string]: string };
  } = $props();
</script>

<div class="section-label">REQUEST HEADERS</div>
<div class="headers-table">
  {#each Object.entries(requestHeaders).filter(([, v]) => v) as [k, v] (k)}
    <div class="hrow"><span class="hkey">{k}</span><span class="hval">{v}</span></div>
  {/each}
</div>
<div class="section-label mt">RESPONSE HEADERS</div>
<div class="headers-table">
  {#if responseHeaders}
    {#each Object.entries(responseHeaders) as [k, v] (k)}
      <div class="hrow"><span class="hkey">{k}</span><span class="hval">{v}</span></div>
    {/each}
  {/if}
</div>

<style>
  .section-label {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.14em;
    color: var(--dim);
    margin-bottom: 6px;
    text-transform: uppercase;
  }
  .section-label.mt {
    margin-top: 16px;
  }

  .headers-table {
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .hrow {
    display: flex;
    border-bottom: 1px solid rgba(30, 45, 61, 0.6);
    font-size: 11px;
  }
  .hrow:last-child {
    border-bottom: none;
  }
  .hkey {
    width: 160px;
    min-width: 160px;
    padding: 5px 8px;
    color: var(--dim);
    background: var(--bg2);
    border-right: 1px solid var(--border);
    word-break: break-all;
  }
  .hval {
    flex: 1;
    padding: 5px 8px;
    color: var(--text);
    word-break: break-all;
  }
</style>
