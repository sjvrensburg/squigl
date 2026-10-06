import { afterEach, describe, expect, it, vi } from "vitest";
import { FrameClient, MAX_IN_FLIGHT, type Viewport } from "./frames";

/** A WebSocket that opens at once and keeps what is sent. */
class FakeSocket {
  static last: FakeSocket;
  sent: string[] = [];
  binaryType = "";
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((ev: { data: ArrayBuffer | string }) => void) | null = null;
  constructor() {
    FakeSocket.last = this;
    queueMicrotask(() => this.onopen?.());
  }
  send(text: string) {
    this.sent.push(text);
  }
  reply(data: ArrayBuffer | string) {
    this.onmessage?.({ data });
  }
}

/** A reply carrying a 2x2 luma frame numbered `seq`. */
function frameReply(seq: number): ArrayBuffer {
  const header = new TextEncoder().encode(
    JSON.stringify({ seq, width: 2, height: 2, format: "luma" }),
  );
  const out = new Uint8Array(4 + header.length + 4);
  new DataView(out.buffer).setUint32(0, header.length, true);
  out.set(header, 4);
  return out.buffer;
}

const viewport: Viewport = { width: 100, height: 100, dpr: 1, centre: [0.5, 0.5], magnification: 1 };

describe("FrameClient", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("keeps two requests out at once, answered in order", async () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const client = new FrameClient({ port: 1, token: "t" });
    const first = client.request(viewport, "luma");
    const second = client.request(viewport, "luma");
    expect(MAX_IN_FLIGHT).toBe(2);
    expect(client.busy).toBe(true);
    await expect(client.request(viewport, "luma")).rejects.toThrow();
    await client.ready;
    await Promise.resolve();
    expect(FakeSocket.last.sent).toHaveLength(2);
    FakeSocket.last.reply(frameReply(7));
    FakeSocket.last.reply('{"none":true}');
    expect((await first)?.header.seq).toBe(7);
    expect(await second).toBeNull();
    expect(client.busy).toBe(false);
  });
});
