// Draws frames with WebGL2: the planes as R8 textures, converted from YUV with the
// coefficients of squigl_core::convert (BT.601, limited range), turned by the
// header's rotation, placed where its placement says, and mapped through the
// display mode's table (squigl_engine::display).
import type { LutReply } from "./engine";
import type { Frame } from "./frames";
import { cornerUvs } from "./viewport";

const VERTEX = `#version 300 es
in vec2 pos;
in vec2 texCoord;
out vec2 uv;
void main() {
  uv = texCoord;
  gl_Position = vec4(pos, 0.0, 1.0);
}`;

const FRAGMENT = `#version 300 es
precision highp float;
in vec2 uv;
uniform sampler2D yTex;
uniform sampler2D uTex;
uniform sampler2D vTex;
uniform sampler2D lut;
uniform bool lumaMode;   // the table maps the Y byte to a colour
uniform bool hasChroma;
out vec4 color;

float lookup(float v, int channel) {
  return texelFetch(lut, ivec2(int(clamp(v, 0.0, 1.0) * 255.0 + 0.5), 0), 0)[channel];
}

void main() {
  float y = texture(yTex, uv).r;
  if (lumaMode) {
    color = vec4(texelFetch(lut, ivec2(int(y * 255.0 + 0.5), 0), 0).rgb, 1.0);
    return;
  }
  float u = hasChroma ? texture(uTex, uv).r : 128.0 / 255.0;
  float v = hasChroma ? texture(vTex, uv).r : 128.0 / 255.0;
  float c = 1.164 * (y * 255.0 - 16.0);
  float d = u * 255.0 - 128.0;
  float e = v * 255.0 - 128.0;
  vec3 rgb = clamp(vec3(c + 1.596 * e, c - 0.391 * d - 0.813 * e, c + 2.018 * d) / 255.0, 0.0, 1.0);
  color = vec4(lookup(rgb.r, 0), lookup(rgb.g, 1), lookup(rgb.b, 2), 1.0);
}`;

function compile(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const s = gl.createShader(type)!;
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
    throw new Error(`shader: ${gl.getShaderInfoLog(s)}`);
  }
  return s;
}

/** An R8 texture whose size follows what is uploaded. */
class Plane {
  readonly tex: WebGLTexture;
  private size: [number, number] = [0, 0];

  constructor(private gl: WebGL2RenderingContext) {
    this.tex = gl.createTexture()!;
  }

  upload(data: Uint8Array, w: number, h: number) {
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    if (this.size[0] !== w || this.size[1] !== h) {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, w, h, 0, gl.RED, gl.UNSIGNED_BYTE, data);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      this.size = [w, h];
    } else {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RED, gl.UNSIGNED_BYTE, data);
    }
  }

  filter(smooth: boolean) {
    const gl = this.gl;
    const f = smooth ? gl.LINEAR : gl.NEAREST;
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, f);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, f);
  }
}

/** What draws frames: WebGL2 ([`Renderer`]) or, without it, Canvas2D. */
export interface FrameRenderer {
  readonly wantsLumaOnly: boolean;
  setLut(table: LutReply): void;
  setSmooth(smooth: boolean): void;
  show(frame: Frame): void;
  draw(): void;
}

/** The screen rectangle (device pixels) a frame's region is drawn into. */
export function screenRect(frame: Frame): { left: number; top: number; width: number; height: number } {
  const { view_region: r, placement } = frame.header;
  const s = placement.scale;
  return {
    left: (r.x - placement.origin[0]) * s,
    top: (r.y - placement.origin[1]) * s,
    width: r.w * s,
    height: r.h * s,
  };
}

export class Renderer implements FrameRenderer {
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private planes: [Plane, Plane, Plane];
  private lutTex: WebGLTexture;
  private lumaMode = false;
  private buffer: WebGLBuffer;
  private frame: Frame | null = null;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", { antialias: false, alpha: false });
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
    this.prog = gl.createProgram()!;
    gl.attachShader(this.prog, compile(gl, gl.VERTEX_SHADER, VERTEX));
    gl.attachShader(this.prog, compile(gl, gl.FRAGMENT_SHADER, FRAGMENT));
    gl.linkProgram(this.prog);
    if (!gl.getProgramParameter(this.prog, gl.LINK_STATUS)) {
      throw new Error(`program: ${gl.getProgramInfoLog(this.prog)}`);
    }
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    this.planes = [new Plane(gl), new Plane(gl), new Plane(gl)];
    this.lutTex = gl.createTexture()!;
    this.buffer = gl.createBuffer()!;
    this.setLut({ kind: "tone", rgba: Array.from({ length: 1024 }, (_, i) => (i % 4 === 3 ? 255 : i >> 2)) });
    this.setSmooth(true);
  }

  setLut(table: LutReply) {
    const gl = this.gl;
    this.lumaMode = table.kind === "luma";
    gl.bindTexture(gl.TEXTURE_2D, this.lutTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 256, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(table.rgba));
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    this.draw();
  }

  get wantsLumaOnly(): boolean {
    return this.lumaMode;
  }

  setSmooth(smooth: boolean) {
    for (const p of this.planes) p.filter(smooth);
    this.draw();
  }

  /** Takes a new frame and draws it. */
  show(frame: Frame) {
    const { width: w, height: h, format } = frame.header;
    this.planes[0].upload(frame.y, w, h);
    if (format === "yuv420") {
      const [cw, ch] = [Math.ceil(w / 2), Math.ceil(h / 2)];
      this.planes[1].upload(frame.u, cw, ch);
      this.planes[2].upload(frame.v, cw, ch);
    }
    this.frame = frame;
    this.draw();
  }

  /** Redraws the last frame (after a resize or a table change). */
  draw() {
    const gl = this.gl;
    const { canvas } = this;
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    const frame = this.frame;
    if (!frame) return;
    const { rotation, format } = frame.header;
    // The region's corners in device pixels, then in clip space.
    const { left, top, width, height } = screenRect(frame);
    const right = left + width;
    const bottom = top + height;
    const clipX = (x: number) => (x / canvas.width) * 2 - 1;
    const clipY = (y: number) => 1 - (y / canvas.height) * 2;
    const corners = [
      [left, top],
      [right, top],
      [left, bottom],
      [right, bottom],
    ];
    const uvs = cornerUvs(rotation);
    const data = new Float32Array(16);
    corners.forEach(([x, y], i) => {
      data.set([clipX(x!), clipY(y!), uvs[i]![0], uvs[i]![1]], i * 4);
    });
    gl.useProgram(this.prog);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.bufferData(gl.ARRAY_BUFFER, data, gl.DYNAMIC_DRAW);
    const pos = gl.getAttribLocation(this.prog, "pos");
    const tc = gl.getAttribLocation(this.prog, "texCoord");
    gl.enableVertexAttribArray(pos);
    gl.vertexAttribPointer(pos, 2, gl.FLOAT, false, 16, 0);
    gl.enableVertexAttribArray(tc);
    gl.vertexAttribPointer(tc, 2, gl.FLOAT, false, 16, 8);
    const names = ["yTex", "uTex", "vTex"];
    this.planes.forEach((p, i) => {
      gl.activeTexture(gl.TEXTURE0 + i);
      gl.bindTexture(gl.TEXTURE_2D, p.tex);
      gl.uniform1i(gl.getUniformLocation(this.prog, names[i]!), i);
    });
    gl.activeTexture(gl.TEXTURE3);
    gl.bindTexture(gl.TEXTURE_2D, this.lutTex);
    gl.uniform1i(gl.getUniformLocation(this.prog, "lut"), 3);
    gl.uniform1i(gl.getUniformLocation(this.prog, "lumaMode"), this.lumaMode ? 1 : 0);
    gl.uniform1i(gl.getUniformLocation(this.prog, "hasChroma"), format === "yuv420" ? 1 : 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }
}
