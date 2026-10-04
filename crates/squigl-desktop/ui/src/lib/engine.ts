// The engine, as the page sees it: the commands it can send, the events it gets,
// and the shapes of both. These mirror squigl_engine's serde output by hand (only
// the fields the page reads); keep them in step with crates/squigl-engine/src/engine.rs.
import { Channel, invoke } from "@tauri-apps/api/core";

export type Rotation = "none" | "cw90" | "cw180" | "cw270";
export type Facing = "back" | "front";
export type SourceSpec =
  | { kind: "phone" }
  | { kind: "replay"; path: string }
  | { kind: "image"; path: string }
  | { kind: "test-pattern" };

export type DisplayMode =
  | "normal"
  | "grey"
  | "inverted"
  | "yellow-on-black"
  | "white-on-black"
  | "black-on-yellow"
  | "custom";

export interface DisplayConfig {
  mode: DisplayMode;
  ink: [number, number, number];
  paper: [number, number, number];
  contrast: number;
  brightness: number;
  gamma: number;
  threshold: number | null;
}

export interface MagnifierConfig {
  magnification: number;
  max_magnification: number;
  smooth: boolean;
  reading_line: boolean;
}

/** The settings file; the page edits only these sections and sends the rest back as it came. */
export interface Config {
  display: DisplayConfig;
  magnifier: MagnifierConfig;
  [section: string]: unknown;
}

export type StreamStatus =
  | { state: "connecting" }
  | { state: "streaming"; width: number; height: number }
  | { state: "waiting"; reason: string; retry_in_ms: number }
  | { state: "stopped" };

export interface StreamSlice {
  source: SourceSpec;
  status: StreamStatus;
  capabilities: { zoom: boolean; torch: boolean; facing: boolean };
  facing: Facing;
  zoom: number;
  zoom_applied: number;
  zoom_range: [number, number] | null;
  torch: boolean;
}

export interface CaptureSlice {
  frozen: { seq: number; width: number; height: number } | null;
  rotation: Rotation;
  view: [number, number] | null;
}

export interface Versioned<T> {
  version: number;
  value: T;
}

export interface Notice {
  text: string;
  level: "info" | "warning" | "error";
}

export type Event =
  | { type: "stream"; data: Versioned<StreamSlice> }
  | { type: "capture"; data: Versioned<CaptureSlice> }
  | { type: "blocks"; data: Versioned<unknown> }
  | { type: "reading"; data: Versioned<unknown> }
  | { type: "models"; data: Versioned<unknown> }
  | { type: "config"; data: Versioned<Config> }
  | { type: "frame"; data: { seq: number } }
  | { type: "result-appended" | "results-cleared" | "history-appended"; data?: unknown }
  | { type: "notice"; data: Notice };

export type Command =
  | { type: "freeze" }
  | { type: "live" }
  | { type: "set-rotation"; rotation: Rotation }
  | { type: "use-source"; source: SourceSpec }
  | { type: "set-facing"; facing: Facing }
  | { type: "set-zoom"; zoom: number }
  | { type: "step-zoom"; steps: number }
  | { type: "set-torch"; on: boolean }
  | { type: "reconnect" }
  | { type: "set-config"; config: Config }
  | { type: "prepare-model"; name: string }
  | { type: "cancel-model-download"; name: string };

export type Reply = "done" | "unchanged";

export function dispatch(command: Command): Promise<Reply> {
  return invoke("dispatch", { command });
}

/** Calls `onEvent` with every slice now, then with every event as it happens. */
export async function subscribe(onEvent: (event: Event) => void): Promise<void> {
  const channel = new Channel<Event>();
  channel.onmessage = onEvent;
  await invoke("subscribe", { channel });
}

export interface Endpoint {
  port: number;
  token: string;
}

export function endpoint(): Promise<Endpoint> {
  return invoke("endpoint");
}

export interface LutReply {
  kind: "tone" | "luma";
  rgba: number[];
}

export function lut(): Promise<LutReply> {
  return invoke("lut");
}
