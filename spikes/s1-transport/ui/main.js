// Spike S1: pull frames from Rust over the chosen transport, draw each with
// WebGL2 (luma as an R8 texture), and time it. See ../README.md.
const { invoke } = window.__TAURI__.core;
const status = document.getElementById("status");
const cfg = await invoke("setup");
const HEADER = 32;
const WARMUP_MS = 2000;

// --- transports: each returns a function giving the next frame's ArrayBuffer
function schemeTransport() {
  // WebView2 (Windows) reaches custom schemes as http://<scheme>.localhost.
  const url = navigator.userAgent.includes("Windows")
    ? "http://frame.localhost/next"
    : "frame://localhost/next";
  return async () => (await fetch(url)).arrayBuffer();
}

function ipcTransport() {
  return async () => invoke("next_frame");
}

async function wsTransport() {
  const ws = new WebSocket(`ws://127.0.0.1:${cfg.ws_port}`);
  ws.binaryType = "arraybuffer";
  const waiting = [];
  ws.onmessage = (ev) => waiting.shift()(ev.data);
  await new Promise((ok, fail) => {
    ws.onopen = ok;
    ws.onerror = fail;
  });
  return () =>
    new Promise((ok) => {
      waiting.push(ok);
      ws.send("n");
    });
}

const next = await {
  scheme: schemeTransport,
  ipc: ipcTransport,
  ws: wsTransport,
}[cfg.transport]();

// --- WebGL2: one R8 texture the size of the frame, drawn as grey
const canvas = document.getElementById("view");
const gl = canvas.getContext("webgl2", { antialias: false });
if (!gl) {
  await invoke("report", { result: { transport: cfg.transport, error: "no WebGL2" } });
}
function shader(type, src) {
  const s = gl.createShader(type);
  gl.shaderSource(s, src);
  gl.compileShader(s);
  return s;
}
const prog = gl.createProgram();
gl.attachShader(prog, shader(gl.VERTEX_SHADER, `#version 300 es
  out vec2 uv;
  void main() {
    vec2 p = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    uv = vec2(p.x, p.y);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    gl_Position.y = -gl_Position.y;
  }`));
gl.attachShader(prog, shader(gl.FRAGMENT_SHADER, `#version 300 es
  precision mediump float;
  in vec2 uv;
  uniform sampler2D luma;
  out vec4 color;
  void main() { float y = texture(luma, uv).r; color = vec4(y, y, y, 1.0); }`));
gl.linkProgram(prog);
gl.useProgram(prog);
const tex = gl.createTexture();
gl.bindTexture(gl.TEXTURE_2D, tex);
gl.texStorage2D(gl.TEXTURE_2D, 1, gl.R8, cfg.width, cfg.height);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

function resize() {
  canvas.width = canvas.clientWidth * devicePixelRatio;
  canvas.height = canvas.clientHeight * devicePixelRatio;
  gl.viewport(0, 0, canvas.width, canvas.height);
}
resize();
addEventListener("resize", resize);

// --- measuring
let measuring = false;
let done = false;
const latencies = [];
let bytes = 0;
let mismatched = 0;
let lastSeq = 0n;
let outOfOrder = 0;

function draw(buf) {
  const view = new DataView(buf);
  const magic = new TextDecoder().decode(new Uint8Array(buf, 0, 8));
  const seq = view.getBigUint64(8, true);
  const sentMs = view.getFloat64(16, true);
  const w = view.getUint32(24, true);
  const h = view.getUint32(28, true);
  const plane = new Uint8Array(buf, HEADER, w * h);
  gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RED, gl.UNSIGNED_BYTE, plane);
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  gl.finish();
  const latency = Date.now() - sentMs;
  // The frame number burned into the pixels must match the header's.
  let burned = 0;
  for (let bit = 0; bit < 32; bit++) {
    if (plane[bit * 16 + 8] > 128) burned |= 1 << bit;
  }
  if (magic !== "S1FRAME1" || (burned >>> 0) !== Number(seq & 0xffffffffn)) mismatched++;
  if (seq < lastSeq) outOfOrder++;
  lastSeq = seq;
  if (measuring) {
    latencies.push(latency);
    bytes += buf.byteLength;
  }
}

// With a target rate, requests are spaced 1/fps apart (shared by the workers);
// without one (0), each worker asks again as soon as it has drawn.
const interval = cfg.fps > 0 ? 1000 / cfg.fps : 0;
let due = performance.now();
async function paced() {
  if (!interval) return;
  const wait = due - performance.now();
  due = Math.max(due, performance.now()) + interval;
  if (wait > 0) await new Promise((ok) => setTimeout(ok, wait));
}

async function worker() {
  while (!done) {
    await paced();
    draw(await next());
  }
}

status.textContent = `${cfg.transport}: ${cfg.width}x${cfg.height} (${(cfg.bytes / 1e6).toFixed(1)} MB), ${cfg.inflight} in flight, ${cfg.fps || "max"} fps`;
for (let i = 0; i < cfg.inflight; i++) worker();
await new Promise((ok) => setTimeout(ok, WARMUP_MS));
await invoke("measure_start");
measuring = true;
const t0 = performance.now();
await new Promise((ok) => setTimeout(ok, cfg.seconds * 1000));
measuring = false;
const elapsed = (performance.now() - t0) / 1000;
done = true;
latencies.sort((a, b) => a - b);
const pct = (p) => latencies[Math.min(latencies.length - 1, Math.floor(p * latencies.length))];
await invoke("report", {
  result: {
    transport: cfg.transport,
    mb: Math.round(cfg.bytes / 1e5) / 10,
    width: cfg.width,
    height: cfg.height,
    inflight: cfg.inflight,
    target_fps: cfg.fps,
    frames: latencies.length,
    fps: Math.round((latencies.length / elapsed) * 10) / 10,
    mb_per_s: Math.round(bytes / elapsed / 1e5) / 10,
    latency_ms_p50: pct(0.5),
    latency_ms_p95: pct(0.95),
    latency_ms_max: latencies[latencies.length - 1],
    mismatched,
    out_of_order: outOfOrder,
    user_agent: navigator.userAgent,
  },
});
