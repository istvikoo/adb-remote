<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, tick } from "svelte";

  let { serial, ready }: { serial: string | null; ready: boolean } = $props();

  type Entry = { id: number; cmd: string; output: string; note: string | null; failed: boolean; pending: boolean };
  type ExecResult = { output: string; note: string | null };

  const HISTORY_KEY = "adb-remote.console.history";
  const SAVED_KEY = "adb-remote.console.saved";
  const MAX_ENTRIES = 50;
  const MAX_HISTORY = 50;

  let entries = $state<Entry[]>([]);
  let command = $state("");
  let running = $state(false);
  let history = $state<string[]>([]);
  let saved = $state<string[]>([]);
  let historyIndex = -1;
  let draft = "";
  let nextId = 0;
  let outputEl = $state<HTMLElement | null>(null);

  function loadList(key: string): string[] {
    try {
      const list: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
      return Array.isArray(list) ? list.filter((x) => typeof x === "string") : [];
    } catch {
      return [];
    }
  }

  function store(key: string, list: string[]) {
    try {
      localStorage.setItem(key, JSON.stringify(list));
    } catch {}
  }

  onMount(() => {
    history = loadList(HISTORY_KEY);
    saved = loadList(SAVED_KEY);
  });

  async function scrollDown() {
    await tick();
    if (outputEl) outputEl.scrollTop = outputEl.scrollHeight;
  }

  async function run(cmd: string) {
    cmd = cmd.trim();
    if (!cmd || !serial || running) return;
    history = [cmd, ...history.filter((h) => h !== cmd)].slice(0, MAX_HISTORY);
    store(HISTORY_KEY, history);
    historyIndex = -1;
    command = "";

    const id = nextId++;
    entries = [...entries, { id, cmd, output: "", note: null, failed: false, pending: true }].slice(-MAX_ENTRIES);
    running = true;
    scrollDown();

    let patch: Partial<Entry>;
    try {
      const r = await invoke<ExecResult>("run_shell", { serial, command: cmd });
      patch = { output: r.output, note: r.note };
    } catch (e) {
      patch = { output: String(e), failed: true };
    }
    entries = entries.map((en) => (en.id === id ? { ...en, ...patch, pending: false } : en));
    running = false;
    scrollDown();
  }

  function stop() {
    invoke("cancel_shell").catch(() => {});
  }

  function onInputKey(e: KeyboardEvent) {
    if (e.key === "ArrowUp") {
      if (!history.length) return;
      e.preventDefault();
      if (historyIndex === -1) draft = command;
      historyIndex = Math.min(historyIndex + 1, history.length - 1);
      command = history[historyIndex];
    } else if (e.key === "ArrowDown") {
      if (historyIndex === -1) return;
      e.preventDefault();
      historyIndex -= 1;
      command = historyIndex === -1 ? draft : history[historyIndex];
    }
  }

  function toggleSaved(cmd: string) {
    cmd = cmd.trim();
    if (!cmd) return;
    saved = saved.includes(cmd) ? saved.filter((s) => s !== cmd) : [...saved, cmd].slice(-20);
    store(SAVED_KEY, saved);
  }
</script>

<section class="console">
  <div class="output" bind:this={outputEl}>
    {#if !entries.length}
      <p class="hint">Runs shell commands on the device, like <code>adb shell …</code>. A command is stopped after 15 seconds.</p>
    {/if}
    {#each entries as en (en.id)}
      <div class="entry">
        <div class="cmd">
          <span>$ {en.cmd}</span>
          <button title={saved.includes(en.cmd) ? "Remove from saved" : "Save command"} onclick={() => toggleSaved(en.cmd)}>
            {saved.includes(en.cmd) ? "★" : "☆"}
          </button>
        </div>
        {#if en.pending}
          <pre class="dim">running…</pre>
        {:else}
          {#if en.output}<pre class:failed={en.failed}>{en.output}</pre>{/if}
          {#if en.note}<pre class="dim">[{en.note}]</pre>{/if}
          {#if !en.output && !en.note}<pre class="dim">(no output)</pre>{/if}
        {/if}
      </div>
    {/each}
  </div>

  {#if saved.length}
    <div class="saved">
      {#each saved as s (s)}
        <span class="chip">
          <button title={s} disabled={!ready || running} onclick={() => run(s)}>{s}</button>
          <button class="x" title="Remove" onclick={() => toggleSaved(s)}>✕</button>
        </span>
      {/each}
    </div>
  {/if}

  <form
    onsubmit={(e) => {
      e.preventDefault();
      run(command);
    }}
  >
    <input
      bind:value={command}
      onkeydown={onInputKey}
      placeholder="Shell command, e.g. getprop ro.product.model"
      disabled={!ready}
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
    />
    {#if running}
      <button type="button" class="primary stop" onclick={stop}>Stop</button>
    {:else}
      <button class="primary" disabled={!ready || !command.trim()}>Run</button>
    {/if}
  </form>
  {#if entries.length}
    <button class="clear" disabled={running} onclick={() => (entries = [])}>Clear output</button>
  {/if}
</section>

<style>
  .console {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 1;
    min-height: 0;
  }
  .output {
    flex: 1;
    min-height: 220px;
    overflow-y: auto;
    background: #0b0c0e;
    border: 1px solid #24272e;
    border-radius: 12px;
    padding: 10px;
    user-select: text;
    -webkit-user-select: text;
    cursor: text;
  }
  .hint {
    margin: 0;
    opacity: 0.55;
    font-size: 13px;
  }
  .entry + .entry {
    margin-top: 10px;
  }
  .cmd {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    color: #8fb0ff;
    font: 12px/1.4 ui-monospace, Menlo, Consolas, monospace;
  }
  .cmd span {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .cmd button {
    opacity: 0.7;
    padding: 0 2px;
  }
  pre {
    margin: 2px 0 0;
    font: 12px/1.4 ui-monospace, Menlo, Consolas, monospace;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  pre.dim {
    opacity: 0.5;
  }
  pre.failed {
    color: #ffb4b9;
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
  form {
    display: flex;
    gap: 8px;
  }
  input {
    flex: 1;
    min-width: 0;
    font: 13px ui-monospace, Menlo, Consolas, monospace;
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
  .primary.stop {
    background: #b3404a;
  }
  .saved {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    max-width: 100%;
    background: #1b1d22;
    border-radius: 999px;
    padding-left: 10px;
    font: 12px ui-monospace, Menlo, Consolas, monospace;
  }
  .chip button:first-child {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 240px;
    padding: 5px 0;
  }
  .chip .x {
    opacity: 0.5;
    padding: 5px 9px 5px 6px;
  }
  .clear {
    align-self: flex-start;
    font-size: 12px;
    opacity: 0.6;
  }
</style>
