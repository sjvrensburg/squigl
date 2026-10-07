<script lang="ts">
  // The Reading pane: which backend reads, getting a built-in model ready (with the
  // person's agreement to the download), Read / Read all / a second opinion / Stop,
  // and the readings as large text. Words the model was unsure of are underlined --
  // dotted when it wavered, wavy when it hesitated -- and tinted, never told by
  // colour alone; what else it might have been is in the word's tooltip.
  import { invoke } from "@tauri-apps/api/core";
  import { dispatch, type BlocksSlice, type Config, type ReadingSlice } from "./lib/engine";
  import {
    describe,
    idle,
    outcome,
    plainText,
    spans,
    uncertain,
    type ModelStatus,
    type ReadResult,
    type Thresholds,
  } from "./lib/reading";

  let {
    reading,
    blocks,
    models,
    results,
    config,
    shortcut,
    blocksAsked,
    onnotice,
  }: {
    reading: ReadingSlice | null;
    blocks: BlocksSlice | null;
    models: ModelStatus[];
    results: ReadResult[];
    config: Config | null;
    shortcut: (action: "read" | "read-all" | "second-opinion") => string;
    /** Blocks were asked for: offer to get the block finder ready if it is not. */
    blocksAsked: boolean;
    onnotice: (text: string) => void;
  } = $props();

  const ui = $derived((config?.ui ?? { steady_threshold: 0.92, wavering_threshold: 0.6, reading_size: 20 }) as Thresholds & {
    reading_size: number;
  });
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
  // Newest first, unless they are a page's blocks in order.
  const ordered = $derived(reading?.in_page_order ? results : [...results].reverse());

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

  function seconds(r: ReadResult): string {
    const o = outcome(r);
    return "ok" in o ? `${o.ok.elapsed.secs + Math.round(o.ok.elapsed.nanos / 1e8) / 10} s` : "";
  }
</script>

<aside class="reading" aria-labelledby="reading-title" style:--reading-size="{ui.reading_size * 1.5}px">
  <h2 id="reading-title">Reading</h2>

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

    <p role="status" class="status">
      {#if reading.reading}
        Reading{reading.reading.label ? ` ${reading.reading.label}` : ""} with {reading.reading.backend}…
        {#if since !== null}{Math.max(0, Math.round((now - since) / 1000))} s{/if}
        {#if reading.reading.queued > 0}({reading.reading.queued} more to read){/if}
      {/if}
    </p>

    {#if results.length > 0}
      <div class="controls">
        <button onclick={() => copy(ordered.map(plainText).join("\n\n"), "Readings")}>Copy all</button>
        <button onclick={() => run({ type: "clear-results" })}>Clear</button>
        <button onclick={saveReadings}>Save readings</button>
      </div>
    {/if}

    <ol class="results" aria-label="Readings">
      {#each ordered as r}
        {@const o = outcome(r)}
        <li>
          <div class="meta">
            {#if r.label}<strong>{r.label}</strong>{/if}
            {#if "ok" in o}
              <span>{o.ok.backend}, {seconds(r)}</span>
            {/if}
            <button class="small" onclick={() => copy(plainText(r), "Reading")}>Copy</button>
          </div>
          {#if "error" in o}
            <p class="error">Could not read it: {o.error}</p>
          {:else if o.ok.readings.length === 0}
            <p>(no answer)</p>
          {:else}
            {#each o.ok.readings as reading}
              {@const parts = spans(reading, ui)}
              {@const unsure = uncertain(parts)}
              {#if o.ok.samples > 1}<p class="support">{reading.count} of {o.ok.samples} readings:</p>{/if}
              <p class="text">
                {#each parts as s}{#if s.confidence === "steady"}{s.text}{:else}<span
                      class={s.confidence}
                      title={s.alternates.length ? `or: ${s.alternates.join(", ")}` : undefined}
                    >{s.text}</span>{/if}{/each}
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
  h2 {
    margin: 0.25rem 0 0.75rem;
    font-size: 1.25rem;
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
</style>
