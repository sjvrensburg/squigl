# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**Squigl**: a Rust workspace built around using an Android phone as a Linux document
camera with live handwriting OCR (`crates/squigl-egui`, the GUI, binary `squigl` -- this
is "the product").
It also exposes the phone as a plain V4L2 (`/dev/videoN`) webcam (`crates/squigl-cli`,
a headless CLI) and provides the shared pipeline both are built on (`crates/squigl-core`,
formerly `phone-cam4linux` -- it's the plumbing: scrcpy protocol, H.264 decode, pixel
conversion, V4L2 sink). `docs/roadmap.md` is the phased plan the workspace is being
restructured by. The on-device capture/encode is **not** reimplemented: the real upstream
`scrcpy-server.jar` is fetched at build time, embedded, and pushed to the phone over
ADB. Everything from the socket down is implemented here.

## Commands

```
cargo build --release                 # fetches scrcpy-server.jar on first build (needs network)
cargo build --release --features ffmpeg   # + system libavcodec decoder (needs full FFmpeg headers)
cargo build --release -p squigl-egui --no-default-features   # GUI without the built-in ONNX models (no ort download) or Typst
cargo test                             # unit tests (the default members: all but squigl-desktop) (protocol parser, camera listing, pixel conversion, GUI crop geometry, quad rectification)
cargo test -p squigl-core protocol::tests::parses_codec_meta   # single test
cargo test -p squigl-models --release glmocr_handwriting -- --ignored --nocapture   # GLM-OCR vs recorded readings; run after an ort bump
cargo clippy --all-targets [--features ffmpeg]
cargo fmt --all -- --check             # CI enforces this and clippy -D warnings, both feature sets
(cd crates/squigl-desktop/ui && npm ci && npm run check && npm test && npm run build)   # the desktop app's web UI, first
cargo build --release -p squigl-desktop   # the Tauri app (needs WebKitGTK 4.1 headers on Linux)
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --lib   # CI enforces this too (libraries only: the binaries' docs are --help text)
```

`squigl --rotate 270 --screenshot-after 8 --screenshot-path /tmp/gui.png` renders the
window against the phone and writes a PNG of it; add `--dev-detect [--dev-read-all]`
to start in block mode (and read every block) unattended.

Run against a phone (USB debugging authorized, Android 12+):
```
./target/release/squigl-cli --list-sizes
./target/release/squigl-cli --facing back --resolution max --bitrate 30 --device /dev/video10
```

Testing tips (no `ffmpeg` CLI needed): grab a frame from the loopback with
`gst-launch-1.0 -q v4l2src device=/dev/video10 num-buffers=1 ! videoconvert ! jpegenc ! filesink location=f.jpg`;
`RUST_LOG=debug` prints a 5 s throughput report and relays the scrcpy server's own log.
The phone's camera app must be closed (`adb shell input keyevent KEYCODE_HOME`) or the
server fails with `CAMERA_IN_USE`.

Exercise the whole V4L2 sink path with **no phone attached**:
```
./target/release/squigl-cli --test-pattern --device /dev/video10
```

Record the phone once, then run anything against the recording with **no phone**
(`--record` keeps the first connection; ~500 KB/s at `--bitrate 4`):
```
./target/release/squigl-cli --resolution 1920x1080 --bitrate 4 --record desk.sqrec   # Ctrl-C to stop
./target/release/squigl-cli --replay desk.sqrec --device /dev/video10                 # loops
./target/release/squigl --replay desk.sqrec --rotate 90 --dev-detect --dev-read-all --screenshot-after 45 --screenshot-path /tmp/gui.png
```

Exercise the WebRTC source (no ADB, no phone -- any browser on the LAN works for
testing, `--webrtc-bind 127.0.0.1` even lets you test from the same machine):
```
./target/release/squigl-cli --webrtc --webrtc-bind 127.0.0.1 --device /dev/video10
```
then open `https://127.0.0.1:8443/` and accept the self-signed certificate warning.

## Releases

`.github/workflows/release.yml` runs on a `v*` tag: builds both binaries on
ubuntu-24.04 (the prebuilt ONNX Runtime needs glibc 2.38), packages them with the
dereferenced `libwebgpu_dawn.so`, docs and `contrib/`, builds the models archive with
`squigl --fetch-model` (so it carries the in-binary checksums), and publishes a
GitHub release with `SHA256SUMS.txt`, plus the AppImage from
`contrib/appimage/build.sh` (appimagetool 1.9.1, sha256-pinned; AppDir = the two
binaries + `libwebgpu_dawn.so` in `usr/bin`, the `.desktop` and SVG icon from
`contrib/appimage/`; models are looked for in `models/` next to the AppImage via
`$APPIMAGE`). Cut one with
`git tag vX.Y.Z && git push --tags` after bumping `[workspace.package].version`.
`LICENSE` is Apache-2.0 and `NOTICE` lists third-party terms -- keep it current when a
component is added (a model, a runtime, ported code).

## Build-time network dependency

`crates/squigl-core/build.rs` downloads `scrcpy-server-v<SCRCPY_VERSION>` from GitHub
releases and verifies it against a pinned `SERVER_SHA256`, writing it to `OUT_DIR`
for `include_bytes!`. Consequences:

- A clean build needs network access. For offline/CI builds, pre-fetch the jar and
  point `SQUIGL_SERVER_JAR=/path/to/scrcpy-server.jar` at it (the old name,
  `PHONE_CAM4LINUX_SERVER_JAR`, still works).
- Bumping the scrcpy version means changing **both** `SCRCPY_VERSION` and
  `SERVER_SHA256` together, and re-checking `protocol.rs` (see below).

## Architecture

The pipeline, in data-flow order (1-6 in `crates/squigl-core/src/`, which has no
Linux-specific code; 7 in `crates/squigl-v4l2/src/`):

1. **`adb.rs`** — shells out to the `adb` binary (not a Rust ADB lib; see the
   module doc for why), found by `locate()`: `$SQUIGL_ADB`, next to the executable,
   `$ANDROID_HOME`/`$ANDROID_SDK_ROOT` `platform-tools`, then `PATH`; spawned with
   `CREATE_NO_WINDOW` on Windows (no console flashing up over the window). Pushes
   the jar, sets up `adb forward tcp:0 localabstract:scrcpy_<scid>`, and launches
   the server via `app_process`. Also `connect_tcp` / `enable_tcpip` for
   Wi-Fi (`adb connect` exits 0 even on failure -- the verdict is in its text).
2. **`session.rs`** — the orchestrator and public API (`CameraSession`, `ConnectOptions`,
   `Facing`). `connect()` starts the server with a fixed set of scrcpy options and
   completes the handshake; `connect_with_stop()`/`run(sink, stop)`; the latter is the blocking decode→sink loop
   (stop flag checked per packet / every 500 ms; `STALL_TIMEOUT` of silence →
   `Error::StreamStalled`). The server's stdout/stderr is relayed into `log`.
   **`cameras.rs`** parses the server's `list_camera_sizes=true` report.
3. **`protocol.rs`** — parses scrcpy's **undocumented** video-socket wire format
   (4-byte codec id, then 12-byte packet headers: a *session meta* packet carrying
   width/height before each encoder session, else pts/flags + size + Annex-B payload;
   flag bits 63/62/61 = session/config/keyframe). Reverse-engineered against the pinned
   server version (4.1; 3.x had a 12-byte codec header and no session packets) and
   unit-tested against hand-built fixtures. A mid-stream session meta at a new size ends
   the session with `Error::StreamResized` so the reconnect loop reopens the sink.
4. **`decode.rs`** — Annex-B → I420 via `openh264` (statically linked via `source`
   feature) or, with the `ffmpeg` feature, system libavcodec (`Backend::Ffmpeg`).
5. **`convert.rs`** — I420 → packed YUYV422 (V4L2) and → RGBA8 (whole, cropped region, or
   decimated for a preview) for on-screen display.
6. **`sink.rs`** — the `FrameSink` trait `run()` feeds (implemented by `V4l2Sink` and by any
   `FnMut(&YuvFrame) -> Result<()>` closure, which is how a GUI gets frames).
7. **`squigl-v4l2`** — the Linux-only crate (empty on other targets): `V4l2Sink`, the
   `v4l` crate's mmap output stream to `/dev/videoN`; `LazyV4l2Sink`, which opens one at
   the first frame's size (for WebRTC, which announces no size up front); and
   `loopback.rs`, which auto-loads `v4l2loopback` via `pkexec modprobe` if the device
   node is missing. The GUI depends on it only on Linux (`--device` is cfg'd out
   elsewhere).

**`replay.rs`** records a session (`CameraSession::record_to`, a tee of the packets
`run` reads) and plays it back (`Replay::run`, same shape as `CameraSession::run`,
paced by the recorded pts, optionally looping). The format is squigl's own (`SQGLREC1`
magic, size, then pts/flags/length/Annex-B per packet), not scrcpy's wire format, so
recordings outlive a scrcpy bump. The engine's stream worker plays one in place of the
phone when `StreamConfig::source` is `SourceSpec::Replay` (`squigl --replay FILE`). Its
tests encode synthetic recordings with openh264, so no binary fixture is checked in.
**`test_pattern.rs`** is the third no-phone source (colour bars, `TestPattern::run`), and
`convert::rgba_to_i420` turns a still image into a frame.

**`webrtc_source.rs`** is a second, independent camera source alongside `session.rs`,
for phones that stream over the network instead of ADB (there's no squigl-side app to
push here, unlike scrcpy-server -- the phone side is a browser page doing
`getUserMedia` and posting an SDP offer over HTTP, WHIP-style). `WebrtcSource::accept_offer`
drives `str0m` (a sans-I/O WebRTC/ICE/DTLS/SRTP implementation, chosen because it fits
this crate's synchronous style with no async runtime) to answer the offer, restricted to
H.264 only (`clear_codecs().enable_h264(true)`); `run` then blocks decoding
frames the same way `CameraSession::run` does. str0m's H.264 depacketizer already hands
back Annex-B (start-code delimited), so it feeds `decode::Decoder::decode` unchanged --
no format conversion between the two sources. LAN-only by design: a host ICE candidate
on the bound interface, no STUN/TURN, no auth.

`crates/squigl-cli` is a clap CLI over this library (`linux.rs`; on other OSes
`main.rs` only says it needs Linux, so the workspace builds everywhere); it owns the
policy bits: Ctrl-C/SIGTERM
handling, the reconnect-with-backoff loop (keeping the V4L2 sink open across sessions),
`--list-sizes` and `--resolution max`. `--webrtc` switches to the WebRTC source instead of
ADB: `webrtc_server.rs` runs a `tiny_http` HTTPS server (a fresh `rcgen` self-signed cert
per run -- `getUserMedia` needs a secure context and a LAN IP isn't CA-certifiable, so the
phone's browser shows a one-time-per-restart warning to accept) serving `webrtc_capture.html`
(the `getUserMedia` + `RTCPeerConnection` page, with `setCodecPreferences` steering the
browser to H.264 since that's all squigl decodes) at `/` and a minimal WHIP-shaped ingest
endpoint at `POST /whip` (one session at a time; a second POST while one is active gets a
503). `contrib/` has boot-time loopback config and a systemd user unit.

`crates/squigl-engine` is the window's UI-independent half -- no egui, ONNX Runtime or
Typst (`cargo tree -p squigl-engine` must show none: the built-in models and the
typesetter implement its traits from outside, so a second front end can sit on it).
`engine.rs` is its API: `Engine::new(config, stream, EngineDeps { backends, detector },
EngineOptions { eager_models, config_file }, waker)`; `handle(Command)` (serde-tagged:
freeze/live, rotation, source, facing/zoom/torch/reconnect, `SetConfig` -- which
keeps unchanged backends, rebuilds the detector on a `[layout]` change and saves --
and `PrepareModel`/`CancelModelDownload`); `pump(now)` returns the `Event`s since the
last call: every versioned slice in `EngineState` (stream, capture, blocks, reading,
models, config) whose version moved since it was last sent (commands refresh slices
too, so `pump` tracks sent versions, not "changed this time"), `Frame { seq }`, and
`Notice`s. The blocks/reading slices and the result/history events are defined but
not filled yet -- reads still run in `app.rs` (roadmap Phase 7). `render_planes`
answers a `view::ViewRequest` (from `view::Viewport::request`: the visible region and
the largest step keeping >= 1 sent pixel per device pixel) with raw planes
(`convert::i420_region_planes`, unrotated, chroma sampled where
`i420_region_to_rgba` would) and a `PlaneHeader`; `display.rs` turns `[display]`
into a 256-entry `Lut` (`Tone` per RGB channel, or `Luma` from the limited-range Y
byte to an ink-to-paper colour) with contrast/brightness/gamma/threshold baked in;
`model.rs`: `ModelPhase` (tagged, `describe()` for people), `ModelContext { eager,
notify }` the factories get, and `PhaseCell`, which coalesces notifications (every
change of kind, download progress per whole per cent or 250 ms); `config.rs`: the
`gui.toml` `Config` (moved from `transcribe.rs`; `[display]` and `[magnifier]` are
new, `every_section_round_trips` guards against one front end dropping another's
fields);
`stream.rs`: worker thread with the reconnect loop, publishing the latest `YuvFrame`
from the `SourceSpec` in `StreamConfig::source` -- `Phone` (the ADB session, per the
config's `options`/`resolution`, which are kept while another source is in use),
`Replay`, `Image` (EXIF-oriented, trimmed to even size, published once) or
`TestPattern`; `SourceSpec::capabilities()` says which camera controls exist (only
the phone has zoom, torch and facing; `set_zoom` is a no-op without), and
`Shared::use_source` switches through a restart (the 1.5 s camera-release grace only
between two phone sessions);
`geometry.rs`: `Crop` and `Selection` in *view* (rotated) coordinates, mapped back to
the source frame by `Crop::to_source`, and `zoom_to_view`; `render.rs`:
`render_region`, and `render_selection`, which paints erasures before rectifying and
enhancing, so reads, saves and both views agree, while live block detection stays
raw; `erase.rs`: the fill -- everything within the radius of the path, one flat
colour per stroke from the 75th-percentile-bright pixel of a ring around it, no
inpainting; `transcribe.rs`: the `Transcriber` trait, the OpenAI-compatible and
halo-workbench `/hint/read` backends (`BackendConfig::build` builds only these; the
front end's `BackendFactory` adds the built-in model), and the backend entries of
the config (`LocalDevice` lives here so every build reads the same file) -- the
default prompts are verbatim from halo-workbench's `handwriting.py` and travel with
each read (`Transcriber::read` takes the prompt; the hint API ignores it),
readings are grouped and counted, never merged; `layout.rs`: the `BlockDetector`
trait, `Block`/`Quad` in view space, `Role` (the 25 classes folded into
text/formula/figure/other -- colour and prompt follow it, `Mode::Formula` for a
formula block) and the perspective `rectify` (imageproc) a
non-rectangular block goes through before it is shown or read; `history.rs`: every
finished read of the session (`App::history`, appended alongside `results`, which
only ever drops its prefix -- "copy all" relies on that), Markdown export by capture;
`typeset.rs`: the `Typesetter` trait and `typeset_source` (the tint colours are the
front end's); `paths.rs`: the config, cache and pictures directories per OS via the
`directories` crate (the `~/.config/squigl`, `~/.cache/squigl` paths below are the
Linux ones, unchanged -- `linux_paths_are_unchanged` holds that).

`crates/squigl-desktop` is the Tauri 2 app that will replace the egui window
(roadmap Phase 4 on; a workspace member but not a default one, since Tauri embeds the
built web UI at compile time). `host.rs`: one thread owns the `Engine` and serves
everything through one channel -- `dispatch`ed commands, `subscribe`d event
channels (every slice on subscribing; the stream slice held to 4 Hz), plane
requests, the display table, `Stop` on exit; `transport.rs`: the frame WebSocket
(spike S1's choice) on `127.0.0.1:0`, refusing any handshake without this launch's
128-bit `?token=` or an `Origin` from the app's own webview; the page sends a
`FrameRequest` (its `view::Viewport`, which frame, which planes) and gets `u32`
header length + JSON `FrameHeader` (the `PlaneHeader`, the view-space region, the
view size and the viewport's `Placement`) + Y, U, V. The web UI (`ui/`, Svelte 5 +
TypeScript, Vite, vitest; Node 22 in CI) pulls a frame whenever something changed
(a `Frame` event while live, the capture, the config, the viewport) with at most one
request in flight, and draws it with WebGL2 (`lib/renderer.ts`: R8 planes, the
convert.rs BT.601 coefficients, rotation as corner texture coordinates, the
`display::lut` table as a 256x1 texture). `lib/engine.ts` mirrors the engine's serde
types by hand -- keep it in step. The `custom-protocol` feature (default) serves the
embedded UI; without it the window loads Vite's dev server (`npm run dev`).
Hidden flags `--dev-keys "r + m"`, `--dev-snapshot-after SECS --dev-snapshot-path
FILE` (the canvas as PNG, then quit) and `--dev-stats` (frames drawn per second, to
the log) drive it from a script; page errors and warnings go to the app's log
(target `page`). `ResizeObserver` alone does not size the canvas: WebKitGTK skips it
for a window that is not being drawn.

`crates/squigl-egui` is the egui document-camera window, on an `Engine` (eager
models; it pumps once per pass, shows `Notice`s in its status line, and reads the
capture, rotation, config and backends from the engine; erasures follow the
engine's capture number, so the engine going live on a zoom or a new source drops
them) (`app.rs`: preview, crop,
capture, save, and hand erasures (`App::erased`, view-space `erase::Stroke`s -- path
plus radius -- painted with a brush over the Zoom pane, whose size is in screen
points, each point mapped back through the rectification by `zoom_to_view`; a
retake, zoom or rotation drops them; ctrl+wheel is egui's `zoom_delta`, so it never
reaches the box-resizing wheel); `panes.rs`: which
of Preview/Zoom/Reading is active, maximised or detached (`App::pane` draws one with
its header; a detached pane is an egui immediate viewport, `App::detached_windows`,
which runs the same shortcuts as the main window while that pane's own window has
focus); `settings.rs`: the Settings window editing
a draft `Config`, applied by `App::apply_config` through `Command::SetConfig` (the
engine keeps unchanged backends and saves; the window follows the selected backend
by name); the scale is egui's zoom factor and the value
in force is authoritative -- `track_zoom` writes any change into config and draft
and saves it at once, so Save/Cancel never touch it; the enhancement knobs edit
`App::enhance`, written into the config once no pointer button is held). `crates/squigl-math` (the GUI's
`math` feature): readings typeset by Typst -- `$…$`/`$$…$$`/`\(…\)`/`\[…\]` segments converted by the `mitex`
crate and evaluated inside MiTeX's Typst scope (vendored under `assets/mitex/`, so
`\operatorname` and friends resolve), the rest escaped as markup, rasterised by
`typst-render` at the window's pixel density and cached per reading as a texture
(`app::Typeset`), laid out at the Reading pane's width once a resize settles
(`SettledWidth`; below `MIN_READING_WIDTH` the pane scrolls sideways instead); a
reading that fails to compile is shown as text. The `mitex`
crate's built-in spec predates Typst 0.15's symbol renames (`diff`→`partial`,
`sect`→`inter`, `plus.circle`→`plus.o`, …), so `modernise` rewrites its output by
the `RENAMES` table -- extend it when a renamed symbol renders wrong. A command
nothing defines (models invent `\softmax`, `\Var`) is not a failure: MiTeX's
`unknown command` makes `convert_math` retry it as `\operatorname{…}`, and a name
MiTeX passes through that Typst lacks (`unknown variable`) makes `Renderer::render`
recompile with it defined as `math.op`; `argmax`/`argmin` are defined in the
template's `compat` scope (with limits). `crates/squigl-models` (the GUI's
`local-model` feature): the built-in models --
`glmocr.rs` drives the onnx-community three-graph GLM-OCR export through `ort`
(vision encoder, embeddings, merged decoder with an explicit KV cache and the
undocumented scalar `num_logits_to_keep` input; preprocessing and MRoPE position ids
ported from oar-ocr-vl's Candle implementation), `layout.rs` is PP-DocLayoutV3
(official ONNX export: 800x800 stretched input, `[N,7]` boxes with a reading-order
column plus `[N,200,200]` instance masks; mask → largest contour → approxPolyDP →
min-area rect gives the quad, as PaddleX does; `suppress_overlaps` is the
cross-class NMS PaddleX also runs, since the model reports the same lines twice at
times), `models.rs` finds or downloads
each model's files (pinned HF revision + sha256 manifest; `$SQUIGL_MODEL_DIR/<name>/`,
`models/<name>/` beside `$APPIMAGE`, exe-adjacent `models/<name>/`, then
`~/.cache/squigl/models/<name>/`; a download goes to `$SQUIGL_MODEL_DIR` when set, else
the cache; `SQUIGL_TEST_DEVICES=cpu` limits the `glmocr` tests to the CPU, as CI's
manual `models` job runs them on all three OSes), and
`lifecycle.rs` is both models' life (`Lifecycle<T>`: idle, preparing on a thread --
`ModelSpec::ensure` then the loader --, ready or failed, with the phase in a
`PhaseCell` and a cancel flag the download checks per file and per chunk; eager or
on `prepare()`, per the `ModelContext`), and
`lib.rs` holds the one process-wide `ort` environment (`ort` refuses a second)
and wraps GLM-OCR as a `Transcriber` (`phase`/`prepare`/`cancel_prepare` from the
lifecycle), plus `RUNTIME`, the
one lock every session load and run takes: the WebGPU EP segfaults on concurrent
`run` across sessions (microsoft/onnxruntime#32561, open) -- keep it until the
pinned runtime has the fix; and `GPU_LOST`, set by `note_gpu_loss` when a run fails
with a lost device (with IO binding the loss surfaces as an ORT error, not a
segfault), after which `attempts()` yields the CPU only and both services drop their
model and reload (`Lifecycle::reload`, phase `Reloading`). In `app.rs` the crop
is a rectangle plus an optional quad (`Selection`); any hand edit of the crop drops
the quad (`set_rect`) except dragging a quad corner, which moves that corner and
refits the rectangle. Block mode (`block_mode`) re-runs the detector whenever the
shown frame is new (throttled to `LIVE_DETECT_INTERVAL` live, paused while a read
holds the GPU), so blocks follow zoom and aim; a fresh detection hands the selection
to the new block with the highest IoU (`follow_selection`, so tab keeps its place and
an untouched crop tracks its block); "read all" captures first, waits for the
capture's own detection, then drains a snapshot queue one read at a time. `ort` is pinned to a git commit because the published rc.13 has a different
API (the pin carries ONNX Runtime 1.30; after moving it, run `glmocr_handwriting`, which
reads `crates/squigl-models/testdata/handwriting/` on WebGPU and CPU against the readings
recorded there -- a near-tie can flip on a kernel change, so re-record with
`SQUIGL_BLESS=1` only after looking at the diff); its `download-binaries` fetches pyke's prebuilt ONNX Runtime at build time, and
the WebGPU provider is a separate `libwebgpu_dawn.so` that lands next to the binary
(as a symlink into `~/.cache/dfbin` -- copy the real file into a release tarball),
found via the `$ORIGIN` rpath from `build.rs`. `--no-default-features` builds without
any of this. The hidden `--screenshot-after SECS --screenshot-path FILE`,
`--dev-crop X,Y,W,H`, `--dev-erase X1,Y1,X2,Y2,R` (a stroke; repeatable; captures first), `--dev-read [--dev-second]`, `--dev-detect`, `--dev-read-all` and `--dev-settings` flags let you
drive it from a script (GNOME blocks external screenshots of the window);
`XDG_CONFIG_HOME` points it at a scratch backend config.

### Non-obvious protocol details (hard-won, don't regress)

These are load-bearing and were each the cause of a real failure during bring-up:

- **`tunnel_forward=true` is required.** We reach the server through `adb forward`, so
  the server must *listen*; its default assumes `adb reverse` (connect-back).
- **`send_dummy_byte=true` + read that byte before the codec header.** With `adb forward`,
  the local TCP connect succeeds *before* the device-side socket exists, so an early
  read gets EOF. `connect_with_retry` reconnects until the dummy byte arrives.
- **`send_stream_meta=true`** is the 4.x name of what 3.x called `send_codec_meta`;
  the server only warns about unknown options, so a stale name silently changes the
  stream layout.
- **`scid` must fit in a signed 32-bit int.** scrcpy parses it with `Integer.parseInt(v, 16)`,
  so the top bit must be 0 (first hex digit ≤ 7). `random_scid_hex8` masks with `0x7fffffff`.

### Resolution ceiling

openh264 hard-codes H.264 level 5.2 (`GetLevelLimits(52)`, 36864 macroblocks) in its SPS
parser: 3840x2160 and 2992x2992 fit, 4000x3000 does not (`dsNoParamSets`, native
error 16). `decode::MAX_MACROBLOCKS` / `Backend::fits` encode this so the CLI rejects
oversized modes up front. The `ffmpeg` feature has no such limit (4000x3000 verified).

Separately, camera sizes that aren't multiples of 8 (4000x2250) fail on the *phone*:
scrcpy rounds them for the encoder and the camera refuses the rounded size
(`onConfigureFailed`, stream connects but no packets ever arrive). `cameras::is_usable_size`
filters these.

libavcodec gotchas in the ffmpeg backend: `avcodec_receive_frame` unrefs its destination
before returning EAGAIN (so drain into a scratch frame and swap), and an SPS/PPS-only
packet is "invalid data" (so it is prepended to the next packet, as scrcpy does).

## Camera controls

scrcpy 4.x exposes exactly two: zoom (`camera_zoom=` at start, Camera2
`CONTROL_ZOOM_RATIO`, Android 11+; the range comes from `--list-sizes` /
`CameraInfo::zoom_range`) and torch (`camera_torch=`). Both are also live control-socket
messages (TYPE_CAMERA_ZOOM_IN/OUT = 19/20, step ×1.0625; TYPE_CAMERA_SET_TORCH = 18)
-- `ConnectOptions::control` opens that second connection (made right after the video
socket's dummy byte; only the first connection gets one) and `CameraSession::control()`
hands out a cloneable `CameraControl` usable from any thread while `run()` blocks
(`crates/squigl-v4l2/examples/control.rs` shows it). The phone never reports the zoom it
ends up at, so the GUI tracks the step count itself. No exposure, focus or
white-balance control exists at any version.

## Scope

Camera→V4L2 only (no audio, display mirroring, or input control). Two camera sources:
USB/TCP-IP ADB (`--connect`; the one-time `--tcpip` switch needs USB) via scrcpy-server,
and `--webrtc` (any browser, no app, LAN only -- see `webrtc_source.rs` above; no
STUN/TURN means it does not reach a phone outside the local network, and it has none of
the zoom/torch/facing control the ADB path gets from scrcpy). Cross-platform
virtual-camera sinks (Windows/macOS) are explicitly out of scope — V4L2 is Linux-only.
