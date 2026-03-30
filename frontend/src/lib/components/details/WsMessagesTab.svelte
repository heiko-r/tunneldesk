<script lang="ts">
  import type { TunneledRequest } from "$lib/types";
  import { fmtTime } from "$lib/utils";

  let {
    messages,
  }: {
    messages: TunneledRequest["wsMessages"];
  } = $props();
</script>

<div class="section-label">WEBSOCKET MESSAGES</div>
<div class="ws-messages">
  {#each messages as msg (msg.ts)}
    <div class="ws-msg {msg.dir}">
      <span class="ws-dir">{msg.dir === "in" ? "▼ CLIENT" : "▲ SERVER"}</span>
      <span class="ws-ts">{fmtTime(msg.ts)}</span>
      <pre class="ws-data">{msg.data}</pre>
    </div>
  {/each}
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

  .ws-messages {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .ws-msg {
    border-radius: 4px;
    padding: 8px 10px;
    border-left: 3px solid;
  }
  .ws-msg.in {
    background: rgba(74, 159, 224, 0.06);
    border-color: var(--blue);
  }
  .ws-msg.out {
    background: rgba(61, 220, 132, 0.06);
    border-color: var(--green);
  }
  .ws-dir {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.1em;
  }
  .ws-msg.in .ws-dir {
    color: var(--blue);
  }
  .ws-msg.out .ws-dir {
    color: var(--green);
  }
  .ws-ts {
    font-size: 9px;
    color: var(--dim);
    margin-left: 8px;
  }
  .ws-data {
    font-size: 10px;
    color: var(--text);
    margin-top: 4px;
    white-space: pre-wrap;
    word-break: break-all;
  }
</style>
