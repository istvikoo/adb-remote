<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let {
    serial,
    ready,
    favorites,
    onToggleFavorite,
  }: {
    serial: string | null;
    ready: boolean;
    favorites: string[];
    onToggleFavorite: (pkg: string) => void;
  } = $props();

  let apps = $state<string[]>([]);
  let loading = $state(false);
  let showAll = $state(false);
  let filter = $state("");
  let message = $state("");
  let busy = $state<string | null>(null);
  let loadRun = 0;

  const visible = $derived.by(() => {
    const f = filter.trim().toLowerCase();
    const list = f ? apps.filter((a) => a.toLowerCase().includes(f)) : apps;
    // Favorites first, then alphabetical (the backend already sorts).
    return [...list.filter((a) => favorites.includes(a)), ...list.filter((a) => !favorites.includes(a))];
  });

  async function load(target: string, all: boolean) {
    const run = ++loadRun;
    loading = true;
    message = "";
    try {
      const list = await invoke<string[]>("list_apps", { serial: target, all });
      if (run === loadRun) apps = list;
    } catch (e) {
      if (run === loadRun) {
        apps = [];
        message = String(e);
      }
    } finally {
      if (run === loadRun) loading = false;
    }
  }

  $effect(() => {
    if (ready && serial) {
      load(serial, showAll);
    } else {
      loadRun++;
      apps = [];
      loading = false;
    }
  });

  async function act(cmd: "launch_app" | "stop_app", pkg: string) {
    if (!serial || busy) return;
    busy = pkg;
    message = "";
    try {
      await invoke(cmd, { serial, package: pkg });
    } catch (e) {
      message = String(e);
    } finally {
      busy = null;
    }
  }
</script>

<section class="apps">
  <div class="bar">
    <input bind:value={filter} placeholder="Filter" autocomplete="off" spellcheck="false" />
    <button class="plain" disabled={!ready || loading} onclick={() => serial && load(serial, showAll)}>
      {loading ? "…" : "Refresh"}
    </button>
  </div>
  <label class="check">
    <input type="checkbox" bind:checked={showAll} disabled={!ready} />
    Show system apps
  </label>

  {#if message}
    <button class="error" onclick={() => (message = "")} title="Dismiss">{message}</button>
  {/if}

  <div class="list">
    {#if !ready}
      <p class="hint">Connect a device to see its apps.</p>
    {:else if loading && !apps.length}
      <p class="hint">Loading…</p>
    {:else if !visible.length}
      <p class="hint">{apps.length ? "Nothing matches the filter." : "No apps found."}</p>
    {/if}
    {#each visible as pkg (pkg)}
      {@const fav = favorites.includes(pkg)}
      <div class="row">
        <button
          class="star"
          class:on={fav}
          title={fav ? "Remove from the remote" : "Add a button to the remote"}
          onclick={() => onToggleFavorite(pkg)}
        >{fav ? "★" : "☆"}</button>
        <span class="name" title={pkg}>{pkg}</span>
        <button class="plain" disabled={busy !== null} onclick={() => act("launch_app", pkg)}>Open</button>
        <button class="plain" disabled={busy !== null} title="Force stop" onclick={() => act("stop_app", pkg)}>Stop</button>
      </div>
    {/each}
  </div>
</section>

<style>
  .apps {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 1;
    min-height: 0;
  }
  .bar {
    display: flex;
    gap: 8px;
  }
  input:not([type]) {
    flex: 1;
    min-width: 0;
    font: inherit;
    color: inherit;
    background: #1b1d22;
    border: 1px solid #2c2f37;
    border-radius: 10px;
    padding: 9px 12px;
    outline: none;
  }
  input:not([type]):focus {
    border-color: #5b8cff;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    opacity: 0.8;
  }
  button {
    font: inherit;
    color: inherit;
    border: 0;
    background: none;
    cursor: pointer;
  }
  button:disabled {
    cursor: default;
    opacity: 0.4;
  }
  .plain {
    background: #23262d;
    border-radius: 9px;
    padding: 6px 10px;
    font-size: 13px;
    font-weight: 600;
    flex: none;
  }
  .plain:not(:disabled):hover {
    background: #2b2f38;
  }
  .list {
    flex: 1;
    min-height: 200px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    background: #1b1d22;
    border-radius: 10px;
    padding: 5px 6px;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
  }
  .star {
    font-size: 16px;
    opacity: 0.5;
    padding: 0 4px;
    flex: none;
  }
  .star.on {
    opacity: 1;
    color: #ffd166;
  }
  .hint {
    margin: 8px 2px;
    opacity: 0.55;
    font-size: 13px;
  }
  .error {
    background: #3a1c1f;
    border: 1px solid #6b2b31;
    color: #ffb4b9;
    border-radius: 10px;
    padding: 8px 12px;
    text-align: left;
  }
</style>
