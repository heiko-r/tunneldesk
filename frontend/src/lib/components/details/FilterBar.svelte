<script lang="ts">
  const HTTP_METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"] as const;

  let {
    filterMethod = $bindable(),
    filterStatus = $bindable(),
    filterUrl = $bindable(),
    count,
  }: {
    filterMethod: string | null;
    filterStatus: string | null;
    filterUrl: string | null;
    count: number;
  } = $props();
</script>

<div class="filters">
  <select bind:value={filterMethod} class="filter-select">
    <option value="">ALL METHODS</option>
    {#each HTTP_METHODS as m (m)}
      <option value={m}>{m}</option>
    {/each}
  </select>
  <select bind:value={filterStatus} class="filter-select">
    <option value="">ALL STATUS</option>
    {#each [2, 3, 4, 5] as s (s)}
      <option value={s}>{s}xx</option>
    {/each}
  </select>
  <input class="filter-input" placeholder="Filter URL…" bind:value={filterUrl} />
  <span class="req-count">{count}</span>
</div>

<style>
  .filters {
    display: flex;
    gap: 8px;
    padding: 8px 12px;
    background: var(--bg1);
    border-bottom: 1px solid var(--border);
    align-items: center;
    flex-shrink: 0;
  }

  .filter-select,
  .filter-input {
    background: var(--bg2);
    border: 1px solid var(--border);
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 11px;
    padding: 4px 8px;
    border-radius: 3px;
    outline: none;
    transition: border-color 0.1s;
  }
  .filter-select:focus,
  .filter-input:focus {
    border-color: var(--green2);
  }
  .filter-input {
    flex: 1;
  }
  .req-count {
    color: var(--dim);
    font-size: 10px;
    white-space: nowrap;
  }
</style>
