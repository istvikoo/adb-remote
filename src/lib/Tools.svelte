<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let {
    serial,
    ready,
    onKey,
  }: {
    serial: string | null;
    ready: boolean;
    /** Sends a single key press through the remote's normal path. */
    onKey: (keycode: number) => void;
  } = $props();

  type DeviceInfo = {
    model: string;
    manufacturer: string;
    android: string;
    sdk: string;
    ip: string;
    resolution: string;
    storage_total_kb: number | null;
    storage_free_kb: number | null;
  };

  const KEY_SLEEP = 223;
  const KEY_WAKEUP = 224;

  let info = $state<DeviceInfo | null>(null);
  let loading = $state(false);
  let message = $state("");
  let notice = $state("");
  let url = $state("");
  let opening = $state(false);
  let confirmReboot = $state(false);
  let confirmTimer: ReturnType<typeof setTimeout> | undefined;
  let loadRun = 0;

  const gb = (kb: number | null) => (kb == null ? "" : (kb / 1024 / 1024).toFixed(1) + " GB");

  const rows = $derived.by(() => {
    if (!info) return [];
    const storage =
      info.storage_free_kb != null ? `${gb(info.storage_free_kb)} free of ${gb(info.storage_total_kb)}` : "";
    return [
      ["Model", [info.manufacturer, info.model].filter(Boolean).join(" ")],
      ["Android", info.android ? `${info.android}${info.sdk ? ` (API ${info.sdk})` : ""}` : ""],
      ["IP address", info.ip],
      ["Resolution", info.resolution],
      ["Storage", storage],
    ].filter(([, v]) => v);
  });

  async function load(target: string) {
    const run = ++loadRun;
    loading = true;
    message = "";
    try {
      const result = await invoke<DeviceInfo>("device_info", { serial: target });
      if (run === loadRun) info = result;
    } catch (e) {
      if (run === loadRun) {
        info = null;
        message = String(e);
      }
    } finally {
      if (run === loadRun) loading = false;
    }
  }

  $effect(() => {
    if (ready && serial) {
      load(serial);
    } else {
      loadRun++;
      info = null;
      loading = false;
    }
  });

  async function openUrl() {
    if (!serial || !url.trim() || opening) return;
    opening = true;
    message = "";
    notice = "";
    try {
      await invoke("open_url", { serial, url });
      notice = "Link sent to the device";
      url = "";
    } catch (e) {
      message = String(e);
    } finally {
      opening = false;
    }
  }

  async function reboot() {
    if (!serial) return;
    if (!confirmReboot) {
      // Two-step button: the first click arms it for a few seconds.
      confirmReboot = true;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirmReboot = false), 4000);
      return;
    }
    clearTimeout(confirmTimer);
    confirmReboot = false;
    message = "";
    notice = "";
    try {
      await invoke("reboot_device", { serial });
      notice = "Reboot command sent";
    } catch (e) {
      message = String(e);
    }
  }
</script>

<section class="tools">
  <div class="card">
    <div class="head">
      <strong>Device</strong>
      <button class="plain" disabled={!ready || loading} onclick={() => serial && load(serial)}>
        {loading ? "…" : "Refresh"}
      </button>
    </div>
    {#if !ready}
      <p class="hint">Connect a device to see its details.</p>
    {:else if rows.length}
      <dl>
        {#each rows as [label, value] (label)}
          <dt>{label}</dt>
          <dd>{value}</dd>
        {/each}
      </dl>
    {:else if loading}
      <p class="hint">Loading…</p>
    {/if}
  </div>

  <div class="card">
    <strong>Power</strong>
    <div class="grid">
      <button class="action" disabled={!ready} onclick={() => onKey(KEY_SLEEP)}>Sleep</button>
      <button class="action" disabled={!ready} onclick={() => onKey(KEY_WAKEUP)}>Wake</button>
      <button class="action" class:danger={confirmReboot} disabled={!ready} onclick={reboot}>
        {confirmReboot ? "Confirm?" : "Reboot"}
      </button>
    </div>
  </div>

  <div class="card">
    <strong>Open a link on the device</strong>
    <form
      onsubmit={(e) => {
        e.preventDefault();
        openUrl();
      }}
    >
      <input bind:value={url} placeholder="https://…" disabled={!ready} autocomplete="off" autocapitalize="off" spellcheck="false" />
      <button class="primary" disabled={!ready || opening || !url.trim()}>Open</button>
    </form>
  </div>

  {#if notice}
    <button class="notice" onclick={() => (notice = "")} title="Dismiss">{notice}</button>
  {/if}
  {#if message}
    <button class="error" onclick={() => (message = "")} title="Dismiss">{message}</button>
  {/if}
</section>

<style>
  .tools {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card {
    background: #16181c;
    border: 1px solid #24272e;
    border-radius: 14px;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  dl {
    margin: 0;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 12px;
    font-size: 13px;
  }
  dt {
    opacity: 0.55;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
  .hint {
    margin: 0;
    opacity: 0.55;
    font-size: 13px;
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
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
  }
  .action {
    background: #23262d;
    border-radius: 12px;
    min-height: 44px;
    font-weight: 600;
  }
  .action:not(:disabled):hover {
    background: #2b2f38;
  }
  .action.danger {
    background: #b3404a;
  }
  form {
    display: flex;
    gap: 8px;
  }
  input {
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
  input:focus {
    border-color: #5b8cff;
  }
  .primary {
    background: #3d6fe8;
    border-radius: 10px;
    padding: 0 14px;
    font-weight: 600;
    min-height: 38px;
  }
  .notice,
  .error {
    border-radius: 10px;
    padding: 8px 12px;
    text-align: left;
  }
  .notice {
    background: #16301f;
    border: 1px solid #2c5c3c;
    color: #a9e5bd;
  }
  .error {
    background: #3a1c1f;
    border: 1px solid #6b2b31;
    color: #ffb4b9;
  }
</style>
