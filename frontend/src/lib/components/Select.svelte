<script lang="ts">
  let {
    value = $bindable(),
    options,
  }: {
    value: string | number;
    options: { value: string | number; label: string }[];
  } = $props();

  let open = $state(false);
  let containerEl: HTMLDivElement;

  const selectedLabel = $derived(options.find((o) => o.value === value)?.label ?? "");

  function select(v: string | number) {
    value = v;
    open = false;
  }

  function onOutsideClick(e: MouseEvent) {
    if (!containerEl.contains(e.target as Node)) open = false;
  }

  $effect(() => {
    if (!open) return;
    document.addEventListener("click", onOutsideClick);
    return () => document.removeEventListener("click", onOutsideClick);
  });
</script>

<div class="select" bind:this={containerEl}>
  <button
    type="button"
    class="trigger"
    class:open
    onclick={() => (open = !open)}
    aria-haspopup="listbox"
    aria-expanded={open}
  >
    <span>{selectedLabel}</span>
    <svg class="arrow" width="8" height="5" viewBox="0 0 8 5" aria-hidden="true">
      <path d="M0 0l4 5 4-5z" fill="currentColor" />
    </svg>
  </button>

  {#if open}
    <div class="dropdown" role="listbox">
      {#each options as opt (opt.value)}
        <div
          class="option"
          class:selected={opt.value === value}
          role="option"
          aria-selected={opt.value === value}
          tabindex="-1"
          onclick={() => select(opt.value)}
          onkeydown={(e) => (e.key === "Enter" || e.key === " ") && select(opt.value)}
        >
          {opt.label}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .select {
    position: relative;
  }

  .trigger {
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 3px;
    color: var(--text);
    font-family: "JetBrains Mono", monospace;
    font-size: 11px;
    padding: 4px 8px;
    cursor: pointer;
    white-space: nowrap;
    transition: border-color 0.1s;
    width: 100%;
  }
  .trigger:focus {
    outline: none;
    border-color: var(--green2);
  }
  .trigger.open {
    border-color: var(--green2);
  }

  .arrow {
    color: var(--dim);
    flex-shrink: 0;
  }

  .dropdown {
    position: absolute;
    top: calc(100% + 2px);
    left: 0;
    min-width: 100%;
    background: var(--bg2);
    border: 1px solid var(--border2);
    border-radius: 3px;
    z-index: 100;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
    overflow: hidden;
  }

  .option {
    padding: 5px 10px;
    font-family: "JetBrains Mono", monospace;
    font-size: 11px;
    color: var(--text);
    cursor: pointer;
    white-space: nowrap;
  }
  .option:hover {
    background: var(--bg3);
    color: var(--green);
  }
  .option.selected {
    color: var(--green);
  }
</style>
