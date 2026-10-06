<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import {
    dispatch,
    endpoint,
    lut,
    openImage,
    subscribe,
    type CaptureSlice,
    type Config,
    type DisplayMode,
    type Event,
    type StreamSlice,
  } from "./lib/engine";
  import { FrameClient, type FrameHeader, type Viewport } from "./lib/frames";
  import { MODES, modeLabel } from "./lib/modes";
  import { Renderer, type FrameRenderer } from "./lib/renderer";
  import { Renderer2D } from "./lib/renderer2d";
  import { keyFor, keyLabel, table, type Action } from "./lib/shortcuts";
  import { pan, turn, zoom, type View } from "./lib/viewport";
  import Connection from "./Connection.svelte";
  import Settings from "./Settings.svelte";

  // The modes the toolbar cycles through; "custom" is reached in Settings.
  const QUICK_MODES = MODES.filter(([m]) => m !== "custom");

  let canvas: HTMLCanvasElement;
  let fileInput: HTMLInputElement;
  let stream = $state<StreamSlice | null>(null);
  let capture = $state<CaptureSlice | null>(null);
  let config = $state<Config | null>(null);
  let view = $state<View>({ centre: [0.5, 0.5], magnification: 1 });
  // The status on the left says "Starting…"; this is for what happens after.
  let notice = $state("");
  let failure = $state<string | null>(null);
  let settingsOpen = $state(false);
  let dropping = $state(false);

  let renderer: FrameRenderer | null = null;
  let client: FrameClient | null = null;
  // A frame is wanted (something changed) while as many as may be are being fetched.
  let wanted = false;
  let firstFrame: (() => void) | null = null;
  // Frames drawn so far (for --dev-stats), and the last one's header.
  let drawn = 0;
  let shown: FrameHeader | null = null;
  // Milliseconds from asking for each frame to having drawn it (for --dev-stats).
  let fetchTimes: number[] = [];

  const maxMagnification = $derived(config?.magnifier.max_magnification ?? 30);
  const frozen = $derived(capture?.frozen != null);
  const keys = $derived(config ? table(config.desktop) : new Map<string, Action>());
  const shortcut = (action: Action) => {
    if (!config) return "";
    const key = keyFor(action, config.desktop);
    return key && keys.get(key) === action ? keyLabel(key) : "";
  };

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

  $effect(() => {
    document.documentElement.dataset.theme = config?.desktop.theme ?? "dark";
  });

  function say(text: string) {
    notice = text;
  }

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
      const asked = performance.now();
      const frame = await client.request(viewport(), renderer.wantsLumaOnly ? "luma" : "yuv420");
      if (frame) {
        renderer.show(frame);
        drawn++;
        shown = frame.header;
        fetchTimes.push(performance.now() - asked);
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
        say(event.data.text);
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
      say(frozen ? "Live" : "Frozen");
    } catch (e) {
      say(String(e));
    }
  }

  async function rotate(quarterTurns: number) {
    const rotation = turn(capture?.rotation ?? "none", quarterTurns);
    await dispatch({ type: "set-rotation", rotation });
    setView({ ...view, centre: [0.5, 0.5] });
    say(`Turned ${quarterTurns > 0 ? "right" : "left"}`);
  }

  async function setConfig(next: Config) {
    try {
      await dispatch({ type: "set-config", config: next });
    } catch (e) {
      say(`Could not save the settings: ${e}`);
    }
  }

  async function setMode(mode: DisplayMode) {
    if (!config) return;
    await setConfig({ ...config, display: { ...config.display, mode } });
    say(modeLabel(mode));
  }

  async function toggleReadingLine() {
    if (!config) return;
    const on = !config.magnifier.reading_line;
    await setConfig({ ...config, magnifier: { ...config.magnifier, reading_line: on } });
    say(on ? "Reading line on" : "Reading line off");
  }

  async function toggleFullscreen() {
    const w = getCurrentWindow();
    await w.setFullscreen(!(await w.isFullscreen()));
  }

  async function openBytes(bytes: Uint8Array, what: string) {
    try {
      await openImage(bytes);
      setView({ centre: [0.5, 0.5], magnification: 1 });
    } catch (e) {
      say(`Could not open ${what}: ${e}`);
    }
  }

  async function onFilePicked() {
    const file = fileInput.files?.[0];
    if (file) await openBytes(new Uint8Array(await file.arrayBuffer()), file.name);
    fileInput.value = "";
  }

  async function onPaste(e: ClipboardEvent) {
    const item = [...(e.clipboardData?.items ?? [])].find((i) => i.type.startsWith("image/"));
    const file = item?.getAsFile();
    if (!file) return;
    e.preventDefault();
    await openBytes(new Uint8Array(await file.arrayBuffer()), "the pasted image");
  }

  const FINE_PAN: Partial<Record<Action, [number, number]>> = {
    "pan-left": [-0.1, 0],
    "pan-right": [0.1, 0],
    "pan-up": [0, -0.1],
    "pan-down": [0, 0.1],
  };

  function run(action: Action) {
    const step = 0.25;
    switch (action) {
      case "freeze":
        return toggleFreeze();
      case "rotate-cw":
        return rotate(1);
      case "rotate-ccw":
        return rotate(-1);
      case "zoom-in":
        return setView(zoom(view, 1, maxMagnification));
      case "zoom-out":
        return setView(zoom(view, -1, maxMagnification));
      case "zoom-reset":
        return setView({ centre: [0.5, 0.5], magnification: 1 });
      case "pan-left":
        return setView(pan(view, -step, 0));
      case "pan-right":
        return setView(pan(view, step, 0));
      case "pan-up":
        return setView(pan(view, 0, -step));
      case "pan-down":
        return setView(pan(view, 0, step));
      case "next-mode": {
        const i = QUICK_MODES.findIndex(([m]) => m === config?.display.mode);
        return setMode(QUICK_MODES[(i + 1) % QUICK_MODES.length]![0]);
      }
      case "reading-line":
        return toggleReadingLine();
      case "fullscreen":
        return toggleFullscreen();
      case "settings":
        settingsOpen = true;
        return;
    }
  }

  function onKey(e: KeyboardEvent) {
    // Keys belong to a focused control or an open dialog; the shortcuts are for
    // the picture.
    const target = e.target as HTMLElement;
    if (settingsOpen || target.closest("input, select, textarea, button, dialog")) return;
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const action = keys.get(e.key);
    if (!action) return;
    e.preventDefault();
    // Shift with a move is a finer step of it.
    const fine = FINE_PAN[action];
    if (e.shiftKey && fine) {
      setView(pan(view, fine[0], fine[1]));
      return;
    }
    run(action);
  }

  /** Development aids (hidden flags): press keys, report the frame rate, then save the picture and quit. */
  async function runDevOptions() {
    const dev = await invoke<{ keys: string[]; snapshot_after_ms: number | null; stats: boolean }>(
      "dev_options",
    );
    if (dev.stats) {
      let last = drawn;
      setInterval(() => {
        const rate = (drawn - last) / 5;
        last = drawn;
        const times = fetchTimes.sort((a, b) => a - b);
        fetchTimes = [];
        const at = (q: number) => (times[Math.floor(q * (times.length - 1))] ?? 0).toFixed(1);
        invoke("page_log", {
          level: "info",
          message: `drew ${rate.toFixed(1)} frames/s; request to drawn ${at(0.5)} ms median, ${at(0.95)} ms 95th percentile`,
        });
      }, 5000);
    }
    if (dev.keys.length === 0 && dev.snapshot_after_ms === null) return;
    const started = performance.now();
    await new Promise<void>((ok) => (firstFrame = ok));
    for (const key of dev.keys) {
      await new Promise((ok) => setTimeout(ok, 500));
      const name = key === "Space" ? " " : key;
      canvas.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true }));
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

  /**
   * For --dev-probe: what a WebDriver test reads. `sample` takes points as
   * fractions of the view and gives, for each, the view pixel there, the colour
   * drawn at its centre on the canvas, and the engine's reference for it.
   */
  function installProbe() {
    (window as unknown as { squiglProbe: object }).squiglProbe = {
      renderer: () => (renderer instanceof Renderer ? "webgl2" : "canvas2d"),
      drawn: () => drawn,
      header: () => shown,
      async sample(points: [number, number][]) {
        const header = shown;
        const r = renderer;
        if (!header || !r) return null;
        const [vw, vh] = header.view;
        const { origin, scale } = header.placement;
        const at = points.map(([fx, fy]) => {
          const view: [number, number] = [
            Math.min(Math.floor(fx * vw), vw - 1),
            Math.min(Math.floor(fy * vh), vh - 1),
          ];
          const canvasAt: [number, number] = [
            Math.floor((view[0] + 0.5 - origin[0]) * scale),
            Math.floor((view[1] + 0.5 - origin[1]) * scale),
          ];
          return { view, canvas: canvasAt, drawn: r.pixel(...canvasAt) };
        });
        const expected = await invoke<([number, number, number] | null)[]>("dev_reference", {
          points: at.map((p) => p.view),
        });
        return at.map((p, i) => ({ ...p, expected: expected[i] }));
      },
    };
  }

  /** WebGL2 if there is one (and --dev-canvas2d is not given), else Canvas2D. */
  async function makeRenderer(): Promise<FrameRenderer> {
    const dev = await invoke<{ canvas2d: boolean; probe: boolean }>("dev_options");
    if (dev.probe) installProbe();
    if (!dev.canvas2d) {
      try {
        return new Renderer(canvas);
      } catch (e) {
        console.warn(`WebGL2 unavailable, drawing with Canvas2D: ${e}`);
      }
    }
    say("Using the slower drawing: this computer's graphics offer no WebGL2");
    return new Renderer2D(canvas);
  }

  onMount(() => {
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
    // A lost WebGL context comes back by itself if asked; the renderer is then rebuilt.
    canvas.addEventListener("webglcontextlost", (e) => {
      e.preventDefault();
      say("The graphics were reset; restoring the picture…");
    });
    canvas.addEventListener("webglcontextrestored", async () => {
      renderer = new Renderer(canvas);
      if (config) {
        renderer.setSmooth(config.magnifier.smooth);
        renderer.setLut(await lut());
      }
      fetchFrame();
    });
    // A file dropped on the window: an image is shown, a recording played.
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "enter" || p.type === "over") dropping = true;
      else if (p.type === "leave") dropping = false;
      else if (p.type === "drop") {
        dropping = false;
        const path = p.paths[0];
        if (path) {
          const source = path.toLowerCase().endsWith(".sqrec")
            ? ({ kind: "replay", path } as const)
            : ({ kind: "image", path } as const);
          dispatch({ type: "use-source", source }).catch((e) => say(String(e)));
          setView({ centre: [0.5, 0.5], magnification: 1 });
        }
      }
    });
    (async () => {
      renderer = await makeRenderer();
      client = new FrameClient(await endpoint());
      await subscribe(onEvent);
      fetchFrame();
      await runDevOptions();
    })().catch((e) => (failure = String(e)));
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", resize);
      unlisten.then((f) => f());
    };
  });
</script>

<svelte:window onkeydown={onKey} onpaste={onPaste} />

<main>
  <header>
    <button onclick={toggleFreeze} aria-pressed={frozen}>
      {frozen ? "Live" : "Freeze"} <kbd>{shortcut("freeze")}</kbd>
    </button>
    <button onclick={() => rotate(-1)}>
      <span class="long">Rotate left</span><span class="short" aria-hidden="true">↺</span>
      <kbd>{shortcut("rotate-ccw")}</kbd>
    </button>
    <button onclick={() => rotate(1)}>
      <span class="long">Rotate right</span><span class="short" aria-hidden="true">↻</span>
      <kbd>{shortcut("rotate-cw")}</kbd>
    </button>
    <label>
      <span class="long">Magnification</span>
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
      <span class="long">Colours</span>
      <select
        value={config?.display.mode ?? "normal"}
        onchange={(e) => setMode(e.currentTarget.value as DisplayMode)}
      >
        {#each MODES as [mode, label]}
          <option value={mode}>{label}</option>
        {/each}
      </select>
    </label>
    <label class="button">
      <span class="long">Open image…</span><span class="short" aria-hidden="true">Open…</span>
      <input
        bind:this={fileInput}
        type="file"
        accept="image/*"
        class="hidden"
        onchange={onFilePicked}
      />
    </label>
    {#if stream && stream.source.kind !== "phone"}
      <button onclick={() => dispatch({ type: "use-source", source: { kind: "phone" } })}>
        <span class="long">Use phone</span><span class="short" aria-hidden="true">Phone</span>
      </button>
    {/if}
    <button onclick={() => (settingsOpen = true)}>
      <span class="long">Settings</span><span class="short" aria-hidden="true">⚙︎</span>
      <kbd>{shortcut("settings")}</kbd>
    </button>
    <button onclick={toggleFullscreen}>
      <span class="long">Full screen</span><span class="short" aria-hidden="true">⛶</span>
      <kbd>{shortcut("fullscreen")}</kbd>
    </button>
  </header>

  <div class="picture">
    <canvas bind:this={canvas} tabindex="0" aria-label="Magnified camera picture"></canvas>
    {#if config?.magnifier.reading_line}
      <div class="reading-line" aria-hidden="true"></div>
    {/if}
    {#if stream?.status.state === "waiting" && stream.status.problem}
      <Connection
        problem={stream.status.problem}
        reason={stream.status.reason}
        onopen={() => fileInput.click()}
      />
    {/if}
    {#if dropping}
      <div class="drop" aria-hidden="true">Drop an image or a recording to open it</div>
    {/if}
  </div>

  <footer>
    <span>{statusText}</span>
    <span role="status" aria-live="polite">{notice}</span>
  </footer>

  {#if failure}
    <p class="failure" role="alert">{failure}</p>
  {/if}
</main>

{#if config}
  <Settings {config} bind:open={settingsOpen} onnotice={say} />
{/if}

<style>
  /* Themes: every colour comes from these. */
  :global(:root) {
    --bg: #000;
    --panel: #161616;
    --text: #fff;
    --control: #262626;
    --edge: #8a8a8a;
    --focus: #ffd400;
    --line: #ffd400;
  }
  :global(:root[data-theme="light"]) {
    --bg: #fff;
    --panel: #f2f2f2;
    --text: #000;
    --control: #fff;
    --edge: #555;
    --focus: #0050c8;
    --line: #c00000;
  }
  :global(:root[data-theme="high-contrast-yellow"]) {
    --bg: #000;
    --panel: #000;
    --text: #ffff00;
    --control: #000;
    --edge: #ffff00;
    --focus: #00ffff;
    --line: #ffff00;
  }
  :global(:root[data-theme="high-contrast-white"]) {
    --bg: #000;
    --panel: #000;
    --text: #fff;
    --control: #000;
    --edge: #fff;
    --focus: #ffff00;
    --line: #fff;
  }
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: var(--bg);
    color: var(--text);
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
    background: var(--panel);
  }
  footer {
    justify-content: space-between;
  }
  :global(button, select, input) {
    font: inherit;
    color: var(--text);
  }
  :global(button),
  :global(select),
  .button {
    padding: 0.35rem 0.75rem;
    border: 0.15rem solid var(--edge);
    border-radius: 0.35rem;
    background: var(--control);
    color: var(--text);
    cursor: pointer;
  }
  /* WebKitGTK draws a native select in the GTK theme's colours, whatever these
     say (white on light grey), so it draws its own, arrow and all. */
  :global(select) {
    appearance: none;
    padding-right: 1.8em;
    background-image:
      linear-gradient(45deg, transparent 50%, currentColor 50%),
      linear-gradient(135deg, currentColor 50%, transparent 50%);
    background-position:
      calc(100% - 1.1em) 55%,
      calc(100% - 0.75em) 55%;
    background-size: 0.35em 0.35em;
    background-repeat: no-repeat;
  }
  :global(select option) {
    background: var(--control);
    color: var(--text);
  }
  .button:focus-within {
    outline: 0.2rem solid var(--focus);
    outline-offset: 0.15rem;
  }
  kbd {
    font-size: 0.8em;
    opacity: 0.85;
  }
  kbd:empty {
    display: none;
  }
  :global(:focus-visible) {
    outline: 0.2rem solid var(--focus);
    outline-offset: 0.15rem;
  }
  .picture {
    position: relative;
    min-height: 0;
    background: #000;
  }
  canvas {
    width: 100%;
    height: 100%;
    display: block;
  }
  .reading-line {
    position: absolute;
    left: 0;
    right: 0;
    top: 50%;
    height: 0.25rem;
    margin-top: -0.125rem;
    background: var(--line);
    box-shadow: 0 0 0 0.1rem #000;
    pointer-events: none;
  }
  .drop {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    font-size: 2rem;
    background: rgb(0 0 0 / 0.75);
    color: #fff;
    pointer-events: none;
  }
  .hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  /* Short labels for a small window -- a small screen, or large text: the long
     ones stay for screen readers and OS magnifiers, only out of sight, so every
     control keeps its full name; the text keeps the size the person chose. */
  .short {
    display: none;
  }
  @media (max-width: 48rem), (max-height: 28rem) {
    .long {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip-path: inset(50%);
      white-space: nowrap;
    }
    .short {
      display: inline;
    }
    kbd {
      display: none;
    }
    header,
    footer {
      gap: 0.25rem 0.4rem;
      padding: 0.25rem 0.5rem;
    }
    header :global(button),
    header :global(select),
    .button {
      padding: 0.15rem 0.45rem;
    }
    header :global(select) {
      padding-right: 1.8em;
    }
    input[type="range"] {
      width: 5rem;
    }
  }
  .failure {
    position: fixed;
    inset: auto 1rem 4rem 1rem;
    margin: 0;
    padding: 1rem;
    background: #600;
    color: #fff;
    border: 0.15rem solid #f88;
  }
</style>
