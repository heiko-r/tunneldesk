<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    open,
    title,
    size,
    onclose,
    children,
  }: {
    open: boolean;
    title: string;
    size?: "sm" | "lg";
    onclose: () => void;
    children?: Snippet;
  } = $props();
</script>

{#if open}
  <div
    class="overlay"
    role="none"
    onclick={(e) => {
      if (e.target === e.currentTarget) onclose();
    }}
  >
    <div
      class="modal"
      class:modal-sm={size === "sm"}
      class:modal-lg={size === "lg"}
      role="dialog"
      aria-modal="true"
      aria-labelledby="modal-title"
      tabindex="-1"
    >
      <div class="modal-title" id="modal-title">{title}</div>
      {@render children?.()}
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.65);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    backdrop-filter: blur(2px);
  }
  .modal {
    background: var(--bg1);
    border: 1px solid var(--border2);
    border-radius: 6px;
    padding: 20px 22px;
    width: 340px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.7);
  }
  .modal-sm {
    width: 280px;
  }
  .modal-lg {
    width: 680px;
    max-height: 90vh;
    overflow-y: auto;
  }
  .modal-title {
    font-family: "Syne", sans-serif;
    font-weight: 800;
    font-size: 14px;
    color: var(--green);
    letter-spacing: 0.1em;
    margin-bottom: 16px;
  }
</style>
