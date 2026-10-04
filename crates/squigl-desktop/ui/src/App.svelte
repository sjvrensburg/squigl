<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import {
    dispatch,
    endpoint,
    lut,
    subscribe,
    type CaptureSlice,
    type Config,
    type DisplayMode,
    type Event,
    type StreamSlice,
  } from "./lib/engine";
  import { FrameClient, type Viewport } from "./lib/frames";
  import { Renderer } from "./lib/renderer";
  import { pan, turn, zoom, type View } from "./lib/viewport";

  const MODES: [DisplayMode, string][] = [
    ["normal", "Normal colours"],
    ["grey", "Black on white"],
    ["inverted", "Inverted colours"],
    ["yellow-on-black", "Yellow on black"],
    ["white-on-black", "White on black"],
    ["black-on-yellow", "Black on yellow"],
  ];

  let canvas: HTMLCanvasElement;
  let stream = $state<StreamSlice | null>(null);
  let capture = $state<CaptureSlice | null>(null);
  let config = $state<Config | null>(null);
  let view = $state<View>({ centre: [0.5, 0.5], magnification: 1 });
  let notice = $state("Starting…");
  let failure = $state<string | null>(null);

  let renderer: Renderer | null = null;
  let client: FrameClient | null = null;
  // A frame is wanted (something changed) while one is being fetched.
  let wanted = false;
  let firstFrame: (() => void) | null = null;
  // Frames drawn so far (for --dev-stats).
  let drawn = 0;

  /** Development aids (hidden flags): press keys, then save the picture and quit. */
  async function runDevOptions() {
    const dev = await invoke<{ keys: string[]; snapshot_after_ms: number | null; stats: boolean }>(
      "dev_options",
    );
    if (dev.stats) {
      let last = drawn;
      setInterval(() => {
        const rate = (drawn - last) / 5;
        last = drawn;
        invoke("page_log", { level: "info", message: `drew ${rate.toFixed(1)} frames/s` });
      }, 5000);
    }
    if (dev.keys.length === 0 && dev.snapshot_after_ms === null) return;
    const started = performance.now();
    await new Promise<void>((ok) => (firstFrame = ok));
    for (const key of dev.keys) {
      await new Promise((ok) => setTimeout(ok, 500));
      const name = key === "Space" ? " " : key;
      canvas.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true, shiftKey: name.length === 1 && name !== name.toLowerCase() }));
    }
    if (dev.snapshot_after_ms === null) return;
    const left = dev.snapshot_after_ms - (performance.now() - started);
    await new Promise((ok) => setTimeout(ok, Math.max(left, 0)));
    // Drawn and read in one task, so the drawing buffer still holds the picture.
    renderer?.draw();
    const base64 = canvas.toDataURL("image/png").split(",")[1] ?? "";
    const png = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
    await invoke("dev_save_snapshot", png);
  }

  const maxMagnification = $derived(config?.magnifier.max_magnification ?? 30);
  const frozen = $derived(capture?.frozen != null);

  const statusText = $derived.by(() => {
    const s = stream?.status;
    if (!s) return "Starting…";
    switch (s.state) {
      case "connecting":
        return stream?.source.kind === "phone" ? "Connecting to the phone…" : "Opening…";
      case "streaming":
        return frozen ? "Frozen" : "Live";
      case "waiting":
        return `${s.reason} — trying again shortly`;
      case "stopped":
        return "Stopped";
    }
  });

  function viewport(): Viewport {
    return {
      width: canvas.clientWidth,
      height: canvas.clientHeight,
      dpr: window.devicePixelRatio,
      centre: view.centre,
      magnification: view.magnification,
    };
  }

  async function fetchFrame() {
    if (!client || !renderer || client.busy) {
      wanted = true;
      return;
    }
    wanted = false;
    try {
      const frame = await client.request(viewport(), renderer.wantsLumaOnly ? "luma" : "yuv420");
      if (frame) {
        renderer.show(frame);
        drawn++;
        firstFrame?.();
        firstFrame = null;
      }
    } catch (e) {
      failure = `Lost the picture: ${e}`;
      return;
    }
    if (wanted) fetchFrame();
  }

  async function onEvent(event: Event) {
    switch (event.type) {
      case "stream":
        stream = event.data.value;
        break;
      case "capture":
        capture = event.data.value;
        fetchFrame();
        break;
      case "config": {
        const first = config === null;
        config = event.data.value;
        if (first) view = { ...view, magnification: config.magnifier.magnification };
        renderer?.setSmooth(config.magnifier.smooth);
        renderer?.setLut(await lut());
        fetchFrame();
        break;
      }
      case "frame":
        if (!frozen) fetchFrame();
        break;
      case "notice":
        notice = event.data.text;
        break;
    }
  }

  function setView(next: View) {
    view = next;
    fetchFrame();
  }

  async function toggleFreeze() {
    try {
      await dispatch({ type: frozen ? "live" : "freeze" });
      notice = frozen ? "Live" : "Frozen";
    } catch (e) {
      notice = String(e);
    }
  }

  async function rotate(quarterTurns: number) {
    const rotation = turn(capture?.rotation ?? "none", quarterTurns);
    await dispatch({ type: "set-rotation", rotation });
    setView({ ...view, centre: [0.5, 0.5] });
  }

  async function setMode(mode: DisplayMode) {
    if (!config) return;
    const next = { ...config, display: { ...config.display, mode } };
    await dispatch({ type: "set-config", config: next });
    notice = MODES.find(([m]) => m === mode)?.[1] ?? mode;
  }

  async function toggleFullscreen() {
    const w = getCurrentWindow();
    await w.setFullscreen(!(await w.isFullscreen()));
  }

  function onKey(e: KeyboardEvent) {
    // Keys belong to a focused control; the shortcuts are for the picture.
    const target = e.target as HTMLElement;
    if (target.closest("input, select, textarea, button") || e.ctrlKey || e.altKey || e.metaKey) {
      return;
    }
    const step = e.shiftKey ? 0.1 : 0.25;
    const handled = (() => {
      switch (e.key) {
        case " ":
          toggleFreeze();
          return true;
        case "r":
          rotate(1);
          return true;
        case "R":
          rotate(-1);
          return true;
        case "+":
        case "=":
          setView(zoom(view, 1, maxMagnification));
          return true;
        case "-":
          setView(zoom(view, -1, maxMagnification));
          return true;
        case "0":
          setView({ centre: [0.5, 0.5], magnification: 1 });
          return true;
        case "ArrowLeft":
          setView(pan(view, -step, 0));
          return true;
        case "ArrowRight":
          setView(pan(view, step, 0));
          return true;
        case "ArrowUp":
          setView(pan(view, 0, -step));
          return true;
        case "ArrowDown":
          setView(pan(view, 0, step));
          return true;
        case "m": {
          const i = MODES.findIndex(([m]) => m === config?.display.mode);
          setMode(MODES[(i + 1) % MODES.length]![0]);
          return true;
        }
        case "f":
          toggleFullscreen();
          return true;
      }
      return false;
    })();
    if (handled) e.preventDefault();
  }

  onMount(() => {
    try {
      renderer = new Renderer(canvas);
    } catch (e) {
      failure = String(e);
      return;
    }
    const resize = () => {
      canvas.width = Math.round(canvas.clientWidth * window.devicePixelRatio);
      canvas.height = Math.round(canvas.clientHeight * window.devicePixelRatio);
      renderer?.draw();
      fetchFrame();
    };
    // The observer follows layout changes; it is not alone, since WebKitGTK does not
    // run it for a window that is not being drawn (behind another, minimised).
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    window.addEventListener("resize", resize);
    (async () => {
      client = new FrameClient(await endpoint());
      await subscribe(onEvent);
      fetchFrame();
      await runDevOptions();
    })().catch((e) => (failure = String(e)));
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", resize);
    };
  });
</script>

<svelte:window onkeydown={onKey} />

<main>
  <header>
    <button onclick={toggleFreeze} aria-pressed={frozen}>
      {frozen ? "Live" : "Freeze"} <kbd>Space</kbd>
    </button>
    <button onclick={() => rotate(-1)}>Rotate left <kbd>Shift+R</kbd></button>
    <button onclick={() => rotate(1)}>Rotate right <kbd>R</kbd></button>
    <label>
      Magnification
      <input
        type="range"
        min="1"
        max={maxMagnification}
        step="0.25"
        value={view.magnification}
        oninput={(e) => setView({ ...view, magnification: Number(e.currentTarget.value) })}
      />
      <output>{view.magnification.toFixed(1)}×</output>
    </label>
    <label>
      Colours
      <select
        value={config?.display.mode ?? "normal"}
        onchange={(e) => setMode(e.currentTarget.value as DisplayMode)}
      >
        {#each MODES as [mode, label]}
          <option value={mode}>{label}</option>
        {/each}
      </select>
    </label>
    <button onclick={toggleFullscreen}>Full screen <kbd>F</kbd></button>
  </header>

  <canvas bind:this={canvas} tabindex="0" aria-label="Magnified camera picture"></canvas>

  <footer>
    <span>{statusText}</span>
    <span role="status" aria-live="polite">{notice}</span>
  </footer>

  {#if failure}
    <p class="failure" role="alert">{failure}</p>
  {/if}
</main>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: #000;
    color: #fff;
    font: 1.125rem/1.4 system-ui, sans-serif;
  }
  main {
    display: grid;
    grid-template-rows: auto 1fr auto;
    height: 100vh;
  }
  header,
  footer {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem 1rem;
    align-items: center;
    padding: 0.5rem 0.75rem;
    background: #111;
  }
  footer {
    justify-content: space-between;
  }
  button,
  select,
  input {
    font: inherit;
  }
  button,
  select {
    padding: 0.35rem 0.75rem;
    border: 2px solid #888;
    border-radius: 0.35rem;
    background: #222;
    color: #fff;
  }
  kbd {
    font-size: 0.8em;
    opacity: 0.8;
  }
  :global(:focus-visible) {
    outline: 0.2rem solid #ffd400;
    outline-offset: 0.15rem;
  }
  canvas {
    width: 100%;
    height: 100%;
    display: block;
    min-height: 0;
  }
  .failure {
    position: fixed;
    inset: auto 1rem 4rem 1rem;
    margin: 0;
    padding: 1rem;
    background: #600;
    border: 2px solid #f88;
  }
</style>
