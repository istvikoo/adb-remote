<script lang="ts">
  import { invoke, Channel } from "@tauri-apps/api/core";
  import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
  import { onMount } from "svelte";

  type Device = { serial: string; state: string; model: string | null };

  const KEY = {
    up: 19, down: 20, left: 21, right: 22, ok: 23,
    back: 4, home: 3, menu: 82, power: 26,
    volUp: 24, volDown: 25, mute: 164,
    playPause: 85, rewind: 89, forward: 90, prev: 88, next: 87,
    del: 67, enter: 66, settings: 176,
  } as const;

  /** PC keys -> Android keycodes. true = repeat while held. */
  const KEYBOARD: Record<string, [number, boolean]> = {
    ArrowUp: [KEY.up, true],
    ArrowDown: [KEY.down, true],
    ArrowLeft: [KEY.left, true],
    ArrowRight: [KEY.right, true],
    Enter: [KEY.ok, false],
    Escape: [KEY.back, false],
    Backspace: [KEY.del, true],
    Home: [KEY.home, false],
    " ": [KEY.playPause, false],
    "+": [KEY.volUp, true],
    "=": [KEY.volUp, true],
    "-": [KEY.volDown, true],
  };
  /** Letter shortcuts are matched by physical key so they work in any layout. */
  const KEYBOARD_BY_CODE: Record<string, [number, boolean]> = {
    KeyM: [KEY.mute, false],
  };
  const keyFor = (e: KeyboardEvent) => KEYBOARD[e.key] ?? KEYBOARD_BY_CODE[e.code];

  const SAVED_KEY = "adb-remote.saved";

  let devices = $state<Device[]>([]);
  let active = $state<string | null>(null);
  let address = $state("");
  let saved = $state<string[]>([]);
  let connecting = $state(false);
  let error = $state("");
  /** adb itself is unavailable; set and cleared by the device poll. */
  let adbError = $state("");
  let refreshing = false;
  let text = $state("");
  let showDevices = $state(true);

  const activeDevice = $derived(devices.find((d) => d.serial === active && d.state === "device"));
  const ready = $derived(!!activeDevice);

  function stateLabel(s: string) {
    if (s === "device") return "connected";
    if (s === "unauthorized") return "confirm on the TV screen";
    if (s === "offline") return "offline";
    return s;
  }

  function loadSaved() {
    try {
      const list: unknown = JSON.parse(localStorage.getItem(SAVED_KEY) ?? "[]");
      saved = Array.isArray(list) ? list.filter((a) => typeof a === "string") : [];
    } catch {
      saved = [];
    }
  }

  function remember(addr: string) {
    saved = [addr, ...saved.filter((a) => a !== addr)].slice(0, 8);
    try {
      localStorage.setItem(SAVED_KEY, JSON.stringify(saved));
    } catch {}
  }

  function forget(addr: string) {
    saved = saved.filter((a) => a !== addr);
    try {
      localStorage.setItem(SAVED_KEY, JSON.stringify(saved));
    } catch {}
  }

  async function refresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      devices = await invoke<Device[]>("list_devices");
      adbError = "";
      if (!devices.some((d) => d.serial === active && d.state === "device")) {
        active = devices.find((d) => d.state === "device")?.serial ?? null;
      }
    } catch (e) {
      adbError = String(e);
    } finally {
      refreshing = false;
    }
  }

  async function connect(addr: string) {
    if (connecting || !addr.trim()) return;
    connecting = true;
    error = "";
    try {
      const serial = await invoke<string>("connect", { address: addr });
      remember(serial);
      address = "";
      await refresh();
      if (devices.some((d) => d.serial === serial && d.state === "device")) {
        active = serial;
        showDevices = false;
        // Already mirroring another device: switch instead of closing the view.
        if (mirroring) await stopMirror(true);
        startMirror();
      }
    } catch (e) {
      error = String(e);
      await refresh();
    } finally {
      connecting = false;
    }
  }

  async function disconnect(serial: string) {
    try {
      await invoke("disconnect", { serial });
    } catch (e) {
      error = String(e);
    }
    await refresh();
  }

  async function press(keycode: number, longpress = false) {
    if (!active) return;
    try {
      await invoke("send_key", { serial: active, keycode, longpress });
    } catch (e) {
      error = String(e);
    }
  }

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  let holdToken = 0;

  /** Press with repeat while held. The next command is sent only after the
   *  previous one finishes, so no queue builds up. */
  function holdStart(keycode: number, repeat: boolean) {
    if (!ready) return;
    const token = ++holdToken;
    (async () => {
      await press(keycode);
      if (!repeat) return;
      await sleep(350);
      while (token === holdToken) {
        await press(keycode);
        await sleep(60);
      }
    })();
  }

  function holdEnd() {
    holdToken++;
  }

  async function sendText() {
    if (!active || !text) return;
    try {
      await invoke("send_text", { serial: active, text });
      text = "";
    } catch (e) {
      error = String(e);
    }
  }

  // ---------- screen view (scrcpy) ----------

  const QUALITY = {
    low: { label: "Low (remote)", maxSize: 960, bitRate: 1_500_000, maxFps: 20 },
    normal: { label: "Normal", maxSize: 1280, bitRate: 4_000_000, maxFps: 30 },
    high: { label: "High (local network)", maxSize: 1920, bitRate: 8_000_000, maxFps: 60 },
  } as const;
  type QualityKey = keyof typeof QUALITY;
  const QUALITY_KEY = "adb-remote.quality";

  let quality = $state<QualityKey>("low");

  function loadQuality() {
    try {
      const q = localStorage.getItem(QUALITY_KEY);
      if (q && q in QUALITY) quality = q as QualityKey;
    } catch {}
  }

  async function setQuality(q: string) {
    if (!(q in QUALITY) || q === quality) return;
    quality = q as QualityKey;
    try {
      localStorage.setItem(QUALITY_KEY, quality);
    } catch {}
    // A new quality is applied by restarting the mirror.
    if (mirroring && !mirrorBusy) {
      await stopMirror(true);
      await startMirror();
    }
  }

  const MSG = { session: 0, config: 1, key: 2, delta: 3, ended: 255 } as const;

  let mirroring = $state(false);
  let mirrorBusy = $state(false);
  let hasPicture = $state(false);
  let canvas = $state<HTMLCanvasElement | null>(null);
  let mirrorSerial: string | null = null;
  let mirrorRun = 0;
  let decoder: VideoDecoder | null = null;
  let waitingKey = true;
  let pointerDown = false;

  async function resizeWindow(width: number) {
    try {
      const win = getCurrentWindow();
      const size = (await win.innerSize()).toLogical(await win.scaleFactor());
      await win.setSize(new LogicalSize(width, Math.max(size.height, 640)));
    } catch {}
  }

  function closeDecoder() {
    try {
      if (decoder && decoder.state !== "closed") decoder.close();
    } catch {}
    decoder = null;
    waitingKey = true;
  }

  function configureDecoder(codec: string, run: number) {
    closeDecoder();
    if (typeof VideoDecoder === "undefined") {
      error = "This system version does not support video decoding (WebCodecs)";
      stopMirror();
      return;
    }
    decoder = new VideoDecoder({
      output: (frame) => {
        if (run === mirrorRun && canvas) {
          if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
            canvas.width = frame.displayWidth;
            canvas.height = frame.displayHeight;
          }
          canvas.getContext("2d")?.drawImage(frame, 0, 0);
          hasPicture = true;
        }
        frame.close();
      },
      error: (e) => {
        if (run !== mirrorRun) return;
        error = "Video decoding error: " + e.message;
        stopMirror();
      },
    });
    decoder.configure({ codec, optimizeForLatency: true });
  }

  function onVideoMessage(raw: unknown, run: number) {
    if (run !== mirrorRun) return;
    const u8 = raw instanceof ArrayBuffer ? new Uint8Array(raw) : new Uint8Array(raw as ArrayLike<number>);
    if (!u8.length) return;
    const type = u8[0];
    if (type === MSG.config) {
      configureDecoder(new TextDecoder().decode(u8.subarray(1)), run);
    } else if (type === MSG.key || type === MSG.delta) {
      if (!decoder || decoder.state !== "configured") return;
      if (type === MSG.delta && waitingKey) return;
      waitingKey = false;
      const pts = Number(new DataView(u8.buffer, u8.byteOffset, u8.byteLength).getBigUint64(1));
      try {
        decoder.decode(
          new EncodedVideoChunk({
            type: type === MSG.key ? "key" : "delta",
            timestamp: pts,
            data: u8.subarray(9),
          }),
        );
      } catch (e) {
        error = "Video decoding error: " + String(e);
        stopMirror();
      }
    } else if (type === MSG.ended) {
      const reason = new TextDecoder().decode(u8.subarray(1));
      if (reason) error = reason;
      stopMirror();
    }
  }

  async function startMirror() {
    if (!active || mirrorBusy || mirroring) return;
    const serial = active;
    const run = ++mirrorRun;
    mirrorBusy = true;
    mirroring = true;
    hasPicture = false;
    mirrorSerial = serial;
    error = "";
    await resizeWindow(1180);
    try {
      const channel = new Channel<unknown>();
      channel.onmessage = (m) => onVideoMessage(m, run);
      const q = QUALITY[quality];
      await invoke("start_mirror", {
        serial,
        maxSize: q.maxSize,
        bitRate: q.bitRate,
        maxFps: q.maxFps,
        onData: channel,
      });
      // Stopped while starting: the backend registered the mirror after our
      // stop request, so stop it again.
      if (run !== mirrorRun) await invoke("stop_mirror", { serial }).catch(() => {});
    } catch (e) {
      if (run === mirrorRun) {
        error = String(e);
        await stopMirror();
      }
    } finally {
      mirrorBusy = false;
    }
  }

  async function stopMirror(keepWindow = false) {
    if (!mirroring) return;
    mirrorRun++;
    mirroring = false;
    hasPicture = false;
    pointerDown = false;
    closeDecoder();
    const serial = mirrorSerial;
    mirrorSerial = null;
    if (serial) {
      try {
        await invoke("stop_mirror", { serial });
      } catch {}
    }
    if (!keepWindow) await resizeWindow(380);
  }

  $effect(() => {
    // Device gone or another one selected: stop mirroring.
    if (mirroring && !mirrorBusy && (!ready || active !== mirrorSerial)) stopMirror();
  });

  /** Touch and scroll events are sent strictly one after another, so a
   *  release can never overtake its press. */
  let inputChain: Promise<unknown> = Promise.resolve();
  function sendInput(cmd: string, args: Record<string, unknown>) {
    inputChain = inputChain.then(() => invoke(cmd, args)).catch(() => {});
  }

  function touch(e: PointerEvent, action: number) {
    if (!canvas || !mirrorSerial || !hasPicture) return;
    const r = canvas.getBoundingClientRect();
    if (!r.width || !r.height) return;
    const clamp = (v: number, max: number) => Math.max(0, Math.min(max - 1, Math.round(v)));
    sendInput("mirror_touch", {
      serial: mirrorSerial,
      action,
      x: clamp(((e.clientX - r.left) / r.width) * canvas.width, canvas.width),
      y: clamp(((e.clientY - r.top) / r.height) * canvas.height, canvas.height),
      width: canvas.width,
      height: canvas.height,
    });
  }

  function onScreenDown(e: PointerEvent) {
    if (e.button !== 0) return;
    pointerDown = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    touch(e, 0);
  }

  function onScreenMove(e: PointerEvent) {
    if (pointerDown) touch(e, 2);
  }

  function onScreenUp(e: PointerEvent) {
    if (!pointerDown) return;
    pointerDown = false;
    touch(e, 1);
  }

  function onScreenWheel(e: WheelEvent) {
    if (!canvas || !mirrorSerial || !hasPicture) return;
    e.preventDefault();
    const r = canvas.getBoundingClientRect();
    const step = (d: number) => Math.max(-4, Math.min(4, -d / 40));
    sendInput("mirror_scroll", {
      serial: mirrorSerial,
      x: Math.round(((e.clientX - r.left) / r.width) * canvas.width),
      y: Math.round(((e.clientY - r.top) / r.height) * canvas.height),
      width: canvas.width,
      height: canvas.height,
      dx: step(e.deltaX),
      dy: step(e.deltaY),
    });
  }

  function isTyping(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA");
  }

  function onKeyDown(e: KeyboardEvent) {
    if (isTyping(e) || e.metaKey || e.ctrlKey || e.altKey) return;
    const map = keyFor(e);
    if (!map) return;
    e.preventDefault();
    if (e.repeat) return;
    holdStart(map[0], map[1]);
  }

  function onKeyUp(e: KeyboardEvent) {
    if (keyFor(e)) holdEnd();
  }

  onMount(() => {
    loadSaved();
    loadQuality();
    // After a page reload the backend may still be streaming to nobody.
    invoke("stop_all_mirrors").catch(() => {});
    refresh();
    const timer = setInterval(refresh, 3000);
    return () => clearInterval(timer);
  });
</script>

<svelte:window onkeydown={onKeyDown} onkeyup={onKeyUp} onblur={holdEnd} />

{#snippet key(label: string, code: number, opts: { repeat?: boolean; cls?: string; title?: string } = {})}
  <button
    class="key {opts.cls ?? ''}"
    title={opts.title}
    aria-label={opts.title ?? label}
    disabled={!ready}
    onpointerdown={() => holdStart(code, opts.repeat ?? false)}
    onpointerup={holdEnd}
    onpointerleave={holdEnd}
    onpointercancel={holdEnd}
  >{label}</button>
{/snippet}

<div class="app" class:wide={mirroring}>
{#if mirroring}
  <section class="screen">
    <canvas
      bind:this={canvas}
      class:hidden={!hasPicture}
      onpointerdown={onScreenDown}
      onpointermove={onScreenMove}
      onpointerup={onScreenUp}
      onpointercancel={onScreenUp}
      onwheel={onScreenWheel}
    ></canvas>
    {#if !hasPicture}
      <p class="wait">Starting screen mirroring…</p>
    {/if}
  </section>
{/if}
<main>
  <header>
    <button class="status" onclick={() => (showDevices = !showDevices)}>
      <span class="dot" class:on={ready}></span>
      <span class="name">
        {#if activeDevice}
          {activeDevice.model ?? activeDevice.serial}
        {:else}
          Not connected
        {/if}
      </span>
      <span class="chev">{showDevices ? "▴" : "▾"}</span>
    </button>
    <button
      class="view"
      class:on={mirroring}
      disabled={!ready || mirrorBusy}
      title={mirroring ? "Turn off screen view" : "Show the device screen"}
      onclick={() => (mirroring ? stopMirror() : startMirror())}
    >Screen</button>
    {@render key("⏻", KEY.power, { cls: "power", title: "Power" })}
  </header>

  {#if showDevices}
    <section class="panel">
      <form
        onsubmit={(e) => {
          e.preventDefault();
          connect(address);
        }}
      >
        <input
          bind:value={address}
          placeholder="Device IP, e.g. 192.168.1.50"
          autocomplete="off"
          spellcheck="false"
        />
        <button class="primary" disabled={connecting || !address.trim()}>
          {connecting ? "…" : "Connect"}
        </button>
      </form>

      <label class="quality">
        <span>Screen quality</span>
        <select value={quality} onchange={(e) => setQuality(e.currentTarget.value)} disabled={mirrorBusy}>
          {#each Object.entries(QUALITY) as [k, q] (k)}
            <option value={k}>{q.label} · up to {q.maxSize} px · {q.bitRate / 1_000_000} Mbps · {q.maxFps} fps</option>
          {/each}
        </select>
      </label>

      {#each devices as d (d.serial)}
        <div class="row" class:sel={d.serial === active}>
          <button class="pick" disabled={d.state !== "device"} onclick={() => (active = d.serial)}>
            <strong>{d.model ?? d.serial}</strong>
            <small>{d.model ? d.serial + " · " : ""}{stateLabel(d.state)}</small>
          </button>
          {#if d.serial.includes(":")}
            <button class="x" title="Disconnect" onclick={() => disconnect(d.serial)}>✕</button>
          {/if}
        </div>
      {/each}

      {#if saved.some((a) => !devices.some((d) => d.serial === a))}
        <div class="saved">
          {#each saved.filter((a) => !devices.some((d) => d.serial === a)) as a (a)}
            <span class="chip">
              <button onclick={() => connect(a)} disabled={connecting}>{a.replace(/:5555$/, "")}</button>
              <button class="x" title="Forget" onclick={() => forget(a)}>✕</button>
            </span>
          {/each}
        </div>
      {/if}
    </section>
  {/if}

  {#if adbError}
    <div class="error">{adbError}</div>
  {/if}
  {#if error}
    <button class="error" onclick={() => (error = "")} title="Dismiss">{error}</button>
  {/if}

  <section class="remote" class:off={!ready}>
    <div class="dpad">
      {@render key("▲", KEY.up, { repeat: true, cls: "up", title: "Up" })}
      {@render key("◀", KEY.left, { repeat: true, cls: "left", title: "Left" })}
      {@render key("OK", KEY.ok, { cls: "ok" })}
      {@render key("▶", KEY.right, { repeat: true, cls: "right", title: "Right" })}
      {@render key("▼", KEY.down, { repeat: true, cls: "down", title: "Down" })}
    </div>

    <div class="grid three">
      {@render key("↩", KEY.back, { title: "Back" })}
      {@render key("⌂", KEY.home, { title: "Home" })}
      {@render key("☰", KEY.menu, { title: "Menu" })}
    </div>

    <div class="grid three">
      {@render key("−", KEY.volDown, { repeat: true, title: "Volume down" })}
      {@render key("Mute", KEY.mute, { cls: "small" })}
      {@render key("+", KEY.volUp, { repeat: true, title: "Volume up" })}
    </div>

    <div class="grid five">
      {@render key("⏮︎", KEY.prev, { title: "Previous" })}
      {@render key("⏪︎", KEY.rewind, { repeat: true, title: "Rewind" })}
      {@render key("⏯︎", KEY.playPause, { title: "Play / pause" })}
      {@render key("⏩︎", KEY.forward, { repeat: true, title: "Fast forward" })}
      {@render key("⏭︎", KEY.next, { title: "Next" })}
    </div>

    <form
      class="text"
      onsubmit={(e) => {
        e.preventDefault();
        sendText();
      }}
    >
      <input bind:value={text} placeholder={mirroring ? "Text" : "Text (Latin only)"} disabled={!ready} autocomplete="off" spellcheck="false" />
      <button class="primary" disabled={!ready || !text}>Send</button>
    </form>
    <div class="grid three">
      {@render key("⌫", KEY.del, { repeat: true, title: "Delete" })}
      {@render key("Enter", KEY.enter, { cls: "small" })}
      {@render key("Settings", KEY.settings, { cls: "small" })}
    </div>
  </section>

  <footer>Keyboard: arrows · Enter = OK · Esc = back · Space = pause · +/− volume</footer>
</main>
</div>

<style>
  :global(html) {
    color-scheme: dark;
  }
  :global(body) {
    margin: 0;
    background: #101114;
    color: #e9eaee;
    font: 14px/1.4 -apple-system, "Segoe UI", system-ui, sans-serif;
    user-select: none;
    -webkit-user-select: none;
    cursor: default;
  }
  .app.wide {
    display: flex;
    height: 100vh;
  }
  .app.wide main {
    width: 360px;
    flex: none;
    margin: 0;
    min-height: 0;
    overflow-y: auto;
  }
  .screen {
    flex: 1;
    min-width: 0;
    background: #000;
    display: flex;
    align-items: center;
    justify-content: center;
    position: relative;
  }
  .screen canvas {
    max-width: 100%;
    max-height: 100vh;
    touch-action: none;
    cursor: pointer;
  }
  .screen canvas.hidden {
    visibility: hidden;
    position: absolute;
  }
  .wait {
    opacity: 0.6;
  }
  .view {
    flex: none;
    background: #1b1d22;
    border-radius: 12px;
    padding: 0 12px;
    min-height: 44px;
    font-weight: 600;
  }
  .view.on {
    background: #3d6fe8;
  }
  .view:disabled {
    opacity: 0.4;
  }
  main {
    max-width: 360px;
    margin: 0 auto;
    padding: 14px 16px 10px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 100vh;
    box-sizing: border-box;
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

  header {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  .status {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    background: #1b1d22;
    border-radius: 12px;
    padding: 10px 12px;
    text-align: left;
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .chev {
    opacity: 0.5;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #5a5d66;
    flex: none;
  }
  .dot.on {
    background: #3ddc84;
    box-shadow: 0 0 8px #3ddc8499;
  }

  .panel {
    background: #16181c;
    border: 1px solid #24272e;
    border-radius: 14px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  form {
    display: flex;
    gap: 8px;
  }
  .primary {
    background: #3d6fe8;
    border-radius: 10px;
    padding: 0 14px;
    font-weight: 600;
    min-height: 38px;
  }
  .primary:disabled {
    opacity: 0.4;
  }
  .quality {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .quality span {
    opacity: 0.6;
  }
  .quality select {
    font: inherit;
    font-size: 13px;
    color: inherit;
    background: #1b1d22;
    border: 1px solid #2c2f37;
    border-radius: 10px;
    padding: 8px 10px;
    width: 100%;
  }
  .row {
    display: flex;
    align-items: center;
    border-radius: 10px;
    background: #1b1d22;
    border: 1px solid transparent;
  }
  .row.sel {
    border-color: #3d6fe8;
  }
  .pick {
    flex: 1;
    min-width: 0;
    text-align: left;
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
  }
  .pick small {
    opacity: 0.6;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .x {
    opacity: 0.5;
    padding: 6px 10px;
  }
  .x:hover {
    opacity: 1;
  }
  .saved {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    background: #1b1d22;
    border-radius: 999px;
    padding-left: 10px;
    font-size: 13px;
  }
  .chip .x {
    padding: 5px 9px 5px 6px;
  }

  .error {
    background: #3a1c1f;
    border: 1px solid #6b2b31;
    color: #ffb4b9;
    border-radius: 10px;
    padding: 8px 12px;
    text-align: left;
    user-select: text;
    -webkit-user-select: text;
  }

  .remote {
    display: flex;
    flex-direction: column;
    gap: 12px;
    transition: opacity 0.2s;
  }
  .remote.off {
    opacity: 0.45;
  }
  .key {
    background: #23262d;
    border-radius: 14px;
    min-height: 48px;
    font-size: 20px;
    transition:
      background 0.08s,
      transform 0.08s;
    touch-action: none;
  }
  .key.small {
    font-size: 13px;
    font-weight: 600;
  }
  .key:not(:disabled):hover {
    background: #2b2f38;
  }
  .key:not(:disabled):active {
    background: #3d6fe8;
    transform: scale(0.96);
  }
  .power {
    width: 44px;
    min-height: 44px;
    border-radius: 12px;
    color: #ff6b6b;
    flex: none;
  }

  .dpad {
    width: 228px;
    height: 228px;
    margin: 4px auto;
    display: grid;
    grid-template: repeat(3, 1fr) / repeat(3, 1fr);
    gap: 6px;
    background: #16181c;
    border-radius: 50%;
    padding: 10px;
    box-sizing: border-box;
  }
  .dpad .key {
    font-size: 18px;
    min-height: 0;
    background: #23262d;
  }
  .up {
    grid-area: 1 / 2;
    border-radius: 34px 34px 12px 12px;
  }
  .left {
    grid-area: 2 / 1;
    border-radius: 34px 12px 12px 34px;
  }
  .right {
    grid-area: 2 / 3;
    border-radius: 12px 34px 34px 12px;
  }
  .down {
    grid-area: 3 / 2;
    border-radius: 12px 12px 34px 34px;
  }
  .dpad .ok {
    grid-area: 2 / 2;
    border-radius: 50%;
    font-size: 16px;
    font-weight: 700;
    background: #2f333c;
  }

  .grid {
    display: grid;
    gap: 8px;
  }
  .three {
    grid-template-columns: repeat(3, 1fr);
  }
  .five {
    grid-template-columns: repeat(5, 1fr);
  }
  .five .key {
    font-size: 17px;
  }

  footer {
    margin-top: auto;
    font-size: 11px;
    opacity: 0.4;
    text-align: center;
  }
</style>
