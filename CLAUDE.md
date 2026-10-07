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
cargo build --release -p squigl-desktop [--features ffmpeg]   # the Tauri app (needs WebKitGTK 4.1 and libspeechd-dev headers on Linux)
cargo test -p squigl-speech --features kokoro [--release -- --ignored --nocapture listen]   # reading aloud; `listen` says a sentence and its maths (Kokoro if in $SQUIGL_MODEL_DIR/kokoro)
SQUIGL_MODEL_DIR=... cargo test -p squigl-misaki --release -- --include-ignored   # the Misaki port against Python Misaki's readings (needs the Kokoro download's misaki*/ files)
cargo build --release -p squigl-core --example synth_recording && node --test crates/squigl-desktop/e2e/app.test.mjs
    # the app end to end through tauri-driver (needs tauri-driver, and WebKitWebDriver / msedgedriver;
    # NATIVE_DRIVER=path if not on PATH); CI runs it on Linux under Xvfb and on Windows
# Windows: the Visual Studio 2026 C++ Build Tools -- with 2022's, the prebuilt ONNX Runtime fails to link (__std_rotate)
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
no format conversion between the two sources. Camera controls go over a data channel the
page opens (`CONTROL_CHANNEL`, JSON text): the page reports a `RemoteCamera` (zoom range,
zoom, torch -- `getCapabilities`/`getSettings`; `getUserMedia` asks `zoom: true`, without
which Chrome hides it) on opening and exactly once per message, and takes `{"zoom": r}` /
`{"torch": b}` (`applyConstraints`); `WebrtcSource::control()` is a `WebrtcControl`
usable from any thread, keeping only the newest zoom and torch to send (the session loop
wakes at least every 50 ms to send them) and showing what was asked until every message
is answered, so a stale report does not jump a slider back. LAN-only by design: a host ICE candidate
on the bound interface, no STUN/TURN, no auth.

`crates/squigl-cli` is a clap CLI over this library (`linux.rs`; on other OSes
`main.rs` only says it needs Linux, so the workspace builds everywhere); it owns the
policy bits: Ctrl-C/SIGTERM
handling, the reconnect-with-backoff loop (keeping the V4L2 sink open across sessions),
`--list-sizes` and `--resolution max`. `--webrtc` switches to the WebRTC source instead of
ADB: `webrtc_server.rs` runs `squigl-pairing`'s server (at `--webrtc-bind`, or every
LAN and overlay address, each printed) and streams each session to the V4L2 device (one at a time: its `on_session` turns a second phone away with a 503).
`contrib/` has boot-time loopback config and a systemd user unit.

`crates/squigl-pairing` is the phone-pairing server both the CLI and the desktop app use:
`PairingServer::start(PairingOptions { addresses, port, decoder, cert_dir,
tailscale_https }, on_session)` runs a `tiny_http` HTTPS server per address (one token
for all; the WebRTC host candidate is the address the offer came in at) serving `capture.html` (the `getUserMedia` +
`RTCPeerConnection` page, with `setCodecPreferences` steering the browser to H.264 since
that's all squigl decodes) at `/` and a minimal WHIP-shaped ingest endpoint at
`POST /whip`, both answering only with this start's random `?t=` token (403 otherwise);
an accepted offer's `WebrtcSource` goes to `on_session` before the answer is sent (an
`Err` there is a 503). `offers()` is each address's `Offer` (network, URL with token,
whether the certificate is trusted), `url()` the first's, and `qr_svg(url)` a QR code
(`qrcode`, black on white). `addresses()` (`addresses.rs`, via `if-addrs`) lists what a
phone could pair at: IPv4 LAN addresses (the default route's first) named Wi-Fi/Wired/
Local from the interface name, then overlays -- Tailscale (`tailscale*`, or 100.64/10),
ZeroTier (`zt*`), Nebula (`nebula*`) -- with loopback, link-local, public and container/
VM bridges (`docker*`, `br-*`, `virbr*`, `vEthernet*`…) left out; `classify` is the
pure, tested part. A phone on another network pairs through an overlay both are on
(squigl embeds none: a browser cannot join one). With `tailscale_https`
(`tailscale.rs`; the desktop's opt-in `[desktop].tailscale_https`, off by default since
issuing it puts the machine's name in public CT logs) the Tailscale address is offered as
`https://machine.tailnet.ts.net` with `tailscale cert`'s Let's Encrypt certificate when
`tailscale status --json` lists the name in `CertDomains` (on Linux the user must be the
tailnet's operator); else it falls back to the self-signed one. The certificate is `rcgen` self-signed (`getUserMedia`
needs a secure context and a LAN IP isn't CA-certifiable, so the phone's browser warns);
with `cert_dir` it is kept (`pairing.crt`/`.key`/`.names`, the key 0600; its names are
every offered IP) and remade only when the addresses change, so each phone warns once; the CLI passes none (a fresh one per
run). Port 8443 unless taken (Firefox keys its exception on host and port). `docs/pairing.md` is the user-facing account of what a firewall must let in (that TCP port; a free UDP port per session, `webrtc_source.rs` binds port 0) per OS. Its
`testing` feature has `FakePhone`: str0m as the browser, sending openh264 colour bars
from 127.0.0.1, which the pairing and engine tests stream through a real session.
Stopping the server closes the listener, but a connection kept alive goes on hanging
(tiny_http keeps its thread): a test's request needs a connection of its own.

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
`Notice`s, plus `ResultAppended`/`ResultsCleared`/`HistoryAppended` (the lists are
too long to resend as slices). `engine/reads.rs` is reading, moved out of the egui
window (roadmap Phase 7): the selection (`SetSelection`, a hand edit, drops a block's
quad and role; `MoveCorner` keeps a quad's other corners), block mode
(`SetBlockMode`; `pump` re-runs the detector whenever the shown frame is new,
throttled to `LIVE_DETECT_INTERVAL` live, not while a read holds the GPU; a fresh
detection hands the selection to the new block with the highest IoU,
`follow_selection`), erasures (`SetErasures`, the whole list: the front end paints,
erasing a live picture captures it, they go with their capture; `render::erased_frame`
paints them into a copy of the capture once per change -- only the pixels a stroke
covers differ -- which `render_planes` and `displayed_pixel` show in its place, so the
picture is what a read sees), reads (`Read`,
`ReadAll` -- captures, waits for the capture's own detection, then a snapshot queue
one read at a time --, `SecondOpinion`, `CancelRead`) on threads that call the
waker, results (kept per capture and scope; `results_generation` changes when the
list empties) and the session's `History`. A read refused with "try again shortly"
(a lost GPU, the model reloading on the CPU) is sent again once the backend is ready,
up to `RELOAD_RETRIES`. Its tests drive it with a scripted fake reader and
detector, gated where a test needs something held. `render_planes`
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
`Replay`, `Image` (EXIF-oriented, trimmed to even size, published once),
`TestPattern` or `Network` (a phone paired by QR code: `Shared::pair`/`Engine::pair`
hands a session over -- a second replaces the first -- and the worker streams it,
sized at its first frame, then waits for the next; until one comes the status is
`Connecting`); `SourceSpec::capabilities()` says which camera controls exist (the
ADB phone has zoom, torch and facing; `set_zoom` is a no-op without), and
`Shared::capabilities()` adds a paired phone's from its `WebrtcControl` (zoom and
torch as its camera reports them: its zoom is any ratio in range, sent at once and
never walked in steps, and nothing of it touches the ADB phone's `options`), and
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
finished read of the session (`Engine::history`, appended alongside the results,
so the last N history entries are the N results -- "copy all" relies on that),
Markdown export by capture;
`speech.rs`: the `Voice` trait (say an utterance by id, stop, `ended`, the
system's voices, `configure` voice and rate, `maths` -- MathML in words) and
`utterances`, a reading as sentences to say (split after `.`/`!`/`?` and a space,
at line breaks and around display maths; maths in words by the voice, from
`math::speakable`, else its source); `engine/aloud.rs` is reading aloud on it
(`Speak { result }`, `ReadAloud` -- block mode on, a "read all", each block said
as it lands --, `PauseSpeaking`/`ResumeSpeaking` -- the sentence again, no voice
pauses mid-word everywhere --, `StopSpeaking`, `SkipSpeech`, `[speech].speak_new`)
with the `SpeechSlice` (voices, what is being said: result and sentence in UTF-16
units, paused, following); each `ReadResult` carries its `selection`, which is how
a front end shows the block being said; `typeset.rs`: the `Typesetter` trait and `typeset_source` (the tint colours are the
front end's); `math.rs`: a reading as text and maths `Part`s (split at `$`, `$$`,
`\(`, `\[` as `squigl-math` splits for Typst; offsets in UTF-16 units, a web page's),
the maths as MathML Core by `math-core` (spike S7's choice, `spikes/s7-mathml`): an
unknown command is retried as `\operatorname{…}`, maths that still fails stays
source text, `for_webkit` moves an `<mo>` script base's spacing (`\log_2`) outside
the script element where WebKit wants it, and `speakable` is the MathML MathCAT
reads best (no variation selectors, the vector arrow as U+2192, no separator after
`cases`); `paths.rs`: the config, cache and pictures directories per OS via the
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
The window: the toolbar (freeze, rotate, magnification, colours, Open image…, Use
phone, Settings, full screen), a reading-line overlay, and `Settings.svelte`, a
native `<dialog>` (colour mode, ink/paper colours, contrast/brightness/mid-tones and a
two-colour cut-off -- sliders send `PreviewConfig` while dragged and `SetConfig` when
let go, so one drag is one save --, smoothing, reading line, start magnification,
the UI theme, and the keyboard). Themes are CSS custom properties on
`:root[data-theme]` from `[desktop].theme`; shortcuts (`lib/shortcuts.ts`) are the
default keys plus the `[desktop].shortcuts` overrides, with character keys dropped
when `single_key_shortcuts` is off (WCAG 2.1.4); a key moved to one action leaves the
one it had. Images open from the file input, paste (`open_image`: the bytes go to
the cache's `opened/`, only the newest kept) and Tauri's drag-and-drop (paths, so a
`.sqrec` plays). `lib/renderer2d.ts` is the Canvas2D fallback when WebGL2 is
missing at start (the same conversion and table on the CPU; it matched WebGL2 to
under 0.1/255 on average in a side-by-side snapshot); a lost WebGL context is
restored, not swapped (a canvas that gave a WebGL context cannot give a 2D one).
When the stream waits on a `stream::Problem` (worked out by `stream::classify` from
the error chain: core's `AdbNotFound`/`NoDevice`/`DeviceUnauthorized`/
`DeviceOffline`, a missing file, then adb's and the scrcpy server's own words --
"multiple devices", "not supported before Android 12", `CAMERA_IN_USE`),
`Connection.svelte` shows `lib/guidance.ts`'s plain-language steps for it (adb's
install advice per platform) with Try now / Open an image instead / Hide, and takes
focus when the problem changes. `adb::pick_device` reports an unauthorized or
offline phone as such rather than "no device".
Hidden flags `--dev-keys "r + m"`, `--dev-snapshot-after SECS --dev-snapshot-path
FILE` (the canvas as PNG, then quit), `--dev-stats` (frames drawn per second and the
request-to-drawn times, to the log), `--dev-canvas2d`, `--dev-config FILE` (start from
the defaults, save there), `--dev-text-scale F`, `--dev-window-size WxH`,
`--dev-fake-phone` (pairs `FakePhone` at start, with zoom and a torch; `dev_phone_camera`
reads what it was asked: the e2e test of the camera controls), `--dev-pairing-bind IP,...` (pair at 127.0.0.1, say, not this machine's addresses; comma-separated, since msedgedriver keeps only the last of a repeated switch) and `--dev-probe` (`window.squiglProbe`:
the last frame's header, and drawn pixels beside `Engine::displayed_pixel`, the
engine's reference -- what `e2e/app.test.mjs` checks each display mode with) drive it
from a script; page errors and warnings go to the app's log (target `page`). The page
keeps up to two frame requests in flight (`frames.ts`'s `MAX_IN_FLIGHT`): with one,
the socket idled while WebKitGTK took in a reply. Windows' Text size setting zooms the
webview (`os_text_scale`); WebKitGTK follows GNOME's by itself. `SQUIGL_LOG_FILE=FILE`
sends the app's log (and panics) to a file -- a Windows release build has no console --
and on Windows an unknown argument is logged and skipped (msedgedriver passes Chromium's).
The main window is built in `setup` (`"create": false` in `tauri.conf.json`) so that,
on Windows, `WEBVIEW2_USER_DATA_FOLDER`/`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` (how
msedgedriver passes its DevTools port and folder) are applied over wry's own.
Pair phone (`Pair.svelte`, `pairing.rs`) runs the pairing server only while its dialog
is open (`pairing_start`/`pairing_stop`; the certificate beside the settings file) and
shows each network's QR code (a radio group picks it when there are several), the
address with Copy, what the phone's browser will ask, and what to do when the phone is on
another network; `pairing_start` is async (a `tailscale cert` can take a while); a phone
that pairs goes to the engine through the host (`Host::pair`) and closes the dialog.
The camera's own zoom (a slider, 0-100 over the log of the range, `lib/camera.ts`, so
its ends are exact and an arrow key is a visible step; `[`/`]` move 4 grid steps) and
torch (`t`) appear in the toolbar when the stream slice's capabilities have them.
After changing the UI, `npm run build` before `cargo build`: the binary embeds `ui/dist`.
Reading (roadmap Phase 8): the app is built with the built-in models
(`local-model`, default, as in egui; `engine_deps` in `main.rs`, prepared only on
`PrepareModel`, so a download needs the person's yes). `Overlay.svelte` is an SVG over
the canvas that draws the blocks (outlined by kind in colour *and* dash pattern) and the
selection with corner handles, placed by the shown frame header's `placement`
(`lib/selection.ts`: view px -> canvas = (p - origin) * scale / dpr), and turns drags
into `SetSelection` / `MoveCorner` and clicks into `SelectBlock`. `Reading.svelte` is
the pane (`p`): backend choice, the download/ready prompt for the reader and -- once
Blocks was asked for -- the block finder, Read (Enter) / Read all (`a`) / Second
opinion (`o`) / Stop, and the results (kept by the page from `ResultAppended` /
`ResultsCleared`) as large text, unsure tokens underlined dotted (wavering) or wavy
(hesitant) as well as tinted (`lib/reading.ts`'s `spans`, only when the tokens rebuild
the text). Maths shows as MathML: the `math_parts` command is the engine's `math::parts`,
`lib/math.ts`'s `runs` lays the marks over it (a formula marked as a whole, by its least
sure token, as egui shades it, with a dotted or dashed line under it) and `sanitize`
rebuilds the MathML from MathML elements and presentation attributes alone before it
goes into the page -- it comes from a model. `b` toggles blocks, `n`/`N` step through them, Escape clears the box (or first leaves
the brush or the readings-only view). The erase brush (`e`; its size in CSS pixels in
the pane) paints a stroke per drag; `x` erases the box (`lib/erase.ts`'s
`coverStrokes`: rows across the outline, spilling over by a fifth of a row at most --
with `n`/`N`, the keyboard's way to erase); `u` undoes a step (a box is many strokes,
so the page keeps where each step began). The pane also has the reading text size
(A−/A+, and Settings; `[ui].reading_size`, shared with egui), the session's history
(`HistoryAppended`, newest first, kept across Clear), and Readings only (`Shift+F`:
the toolbar hidden, the picture laid out but invisible so its canvas keeps a size,
the window full screen until it is left).
`--dev-backend URL` reads with one OpenAI-compatible server instead of the configured
backends (the e2e test runs a fake one that answers with the image's size).
Reading aloud (`squigl-speech`, the `speech` feature, default): `Voices` is the
engine's `Voice`, routing to Kokoro (`KokoroVoice`, the `kokoro` feature, which the
desktop's `local-model` turns on) once its model is ready and chosen -- `[speech].voice`
`None` or `kokoro:<id>` -- else the system's (`SystemVoice`: `tts`, speech-dispatcher /
WinRT / AVSpeechSynthesizer, on its own thread, an utterance's end noticed there -- the
end callback as a nudge, `is_speaking` as the truth -- and passed on through the
waker; only its English voices are listed, speech-dispatcher has 13,000); `Maths` is
MathCAT (ClearSpeak, rules zipped in) on its own thread (its state is per thread), for
either voice. `KokoroVoice` makes each sentence with squigl-models' `KokoroService` on
its thread (the next one while this plays: the engine's `Voice::prepare`), resamples
24 kHz to the output's rate (windowed sinc) and plays through cpal -- so it pauses
mid-word (`Voice::pause`/`resume`; the system voice says the sentence again). Not a
default member: on Linux it needs `libspeechd-dev` (and libclang) and, for Kokoro,
`libasound2-dev`. The voices are their own slice (`VoicesSlice`, sent once); a
voice's downloadable model (`Voice::model`) is listed with the reading models
(`ModelKind::Voice`) and the pane offers it ("a natural voice"), downloaded only on
the person's yes. The pane has Read aloud (`s`), Read the page
aloud (`A`), Pause/Go on (`.`), Stop (`S`), Previous/Next (`<`/`>`) and a per-reading
Read aloud; the sentence being said is boxed in the text (`runs`' `current`), its
reading edged and its block outlined on the picture; Settings has voice, speed and
"read each new reading aloud". `--dev-fake-voice` reads aloud with a voice that says
nothing, 0.4 s an utterance, and keeps what it was given (`dev_spoken`).
WebKitWebDriver's pointer lands off where it is aimed (about 100 px high, under
Xvfb): its click misses buttons on the toolbar's second row, so e2e tests focus and
press Enter instead, and a drag test reads back where its stroke went
(`squiglProbe.erasures`).
Toolbar icons are Lucide's (`@lucide/svelte`, ISC, in NOTICE), drawn in `currentColor`
so they follow the theme. In a small window (`max-width: 48rem` or `max-height: 28rem`:
a small screen or large text) the toolbar goes compact -- icons alone, no key hints --
with the labels visually hidden, not removed, so names stay whole. `<select>` and the
range slider are `appearance: none` and draw themselves (the track `--edge`, the thumb
`--text`): WebKitGTK paints native ones in GTK's colours (a white-on-light-grey select)
while reporting the CSS ones, which fooled the contrast test. Under WebKitGTK's page
zoom (`--dev-text-scale`) the icons' strokes render thick; GNOME's text scale arrives
as the device pixel ratio instead, so real use is unaffected. Without `nasm` on
PATH, `openh264-sys2` quietly builds without its assembly (~40% slower decoding). `ResizeObserver` alone does not size the canvas: WebKitGTK skips it
for a window that is not being drawn.

`crates/squigl-egui` is the egui document-camera window, on an `Engine` (eager
models; it pumps once per pass, shows `Notice`s in its status line, and reads the
capture, rotation, config, backends and all of reading from the engine) (`app.rs`:
preview, crop, capture, save, and hand erasures (the engine's, view-space
`erase::Stroke`s -- path plus radius -- painted with a brush over the Zoom pane, whose size is in screen
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
template's `compat` scope (with limits). `crates/squigl-misaki`: Misaki's English G2P (hexgrad/misaki `en.py` at fba1236,
Apache-2.0) ported -- `lexicon.rs` function for function (gold/silver dictionaries,
stress, special cases, -s/-ed/-ing, numbers with `numbers.rs` as num2words writes
them), `lib.rs` the token pipeline (subtokenize, retokenize, context, stress
resolution), `tag.rs` a tokenizer and part-of-speech guesser in place of spaCy
(function words from a list, the rest from their neighbours: 98.6% of words as
Python Misaki has them on `testdata/reference.tsv`, the rest mostly spaCy's own
slips), `fallback.rs` Misaki's BART fallback for other words in plain Rust from
its safetensors (greedy, transformers' 20-token cap; exact on `testdata/fallback.tsv`).
American English only. `crates/squigl-models` (the GUI's
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
`kokoro.rs` is Kokoro-82M (onnx-community fp16 export, `models::KOKORO`, which also
fetches Misaki's dictionaries from GitHub and its fallback -- a `ModelFile` may name
its own `url`; `HF_TOKEN` goes only to huggingface.co): phonemes by squigl-misaki
(symbols said first: Misaki reads `=` as "x"), cut at MathCAT's pauses under Kokoro's
510, run on the CPU outside `RUNTIME` (spike S8: safe beside GLM-OCR on WebGPU); not in
`models::ALL`, which `--fetch-model` takes for egui. `lifecycle.rs` is the models' life (`Lifecycle<T>`: idle, preparing on a thread --
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
segfault), after which `attempts()` yields the CPU only (as it does for `auto` when
`gpu_present()` -- wgpu's adapter list on D3D12/Metal/Vulkan, asked once -- finds only
software rasterisers: WebGPU on WARP or llvmpipe read a crop ~10x slower than ONNX
Runtime's CPU kernels) and both services drop their
model and reload (`Lifecycle::reload`, phase `Reloading`). `app.rs` draws the
engine's reading state (`engine/reads.rs`) and turns gestures and keys into its
commands; it keeps only view state (drags, the brush, the typeset textures per
result, panes, the live enhancement, flushed into the config before a read). `ort` is pinned to a git commit because the published rc.13 has a different
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
STUN/TURN means it does not reach a phone outside the local network, and of the ADB path's
camera controls it has zoom and torch, when the phone's browser offers them, not facing). Cross-platform
virtual-camera sinks (Windows/macOS) are explicitly out of scope — V4L2 is Linux-only.
