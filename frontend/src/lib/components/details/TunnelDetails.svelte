<script lang="ts">
  import { setActiveQueryFilter, storage, updateRequests } from "$lib/stores.svelte";
  import { queryRequests } from "$lib/api/websocket.svelte";
  import type { RequestTab, Tunnel, TunneledRequest } from "$lib/types";
  import TunnelHeader from "./TunnelHeader.svelte";
  import FilterBar from "./FilterBar.svelte";
  import RequestTable from "./RequestTable.svelte";
  import RequestDetails from "./RequestDetails.svelte";

  let {
    selectedTunnel = $bindable(),
    selectedRequestId = $bindable(),
  }: {
    selectedTunnel: Tunnel;
    selectedRequestId: string | null;
  } = $props();

  let filterMethod: string | null = $state(null);
  let filterStatus: string | null = $state(null);
  let filterUrl: string | null = $state(null);
  let sortField: "Timestamp" | "ResponseTime" = $state("Timestamp");
  let sortDir: "asc" | "desc" = $state("desc");
  let activeRequestTab: RequestTab = $state("headers");

  // All filtering and sorting is done server-side; the store holds the current query results.
  let tunnelRequests: TunneledRequest[] = $derived(storage.requests.get(selectedTunnel.name) || []);

  let selectedRequest = $derived(tunnelRequests.find((r) => r.id === selectedRequestId) || null);

  // Clear stale requests immediately when switching tunnels.
  $effect(() => {
    updateRequests(selectedTunnel.name, []);
  });

  // Send a debounced query to the server whenever the tunnel or any filter/sort changes.
  // The 300ms delay collapses rapid keystrokes in the URL field into a single query.
  $effect(() => {
    const tunnel = selectedTunnel.name;
    const method = filterMethod || undefined;
    const urlContains = filterUrl || undefined;
    const status = filterStatus ? { Class: Number(filterStatus) } : undefined;
    const sf = sortField; // read synchronously so Svelte tracks it as a dependency
    const sortDirection = (sortDir === "asc" ? "Asc" : "Desc") as "Asc" | "Desc";

    const timer = setTimeout(() => {
      setActiveQueryFilter({ tunnelName: tunnel, method, urlContains, status });
      queryRequests(tunnel, method, status, urlContains, undefined, undefined, sf, sortDirection);
    }, 300);

    return () => clearTimeout(timer);
  });

  function clearRequests() {
    updateRequests(selectedTunnel.name, []);
    // TODO: Clear on server
    selectedRequestId = null;
  }

  function toggleTunnel() {
    selectedTunnel.active = !selectedTunnel.active;
  }

  function selectRequest(id: string) {
    selectedRequestId = id;
    activeRequestTab = "headers";
  }
</script>

<TunnelHeader
  tunnel={selectedTunnel}
  requests={tunnelRequests}
  onclear={clearRequests}
  ontoggle={toggleTunnel}
/>

<div class="pane-split" class:has-detail={!!selectedRequest}>
  <div class="req-pane">
    <FilterBar bind:filterMethod bind:filterStatus bind:filterUrl count={tunnelRequests.length} />
    <RequestTable
      requests={tunnelRequests}
      bind:selectedRequestId
      bind:sortField
      bind:sortDir
      onRowSelect={selectRequest}
    />
  </div>

  {#if selectedRequest}
    <RequestDetails
      request={selectedRequest}
      bind:activeRequestTab
      onclose={() => {
        selectedRequestId = null;
      }}
    />
  {/if}
</div>

<style>
  .pane-split {
    flex: 1;
    display: flex;
    overflow: hidden;
  }

  .req-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 0;
    border-right: 1px solid var(--border);
  }
</style>
