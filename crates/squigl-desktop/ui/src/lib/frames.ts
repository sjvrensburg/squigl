// The frame transport's client: one WebSocket to the app (see
// crates/squigl-desktop/src/transport.rs), with up to MAX_IN_FLIGHT requests
// outstanding, answered in order.
import type { Endpoint, Rotation } from "./engine";

export interface Viewport {
  /** The drawing area in CSS pixels, and device pixels per CSS pixel. */
  width: number;
  height: number;
  dpr: number;
  /** The view-space point at the area's centre, as fractions of the view. */
  centre: [number, number];
  magnification: number;
}

export type PlaneFormat = "luma" | "yuv420";

export interface Crop {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface FrameHeader {
  seq: number;
  source_width: number;
  source_height: number;
  region: Crop;
  step: number;
  rotation: Rotation;
  format: PlaneFormat;
  width: number;
  height: number;
  view_region: Crop;
  view: [number, number];
  placement: { origin: [number, number]; scale: number };
}

export interface Frame {
  header: FrameHeader;
  y: Uint8Array;
  /** Empty for the luma format. */
  u: Uint8Array;
  v: Uint8Array;
}

/** Splits a binary reply: header length (u32 LE), header JSON, then Y, U, V. */
export function parseFrame(buf: ArrayBuffer): Frame {
  const headerLen = new DataView(buf).getUint32(0, true);
  const header: FrameHeader = JSON.parse(
    new TextDecoder().decode(new Uint8Array(buf, 4, headerLen)),
  );
  const { width: w, height: h, format } = header;
  const ySize = w * h;
  const cSize = format === "yuv420" ? Math.ceil(w / 2) * Math.ceil(h / 2) : 0;
  let at = 4 + headerLen;
  const y = new Uint8Array(buf, at, ySize);
  at += ySize;
  const u = new Uint8Array(buf, at, cSize);
  at += cSize;
  const v = new Uint8Array(buf, at, cSize);
  return { header, y, u, v };
}

/**
 * Requests outstanding at once. One leaves the socket idle while the webview takes
 * in a reply (about 40 ms for 3 MB in WebKitGTK on the Ubuntu box); with two, the
 * app builds the next frame meanwhile.
 */
export const MAX_IN_FLIGHT = 2;

export class FrameClient {
  private ws: WebSocket;
  private waiting: ((reply: ArrayBuffer | string) => void)[] = [];
  readonly ready: Promise<void>;

  constructor(endpoint: Endpoint) {
    this.ws = new WebSocket(`ws://127.0.0.1:${endpoint.port}/?token=${endpoint.token}`);
    this.ws.binaryType = "arraybuffer";
    this.ws.onmessage = (ev) => this.waiting.shift()?.(ev.data);
    this.ready = new Promise((ok, fail) => {
      this.ws.onopen = () => ok();
      this.ws.onerror = () => fail(new Error("the frame connection failed"));
    });
  }

  /** Whether another request must wait for a reply. */
  get busy(): boolean {
    return this.waiting.length >= MAX_IN_FLIGHT;
  }

  /** The planes for `viewport`, or null when there is no frame yet. */
  async request(viewport: Viewport, format: PlaneFormat): Promise<Frame | null> {
    if (this.busy) throw new Error(`at most ${MAX_IN_FLIGHT} frame requests at a time`);
    const reply = new Promise<ArrayBuffer | string>((ok) => this.waiting.push(ok));
    await this.ready;
    this.ws.send(JSON.stringify({ viewport, frame: "shown", format }));
    const data = await reply;
    return typeof data === "string" ? null : parseFrame(data);
  }
}
