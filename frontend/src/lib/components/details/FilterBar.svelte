<script lang="ts">
  import Select from "$lib/components/Select.svelte";

  const METHOD_OPTIONS = [
    { value: "", label: "ALL METHODS" },
    ...["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"].map((m) => ({
      value: m,
      label: m,
    })),
  ];

  const STATUS_OPTIONS = [
    { value: "", label: "ALL STATUS" },
    ...[2, 3, 4, 5].map((s) => ({ value: s, label: `${s}xx` })),
  ];

  let {
    filterMethod = $bindable(),
    filterStatus = $bindable(),
    filterUrl = $bindable(),
    count,
  }: {
    filterMethod: string;
    filterStatus: string;
    filterUrl: string | null;
    count: number;
  } = $props();
</script>

<div class="filters">
  <Select bind:value={filterMethod} options={METHOD_OPTIONS} />
  <Select bind:value={filterStatus} options={STATUS_OPTIONS} />
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
    flex: 1;
  }
  .filter-input:focus {
    border-color: var(--green2);
  }
  .req-count {
    color: var(--dim);
    font-size: 10px;
    white-space: nowrap;
  }
</style>
