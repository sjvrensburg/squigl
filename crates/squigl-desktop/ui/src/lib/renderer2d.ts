// The fallback when WebGL2 is not there: the same picture, converted on the CPU
// (the coefficients and table of renderer.ts) and placed with the canvas
// transform. Slower -- every pixel passes through JavaScript -- so it asks for luma
// only whenever the display mode allows.
import type { LutReply } from "./engine";
import type { Frame } from "./frames";
import { screenRect, type FrameRenderer } from "./renderer";

const QUARTER: Record<string, number> = { none: 0, cw90: 1, cw180: 2, cw270: 3 };

export class Renderer2D implements FrameRenderer {
  private ctx: CanvasRenderingContext2D;
  private image = document.createElement("canvas");
  private lut: LutReply = {
    kind: "tone",
    rgba: Array.from({ length: 1024 }, (_, i) => (i % 4 === 3 ? 255 : i >> 2)),
  };
  private smooth = true;
  private frame: Frame | null = null;

  constructor(private canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("no 2D canvas either");
    this.ctx = ctx;
  }

  get wantsLumaOnly(): boolean {
    return this.lut.kind === "luma";
  }

  setLut(table: LutReply) {
    this.lut = table;
    if (this.frame) this.show(this.frame);
  }

  setSmooth(smooth: boolean) {
    this.smooth = smooth;
    this.draw();
  }

  show(frame: Frame) {
    const { width: w, height: h, format } = frame.header;
    this.image.width = w;
    this.image.height = h;
    const ictx = this.image.getContext("2d")!;
    const out = ictx.createImageData(w, h);
    const px = out.data;
    const t = this.lut.rgba;
    const luma = this.lut.kind === "luma";
    const cw = Math.ceil(w / 2);
    for (let row = 0; row < h; row++) {
      for (let col = 0; col < w; col++) {
        const i = row * w + col;
        const o = i * 4;
        const y = frame.y[i]!;
        if (luma) {
          px[o] = t[y * 4]!;
          px[o + 1] = t[y * 4 + 1]!;
          px[o + 2] = t[y * 4 + 2]!;
        } else {
          const ci = (row >> 1) * cw + (col >> 1);
          const d = (format === "yuv420" ? frame.u[ci]! : 128) - 128;
          const e = (format === "yuv420" ? frame.v[ci]! : 128) - 128;
          const c = 1.164 * (y - 16);
          const clamp = (v: number) => (v < 0 ? 0 : v > 255 ? 255 : Math.round(v));
          px[o] = t[clamp(c + 1.596 * e) * 4]!;
          px[o + 1] = t[clamp(c - 0.391 * d - 0.813 * e) * 4 + 1]!;
          px[o + 2] = t[clamp(c + 2.018 * d) * 4 + 2]!;
        }
        px[o + 3] = 255;
      }
    }
    ictx.putImageData(out, 0, 0);
    this.frame = frame;
    this.draw();
  }

  draw() {
    const { ctx, canvas } = this;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    if (!this.frame) return;
    const { left, top, width, height } = screenRect(this.frame);
    const turns = QUARTER[this.frame.header.rotation] ?? 0;
    // Turned about the rectangle's centre; a quarter turn swaps the drawn size.
    const [dw, dh] = turns % 2 ? [height, width] : [width, height];
    ctx.imageSmoothingEnabled = this.smooth;
    ctx.translate(left + width / 2, top + height / 2);
    ctx.rotate((turns * Math.PI) / 2);
    ctx.drawImage(this.image, -dw / 2, -dh / 2, dw, dh);
  }
}
