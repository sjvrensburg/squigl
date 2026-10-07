<script lang="ts">
  // The Reading pane: which backend reads, getting a built-in model ready (with the
  // person's agreement to the download), Read / Read all / a second opinion / Stop,
  // and the readings as large text. Words the model was unsure of are underlined --
  // dotted when it wavered, wavy when it hesitated -- and tinted, never told by
  // colour alone; what else it might have been is in the word's tooltip. Also the
  // erase brush (what the model should not read), the text size, the session's
  // history, and the readings-only view.
  import { invoke } from "@tauri-apps/api/core";
  import { dispatch, type BlocksSlice, type Config, type ReadingSlice, type SpeechSlice } from "./lib/engine";
  import { BRUSH_MAX, BRUSH_MIN } from "./lib/erase";
  import { runs, sanitize, uncertainRuns, type Part } from "./lib/math";
  import type { Action } from "./lib/shortcuts";
  import {
    describe,
    idle,
    outcome,
    plainText,
    spans,
    uncertain,
    type HistoryEntry,
    type ModelStatus,
    type ReadResult,
  } from "./lib/reading";

  let {
    reading,
    blocks,
    models,
    results,
    history,
    config,
    shortcut,
    blocksAsked,
    erase,
    speech,
    readingOnly,
    onaction,
    onbrush,
    onnotice,
  }: {
    reading: ReadingSlice | null;
    blocks: BlocksSlice | null;
    models: ModelStatus[];
    results: ReadResult[];
    history: HistoryEntry[];
    config: Config | null;
    shortcut: (action: Action) => string;
    /** Blocks were asked for: offer to get the block finder ready if it is not. */
    blocksAsked: boolean;
    /** The erase brush: on, its radius (CSS px), the strokes so far, whether there is a box to erase. */
    erase: { on: boolean; brush: number; strokes: number; box: boolean };
    speech: SpeechSlice | null;
    readingOnly: boolean;
    onaction: (action: Action) => void;
    onbrush: (radius: number) => void;
    onnotice: (text: string) => void;
  } = $props();

  const ui = $derived(config?.ui ?? { steady_threshold: 0.92, wavering_threshold: 0.6, reading_size: 20, scale: 1 });
  const backend = $derived(reading?.backends[reading.selected] ?? null);
  // The selected backend's built-in model, if it is one.
  const model = $derived(models.find((m) => m.kind === "transcriber" && m.name === backend) ?? null);
  const ready = $derived(!model || model.phase.phase === "ready");
  const detector = $derived(models.find((m) => m.kind === "detector") ?? null);
  // The models to offer to get ready: the selected reader's, and the block finder's
  // once blocks were asked for.
  const preparing = $derived(
    [
      model && !ready ? { m: model, role: "Reading" } : null,
      detector && blocksAsked && detector.phase.phase !== "ready" ? { m: detector, role: "Finding blocks" } : null,
    ].filter((x) => x !== null),
  );
  const busy = $derived(reading?.reading != null);
  const what = $derived(
    reading?.mode === "crop" ? "Read the box" : reading?.mode === "formula" ? "Read the formula" : "Read the page",
  );
  // Newest first, unless they are a page's blocks in order; each with its place in
  // the list, which is how the engine names it.
  const ordered = $derived(
    (reading?.in_page_order ? results.map((r, i) => [r, i] as const) : results.map((r, i) => [r, i] as const).reverse()),
  );
  const speaking = $derived(speech?.speaking ?? null);
  const aloud = $derived(speaking !== null || (speech?.following ?? false));

  // The reading being said stays in sight.
  let list = $state<HTMLOListElement>();
  $effect(() => {
    if (speaking === null || !list) return;
    list.querySelector(`[data-result="${speaking.result}"]`)?.scrollIntoView({ block: "nearest" });
  });

  // Seconds the read in flight has taken, ticking while there is one.
  let since = $state<number | null>(null);
  let now = $state(Date.now());
  $effect(() => {
    if (!busy) {
      since = null;
      return;
    }
    since ??= Date.now();
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  async function run(command: Parameters<typeof dispatch>[0]) {
    try {
      await dispatch(command);
    } catch (e) {
      onnotice(String(e));
    }
  }

  async function copy(text: string, what: string) {
    try {
      await navigator.clipboard.writeText(text);
      onnotice(`${what} copied`);
    } catch {
      onnotice(`Could not copy ${what.toLowerCase()}`);
    }
  }

  async function saveReadings() {
    run({ type: "save-history", dir: await invoke<string>("save_dir") });
  }

  // The reading text size, in points, as the settings keep it.
  const SIZE_MIN = 10;
  const SIZE_MAX = 60;
  function textSize(delta: number) {
    if (!config) return;
    const size = Math.min(SIZE_MAX, Math.max(SIZE_MIN, ui.reading_size + delta));
    if (size === ui.reading_size) return;
    run({ type: "set-config", config: { ...config, ui: { ...config.ui, reading_size: size } } });
    onnotice(`Reading text ${size} points`);
  }

  // Each reading's text and maths runs, from the engine, as they arrive.
  let partsOf = $state<Record<string, Part[]>>({});
  const asked = new Set<string>();
  function parts(text: string): Part[] | null {
    const p = partsOf[text];
    if (!p && !asked.has(text)) {
      asked.add(text);
      invoke<Part[]>("math_parts", { text })
        .then((p) => (partsOf[text] = p))
        .catch((e) => onnotice(`Could not lay out the maths: ${e}`));
    }
    return p ?? null;
  }

  /** Puts MathML into `node`, rebuilt from MathML elements alone. */
  function mathml(node: HTMLElement, src: string) {
    const put = (s: string) => {
      const m = sanitize(s);
      node.replaceChildren(...(m ? [m] : []));
    };
    put(src);
    return { update: put };
  }

  const time = (at: string) => new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

  function seconds(r: ReadResult): string {
    const o = outcome(r);
    return "ok" in o ? `${o.ok.elapsed.secs + Math.round(o.ok.elapsed.nanos / 1e8) / 10} s` : "";
  }
</script>

<aside class="reading" aria-labelledby="reading-title" style:--reading-size="{ui.reading_size * 1.5}px">
  <div class="title">
    <h2 id="reading-title">Reading</h2>
    <div class="controls tight">
      <button
        aria-label="Smaller reading text"
        disabled={!config || ui.reading_size <= SIZE_MIN}
        onclick={() => textSize(-2)}>A−</button
      >
      <button
        aria-label="Larger reading text"
        disabled={!config || ui.reading_size >= SIZE_MAX}
        onclick={() => textSize(2)}>A+</button
      >
      <button aria-pressed={readingOnly} onclick={() => onaction("reading-only")}>
        {readingOnly ? "Back to the picture" : "Readings only"} <kbd>{shortcut("reading-only")}</kbd>
      </button>
    </div>
  </div>

  {#if !reading || reading.backends.length === 0}
    <p>No way to read is set up. Add a model or a server in the settings file.</p>
  {:else}
    <div class="controls">
      <label>
        Read with
        <select
          value={reading.selected}
          onchange={(e) => run({ type: "select-backend", index: Number(e.currentTarget.value) })}
        >
          {#each reading.backends as name, i}
            <option value={i}>{name}</option>
          {/each}
        </select>
      </label>
    </div>

    {#each preparing as { m, role }}
      <div class="model" role="group" aria-label="{role}: {m.name}">
        <p>{role}: {m.name}. {describe(m.phase)}</p>
        {#if m.phase.phase === "downloading"}
          <progress value={m.phase.done} max={m.phase.total}></progress>
        {/if}
        {#if idle(m.phase)}
          <p class="note">It runs on this computer: nothing you read leaves it.</p>
          <button onclick={() => run({ type: "prepare-model", name: m.name })}>
            {m.phase.phase === "not-installed" ? "Download it" : "Get it ready"}
          </button>
        {:else if m.phase.phase === "downloading"}
          <button onclick={() => run({ type: "cancel-model-download", name: m.name })}>
            Stop the download
          </button>
        {/if}
      </div>
    {/each}

    <div class="controls">
      <button disabled={busy || !ready} onclick={() => run({ type: "read" })}>
        {what} <kbd>{shortcut("read")}</kbd>
      </button>
      {#if blocks?.enabled && blocks.blocks.length > 0}
        <button disabled={busy || !ready} onclick={() => run({ type: "read-all" })}>
          Read all blocks <kbd>{shortcut("read-all")}</kbd>
        </button>
      {/if}
      {#if reading.backends.length > 1}
        <button disabled={busy} onclick={() => run({ type: "second-opinion" })}>
          Second opinion <kbd>{shortcut("second-opinion")}</kbd>
        </button>
      {/if}
      {#if busy}
        <button onclick={() => run({ type: "cancel-read" })}>Stop</button>
      {/if}
    </div>

    {#if !readingOnly}
      <div class="controls" role="group" aria-label="Erasing">
        <button aria-pressed={erase.on} onclick={() => onaction("erase-brush")}>
          Erase brush <kbd>{shortcut("erase-brush")}</kbd>
        </button>
        {#if erase.on}
          <label>
            Size
            <input
              type="range"
              min={BRUSH_MIN}
              max={BRUSH_MAX}
              step="2"
              value={erase.brush}
              oninput={(e) => onbrush(Number(e.currentTarget.value))}
            />
          </label>
        {/if}
        {#if erase.box}
          <button onclick={() => onaction("erase-box")}>Erase the box <kbd>{shortcut("erase-box")}</kbd></button>
        {/if}
        {#if erase.strokes > 0}
          <button onclick={() => onaction("undo-erase")}>Undo erasing <kbd>{shortcut("undo-erase")}</kbd></button>
          <button onclick={() => run({ type: "set-erasures", strokes: [] })}>Erase nothing</button>
        {/if}
      </div>
    {/if}

    {#if speech?.available}
      <div class="controls" role="group" aria-label="Reading aloud">
        <button disabled={results.length === 0} onclick={() => run({ type: "speak" })}>
          Read aloud <kbd>{shortcut("speak")}</kbd>
        </button>
        {#if detector}
          <button disabled={busy} onclick={() => run({ type: "read-aloud" })}>
            Read the page aloud <kbd>{shortcut("read-aloud")}</kbd>
          </button>
        {/if}
        {#if aloud}
          <button onclick={() => onaction("pause-speech")} aria-pressed={speech?.paused ?? false}>
            {speech?.paused ? "Go on" : "Pause"} <kbd>{shortcut("pause-speech")}</kbd>
          </button>
          <button onclick={() => onaction("stop-speech")}>Stop reading aloud <kbd>{shortcut("stop-speech")}</kbd></button>
          {#if results.length > 1}
            <button onclick={() => onaction("previous-spoken")} aria-label="Read aloud from the previous one"
              >Previous <kbd>{shortcut("previous-spoken")}</kbd></button
            >
            <button onclick={() => onaction("next-spoken")} aria-label="Read aloud from the next one"
              >Next <kbd>{shortcut("next-spoken")}</kbd></button
            >
          {/if}
        {/if}
      </div>
    {/if}

    <p role="status" class="status">
      {#if reading.reading}
        Reading{reading.reading.label ? ` ${reading.reading.label}` : ""} with {reading.reading.backend}…
        {#if since !== null}{Math.max(0, Math.round((now - since) / 1000))} s{/if}
        {#if reading.reading.queued > 0}({reading.reading.queued} more to read){/if}
      {/if}
    </p>

    {#if results.length > 0}
      <div class="controls">
        <button onclick={() => copy(ordered.map(([r]) => plainText(r)).join("\n\n"), "Readings")}>Copy all</button>
        <button onclick={() => run({ type: "clear-results" })}>Clear</button>
        <button onclick={saveReadings}>Save readings</button>
      </div>
    {/if}

    <ol class="results" aria-label="Readings" bind:this={list}>
      {#each ordered as [r, index]}
        {@const o = outcome(r)}
        {@const said = speaking?.result === index ? speaking : null}
        <li data-result={index} class:speaking={said !== null}>
          <div class="meta">
            {#if r.label}<strong>{r.label}</strong>{/if}
            {#if "ok" in o}
              <span>{o.ok.backend}, {seconds(r)}</span>
            {/if}
            <button class="small" onclick={() => copy(plainText(r), "Reading")}>Copy</button>
            {#if speech?.available}
              <button class="small" onclick={() => run({ type: "speak", result: index })}>Read aloud</button>
            {/if}
            {#if said}<span class="badge">Reading aloud{speech?.paused ? " (paused)" : ""}</span>{/if}
          </div>
          {#if "error" in o}
            <p class="error">Could not read it: {o.error}</p>
          {:else if o.ok.readings.length === 0}
            <p>(no answer)</p>
          {:else}
            {#each o.ok.readings as reading, k}
              {@const laid = parts(reading.text)}
              {@const sentence: [number, number] | null = said && k === 0 ? [said.start, said.end] : null}
              {@const shown = laid ? runs(reading, ui, laid, sentence) : [{ kind: "text" as const, spans: spans(reading, ui) }]}
              {@const unsure = laid ? uncertainRuns(shown) : uncertain(spans(reading, ui))}
              {#if o.ok.samples > 1}<p class="support">{reading.count} of {o.ok.samples} readings:</p>{/if}
              <p class="text">
                {#each shown as run}{#if run.kind === "text"}{#each run.spans as s}{#if s.confidence === "steady" && !s.current}{s.text}{:else}<span
                          class={[s.confidence !== "steady" && s.confidence, s.current && "current"]}
                          title={s.alternates.length && s.confidence !== "steady" ? `or: ${s.alternates.join(", ")}` : undefined}
                        >{s.text}</span>{/if}{/each}{:else if run.mathml}<span
                      class={["math", run.display && "display", run.confidence !== "steady" && run.confidence, run.current && "current"]}
                      title={run.alternates.length ? `or: ${run.alternates.join(", ")}` : undefined}
                      use:mathml={run.mathml}
                    ></span>{:else}<span class={run.confidence !== "steady" ? run.confidence : undefined}
                      >{run.source}</span
                    >{/if}{/each}
              </p>
              {#if unsure > 0 || reading.truncated}
                <p class="support">
                  {#if unsure > 0}{unsure} uncertain {unsure === 1 ? "word" : "words"} underlined.{/if}
                  {#if reading.truncated}It may be cut short.{/if}
                </p>
              {/if}
            {/each}
          {/if}
        </li>
      {/each}
    </ol>

    {#if history.length > 0}
      <details class="history">
        <summary>This session's readings ({history.length})</summary>
        <ol aria-label="This session's readings">
          {#each [...history].reverse() as h}
            {@const text = plainText({ label: null, result: h.result })}
            <li>
              <div class="meta">
                <span>{time(h.at)}, picture {h.capture}, {h.what}</span>
                <button class="small" onclick={() => copy(text, "Reading")}>Copy</button>
              </div>
              <p class="past">{text}</p>
            </li>
          {/each}
        </ol>
      </details>
    {/if}
  {/if}
</aside>

<style>
  .reading {
    overflow-y: auto;
    padding: 0.5rem 1rem 1rem;
    background: var(--panel);
    border-left: 0.15rem solid var(--edge);
    min-width: 0;
  }
  .title {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  h2 {
    margin: 0.25rem 0 0.75rem;
    font-size: 1.25rem;
  }
  .tight {
    margin-bottom: 0.5rem;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: center;
    margin-bottom: 0.75rem;
  }
  .model {
    border: 0.15rem solid var(--edge);
    border-radius: 0.35rem;
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.75rem;
  }
  .model p {
    margin: 0.25rem 0 0.5rem;
  }
  .note {
    font-size: 0.9em;
  }
  progress {
    width: 100%;
  }
  .status:empty {
    display: none;
  }
  .results {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .results li {
    border-top: 0.1rem solid var(--edge);
    padding: 0.5rem 0;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: center;
    font-size: 0.9em;
  }
  .small {
    padding: 0.1rem 0.5rem;
  }
  .text {
    font-size: var(--reading-size);
    line-height: 1.5;
    margin: 0.25rem 0;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .support {
    font-size: 0.9em;
    margin: 0.25rem 0;
  }
  /* Reading aloud: the reading being said is edged, and its sentence boxed (not
     only tinted) so it can be followed by someone not looking for a colour. */
  .results li.speaking {
    border-left: 0.3rem solid var(--focus);
    padding-left: 0.5rem;
  }
  .badge {
    font-weight: bold;
  }
  .current {
    outline: 0.12em solid var(--focus);
    outline-offset: 0.05em;
    background: color-mix(in srgb, var(--focus) 25%, transparent);
  }
  /* Maths: a display formula is a block of its own, scrolling sideways if wide. */
  .math.display {
    display: block;
    margin: 0.25em 0;
    overflow-x: auto;
    white-space: normal;
  }
  /* A formula is marked as a whole, by a line under it rather than through it. */
  .math.wavering {
    text-decoration: none;
    border-bottom: 0.12em dotted currentColor;
  }
  .math.hesitant {
    text-decoration: none;
    border-bottom: 0.15em dashed currentColor;
  }
  /* Uncertainty: the line's style says it, the tint helps. */
  .wavering {
    text-decoration: underline dotted 0.12em;
    text-underline-offset: 0.2em;
    background: color-mix(in srgb, var(--line) 18%, transparent);
  }
  .hesitant {
    text-decoration: underline wavy 0.1em;
    text-underline-offset: 0.2em;
    background: color-mix(in srgb, var(--line) 35%, transparent);
  }
  .error {
    font-weight: bold;
  }
  .history {
    margin-top: 1rem;
    border-top: 0.15rem solid var(--edge);
    padding-top: 0.5rem;
  }
  .history summary {
    cursor: pointer;
    font-weight: bold;
  }
  .history ol {
    list-style: none;
    padding: 0;
  }
  .history li {
    border-top: 0.1rem solid var(--edge);
    padding: 0.4rem 0;
  }
  .past {
    margin: 0.25rem 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
