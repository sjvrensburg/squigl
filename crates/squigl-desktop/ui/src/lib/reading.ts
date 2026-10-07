// Readings as the page shows them: the result types (squigl_engine::engine::ReadResult
// and transcribe::Transcription, by hand), a reading split into spans by how sure the
// model was, and the built-in models' states in words.

export interface TokenAlt {
  text: string;
  prob: number;
}

export interface Token {
  text: string;
  prob: number;
  alternates: TokenAlt[];
}

export interface Reading {
  text: string;
  count: number;
  truncated: boolean;
  tokens: Token[] | null;
}

export interface Transcription {
  backend: string;
  readings: Reading[];
  silent: number;
  samples: number;
  elapsed: { secs: number; nanos: number };
}

export interface ReadResult {
  label: string | null;
  result: { Ok: Transcription } | { Err: string };
}

export type Confidence = "steady" | "wavering" | "hesitant";

export interface Thresholds {
  steady_threshold: number;
  wavering_threshold: number;
}

export function confidence(prob: number, t: Thresholds): Confidence {
  if (prob >= t.steady_threshold) return "steady";
  if (prob >= t.wavering_threshold) return "wavering";
  return "hesitant";
}

export interface Span {
  text: string;
  confidence: Confidence;
  /** What else the model thought it might be, most likely first. */
  alternates: string[];
}

/**
 * The reading as spans: its tokens, each with its confidence, when they put the
 * text back together exactly (a backend's per-token decode can drift from its
 * whole-text one, and a wrong marking is worse than none); else the text as one
 * steady span. Neighbouring tokens of the same confidence are joined.
 */
export function spans(r: Reading, t: Thresholds): Span[] {
  const tokens = r.tokens;
  if (!tokens?.length || tokens.map((k) => k.text).join("") !== r.text) {
    return [{ text: r.text, confidence: "steady", alternates: [] }];
  }
  const out: Span[] = [];
  for (const k of tokens) {
    const c = confidence(k.prob, t);
    const last = out[out.length - 1];
    if (last && c === "steady" && last.confidence === "steady") {
      last.text += k.text;
    } else {
      out.push({ text: k.text, confidence: c, alternates: k.alternates.map((a) => a.text) });
    }
  }
  return out;
}

/** How many words were uncertain, for a summary a screen reader can say. */
export function uncertain(spans: Span[]): number {
  return spans.filter((s) => s.confidence !== "steady" && s.text.trim()).length;
}

/** The result's readings, or its error. */
export function outcome(r: ReadResult): { ok: Transcription } | { error: string } {
  return "Ok" in r.result ? { ok: r.result.Ok } : { error: r.result.Err };
}

/** A result as plain text: each reading on its line (with its support when sampled). */
export function plainText(r: ReadResult): string {
  const o = outcome(r);
  if ("error" in o) return `(failed: ${o.error})`;
  const t = o.ok;
  if (t.readings.length === 0) return "(no answer)";
  if (t.samples === 1) return t.readings[0]!.text;
  return t.readings.map((x) => `[${x.count}/${t.samples}] ${x.text}`).join("\n");
}

/** A built-in model's state (squigl_engine::model::ModelPhase). */
export type ModelPhase =
  | { phase: "not-installed"; size: number }
  | { phase: "installed" }
  | { phase: "locating" }
  | { phase: "downloading"; done: number; total: number }
  | { phase: "verifying" }
  | { phase: "loading"; device: string }
  | { phase: "ready"; device: string }
  | { phase: "reloading" }
  | { phase: "failed"; error: string };

export interface ModelStatus {
  name: string;
  kind: "transcriber" | "detector";
  phase: ModelPhase;
}

/** "650 MB", "1.2 GB". */
export function size(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

/** A model's state in words. */
export function describe(p: ModelPhase): string {
  switch (p.phase) {
    case "not-installed":
      return `Not downloaded yet (${size(p.size)})`;
    case "installed":
      return "Downloaded; loads when first used";
    case "locating":
      return "Looking for its files…";
    case "downloading":
      return `Downloading: ${size(p.done)} of ${size(p.total)}`;
    case "verifying":
      return "Checking the download…";
    case "loading":
      return `Loading on the ${p.device}…`;
    case "ready":
      return `Ready on the ${p.device}`;
    case "reloading":
      return "The graphics card stopped; loading again on the processor…";
    case "failed":
      return `Could not get ready: ${p.error}`;
  }
}

/** Whether asking it to get ready would do something (a download or a load). */
export function idle(p: ModelPhase): boolean {
  return p.phase === "not-installed" || p.phase === "installed" || p.phase === "failed";
}
