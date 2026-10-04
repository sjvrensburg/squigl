# Squigl roadmap: from Linux document camera to cross-platform accessibility tool

## Context

Squigl today is a capable tool for one expert user. It is Linux-only, and the setup is aimed at developers (USB debugging, adb, v4l2loopback).

We agreed to restructure **in place**: same repo, same name, cut as "Squigl 1.0" when the new UI ships.

### Who it is for

There are two audiences, and they often overlap (the author is in both):

1. **Low-vision users who want a "poor man's video magnifier".** They need a phone (or any camera) as a desktop magnifier: a big magnified live view, freeze, high-contrast display modes, and a simple, large UI.
2. **Teachers and lecturers reading students' handwriting.** They need to get a transcription of a hard-to-read script, including the maths, see where the model was unsure, and compare readings. Student work stays on the machine with the built-in model.

**Read-aloud serves both.** It speaks a transcription, or a printed page, in reading order, with maths spoken properly.

**Non-goal: blind users.** Squigl needs the user to aim a camera and check the ink against the reading, so it will never work for someone with no sight. Screen-reader-only use is out of scope and is not a design driver:

- There is no screen-reader parity work and no Orca/NVDA/VoiceOver exit criteria.
- The web UI still uses native HTML elements and labels, because that is free and helps keyboard and OS-magnifier users.
- What matters for low vision is legibility at large scales, high contrast, keyboard operation, and staying usable alongside OS magnifiers (GNOME Zoom, Windows Magnifier, macOS Zoom) that follow the focus.

### Decisions

- **The final UI is Tauri 2.** It is a web-tech UI in a desktop shell, the way VS Code is, with the Rust core linked directly.
- **The egui UI gets no new accessibility work.** It stays working, with maintenance only, until Tauri reaches parity, and is then retired.
- **V4L2 becomes a Linux-only add-on crate.** It is no longer a dependency of the core.

### What shapes the ordering

- **The magnifier ships first.** The magnifier MVP needs only the camera pipeline, freeze, rotation, a viewport and display modes. It needs no OCR, models or Typst, so the Tauri magnifier can ship **before** the big extraction of read orchestration from `app.rs`.
- **Reading and read-aloud follow as one phase.** Together they serve both audiences.
- **Teacher workflows come after that:** scripts with many pages, opening scans and PDFs, and export.

Two rules hold throughout:

- Every phase leaves the egui app working, and CI green for both feature sets.
- `CLAUDE.md`, `README.md` and `NOTICE` are updated in the same PR as the change they describe.

### Test machines

Squigl is tested on two machines:

- a Fedora box (AMD, Wayland and X11);
- an Ubuntu box with an Nvidia GPU.

There is no Windows PC or Mac. The exit criteria below are graded to match:

- **Linux** is tested on real hardware on both boxes, for function and for performance.
- **Windows** is built and tested by GitHub's `windows-latest` runners. A Windows 11 VM on the Fedora box (KVM, with the phone passed through over USB and the VM on the LAN) checks function: adb drivers, firewall prompts, installers, Windows Magnifier, voices. It cannot measure performance, because a Windows guest gets no GPU acceleration under KVM.
- **macOS** is built and tested by GitHub's `macos-14` runners only. Apple's licence allows macOS VMs only on Apple hardware.
- **Where a criterion needs real Windows or macOS hardware** (frame rates, the GPU provider, macOS Zoom, clean-machine installs), it is marked *(tester)*. It is met when someone with that machine runs it, and does not block the phase.

---

## Target architecture

```
crates/
  squigl-core/     was phone-cam4linux. adb, protocol, decode, convert, cameras, session,
                   webrtc_source, sink (FrameSink trait only), error, + test_pattern (from CLI)
                   + replay (H.264 Annex-B file source for CI/demos). Keeps the scrcpy-jar build.rs.
                   No `v4l` dependency.
  squigl-v4l2/     Linux-only: V4l2Sink, LazyV4l2Sink, loopback (pkexec/modprobe)
  squigl-pairing/  HTTPS/WHIP server + webrtc_capture.html (from squigl-cli/webrtc_server.rs),
                   detect_lan_ip, QR code (SVG), persisted cert, per-session token
  squigl-engine/   UI-agnostic app logic: geometry, render, stream, config, enhance, erase,
                   history, layout/transcribe traits, typeset trait, display LUTs. No egui/ort/typst.
  squigl-models/   local/* (GLM-OCR, PP-DocLayoutV3, models.rs, RUNTIME, GPU_LOST); ort-gated
  squigl-math/     mathtext.rs + assets/mitex (Typst); egui-only, retired with egui
  squigl-egui/     was crates/squigl; binary still named `squigl` until switchover
  squigl-desktop/  Tauri 2: src/{main,host,commands,transport,pairing}.rs, ui/ (Vite + TS),
                   tauri.conf.json, capabilities/
  squigl-cli/      core + pairing + (Linux) squigl-v4l2
```

**Dependencies:**

- `squigl-engine` depends on `squigl-core`.
- `squigl-models` and `squigl-math` implement the engine's traits.
- `squigl-egui` uses `squigl-engine`, `squigl-models` and `squigl-math`.
- `squigl-desktop` uses `squigl-engine`, `squigl-models` and `squigl-pairing`.

**Workspace rule:** `squigl-desktop` is **not** in `default-members`. Tauri's `generate_context!` needs the built frontend at compile time, so this keeps `cargo build --workspace` and the current CI unaffected. The desktop app gets its own CI job (`npm ci && npm run build`, Node pinned).

### Engine API (Tauri and egui share it)

- **A synchronous state machine.** The engine spawns workers but has no threads of its own beyond them. It is deterministic in tests (fake `Transcriber`/`BlockDetector`, injected `now`).
  - `Engine::new(config, deps, wake: Arc<dyn Fn()+Send+Sync>)`
  - `handle(Command) -> Result<Reply>`
  - `pump(now) -> Vec<Event>`
  - `state()`, `frame(which)`, `render_planes(&ViewRequest)`
- **Background work.** Each background thread posts an `Internal` message (`ReadDone`, `DetectDone`, `Model`, `StreamStatus`, `NewFrame`) and calls `wake`. This replaces egui's per-frame polling (`poll_read`, `maybe_detect`, `drop_stale_*`, `start_read_all_if_ready`). PNG encoding moves off the UI thread into the read thread.
- **`Command`** (serde-tagged):
  - freeze, rotation, selection, `DragCorner`, block select/step/mode;
  - `Read`, `ReadAll`, `SecondOpinion`, `CancelRead`;
  - erase begin/extend/end/undo;
  - save and export;
  - `SetConfig`, camera zoom/torch/facing, `UseSource(SourceSpec)`;
  - `PrepareModel`, `CancelModelDownload`.
- **`Event`:**
  - versioned slices (`Stream`, `Capture`, `Blocks`, `Reading`, `Models`, `Config`);
  - append-only `ResultAppended`, `ResultsCleared`, `HistoryAppended`;
  - `Notice{text, level}`, which replaces `say()` and maps to `aria-live` in the web UI.
- **Tauri host.** An `EngineHost` thread owns the `Engine`. It diffs slices against the last ones emitted and pushes only the changed ones over one `tauri::ipc::Channel<Event>`. The `Stream` slice is throttled to 4 Hz. Commands go through a single `dispatch(cmd)`, with TypeScript types generated by `tauri-specta`, or `ts-rs` as a fallback.
- **egui adapter.** The waker is `ctx.request_repaint`. Each `pass` calls `pump` once and draws from `state()`. The dev flags become engine commands.
- **Models load lazily.** Today `LocalBackend::new` calls `prepare()` immediately (`crates/squigl-models/src/lib.rs`, `LocalBackend::new`), which can start a ~780 MB download at launch.
  - The engine prepares a model only on `PrepareModel`, which the Tauri UI sends after a consent screen.
  - egui keeps eager loading through a config flag.
  - Progress is structured: `ModelPhase::{NotInstalled, Locating, Downloading{done,total}, Verifying, Loading, Ready, Reloading, Failed}`. It is reported through a notify callback, which also fixes the model threads having no wake today, and coalesced to 4 Hz or 1% steps.

### Frame transport (the biggest technical risk; spike S1 decides it)

- **Reduce the data in Rust.**
  - JS sends a `Viewport` (CSS size, DPR, centre, magnification, rotation). Rust crops to the source region and decimates by an integer step, so delivered pixels are at most ~1× display pixels.
  - Rust sends **raw planes**: Y only for high-contrast modes, I420 for colour. There is no RGBA conversion and no rotation in Rust (new `convert::i420_region_planes`).
  - On freeze, the full-resolution frame is sent once. Pan and zoom then run on the GPU with no transport at all.
  - Each frame carries a header: seq, pts, source size, region, step, format, strides.
  - Flow control is pull-based and latest-wins: at most two frames in flight.
- **Default transport: a localhost WebSocket.**
  - Bound to `127.0.0.1:0` with a 128-bit per-launch token and an Origin check; CSP allows `connect-src ws://127.0.0.1:*`.
  - Why WebSocket: its throughput doesn't depend on the webview's custom-scheme handlers (the least predictable part of WebKitGTK and WebView2), it is the same on all three OSes, and the same socket carries viewport updates back.
  - If S1 shows the Tauri custom URI scheme or IPC Channel meets the bar on all three OSes, use that instead and avoid opening a port.
  - Why raw instead of JPEG: JPEG ringing around text edges gets amplified by high-contrast lookup tables at 8–16×.
- **Rendering.**
  - A WebGL2 shader does YUV→RGB (matching `convert.rs` coefficients) and treats rotation, pan and zoom as UV transforms.
  - Display modes use a 256×1 lookup table computed in Rust by `squigl_engine::display::lut(&DisplayMode)`. Modes: normal, grey, inverted, yellow-on-black, white-on-black, black-on-yellow, custom pair. Contrast, brightness, gamma and threshold are baked into the table, so it can be unit-tested and shared with egui.
  - Canvas2D fallback when WebGL2 is unavailable or the context is lost.
  - The heavier `enhance.rs` pass stays in Rust, for frozen captures only.
- **WebCodecs (H.264 decoded in the webview)** is deferred. It can't be relied on in WebKitGTK, 4000×3000 exceeds hardware decoder levels, and Rust needs the frames anyway for capture and OCR.

---

## Phases

### Phase 0: Spikes (about two weeks, alongside Phase 1)

| Spike | Time box | Decides |
|---|---|---|
| **S1** Frame transport | 4 days | Tauri hello app with a synthetic source (seq and timestamp burned into pixels). Custom scheme vs IPC Channel vs WebSocket, at 1/3/5.5/12 MB payloads, with a WebGL2 renderer. Machines: Fedora Wayland/X11 (AMD) and Ubuntu 24.04 with Nvidia; Windows 11 and macOS 14 *(tester)*. WebKitGTK is the hardest webview, so Linux passing is the bar; WebSocket stays the default unless another transport passes everywhere it can be measured. **Pass:** ≥30 fps at 3 MB, p95 ≤ 50 ms, under one core in total. |
| **S2** Cross-OS native build | 2 days | On the GitHub Windows and macOS runners, brought forward as Phase 3's CI matrix, and in the Windows VM: ort git-pin prebuilt binaries for windows-x64 and macos-arm64; where the Dawn DLL/dylib lands; openh264 `source` with MSVC and nasm; the scrcpy jar fetch; `glmocr_handwriting` on CPU on each OS. If the WebGPU EP is missing on an OS, use CPU there for now. |
| **S3** WebKitGTK GPU, scaling and OS magnifiers | 3 days | Is WebGL2 on with Nvidia/Wayland (Ubuntu box), AMD Wayland/X11 (Fedora box) and llvmpipe (`LIBGL_ALWAYS_SOFTWARE=1`)? Note the `WEBKIT_DISABLE_DMABUF_RENDERER` workaround. Does GNOME Zoom follow keyboard focus in a Tauri window? Windows Magnifier is checked in the VM; macOS Zoom is *(tester)*. Does OS text scaling reach the webview, and does the UI hold up at 200–300%? Flatpak vs .deb. |
| **S4** Phone onboarding | 2 days | Windows adb on 2–3 phones (OEM driver?). Adb setup and QR pairing walked through with one low-vision user and one non-technical teacher, noting where each gets stuck (Developer options, the certificate warning). |
| **S5** Display-mode legibility | 1–2 days on S1's build | 2–3 low-vision users compare the modes, and raw vs JPEG, at 8–16×. |
| **S6** WebCodecs probe | 0.5 day | Availability per webview (for the record only). |
| **S7** LaTeX→MathML and maths speech | 2 days, before Phase 8 | `pulldown-latex`/`latex2mmlc` in Rust vs Temml in JS, measured on `testdata/handwriting` readings. Quality of MathCAT's ClearSpeak speech from that MathML through the `tts` crate on each OS. |

**S2 findings (CI, 2026-10-04):**

- core, engine, models, math and the egui window build, pass clippy and pass their tests on `windows-latest` and `macos-14` unchanged, apart from the per-OS rpath.
- The ort git pin's prebuilt binaries include the WebGPU provider on both:
  - **Windows x64:** `webgpu_dawn.dll`, plus `dxcompiler.dll` and `dxil.dll` (the D3D12 shader compiler), next to the exe.
  - **macOS arm64:** `libwebgpu_dawn.dylib`, next to the binary (found through `@executable_path`).

  On both, as on Linux, they are symlinks into ort's cache, so packaging copies the real files (Phase 6: all three DLLs on Windows).
- Still open:
  - `glmocr_handwriting` on CPU on those runners, which needs the model download (a manual job);
  - whether the WebGPU provider actually runs there, which needs a real GPU *(tester)*;
  - the Windows VM's checks.

### Phase 1: Mechanical restructure (no behaviour change)

- **1a. Rename `phone-cam4linux/` to `crates/squigl-core`.**
  - Accept both `SQUIGL_SERVER_JAR` and the old `PHONE_CAM4LINUX_SERVER_JAR`.
  - Update the build.rs path in the `sed` lines and cache keys of `.github/workflows/{ci,release}.yml`.
- **1b. Split out `squigl-v4l2`.**
  - Move `sink::V4l2Sink`, `loopback.rs`, `CameraSession::run_to_v4l2` (`session.rs:~257`), `WebrtcSource::run_to_v4l2` (`webrtc_source.rs:~114`) and `examples/control.rs` there.
  - Core's `Error::Sink` becomes generic; `Error::Loopback` moves to the v4l2 crate.
  - In the GUI, the `--device` tee (`crates/squigl/src/stream.rs:8,290,334,387`) becomes Linux-only by target cfg (done that way rather than as a cargo feature: same behaviour on Linux, nothing to remember to enable).
- **1c. Create `squigl-engine` from the egui-free modules:** enhance, erase, history, layout, stream, transcribe (including `Config`).
  - New `geometry` module: `Crop`, `Selection`, `zoom_to_view`, from `app.rs:73-188,367-444`. (`wheel_notches` stays in `app.rs`: it's scroll-input handling, not geometry.)
  - New `render` module: `render_region` and `render_selection`, from `app.rs:349-428`.
  - New `typeset` module: `TintSpan`, `Typesetter` and `typeset_source`. `typeset_source` takes a tint closure, so `Color32` stays in the UI.
  - This removes the back-references into `app.rs` from `layout.rs:5`, `local/layout.rs:9` and `mathtext.rs:7,495`.
  - `LocalDevice` is defined once, in the engine (`local::DevicePref` aliases it). `BackendConfig::build` builds only the HTTP backends; the GUI passes a `BackendFactory` that adds the built-in model, the same pattern as `DetectorFactory` (and the shape of Phase 2's `EngineDeps`).
- **1d. Create `squigl-models` (`local/*`) and `squigl-math` (`mathtext.rs` + `assets/mitex`).** Rename `crates/squigl` to `crates/squigl-egui`, keeping `[[bin]] name = "squigl"`.

**Exit criteria:**

- Same test count, now spread across crates.
- fmt and clippy `-D warnings` pass for both feature sets.
- A CI check that `cargo tree -p squigl-engine` contains no egui, ort or typst.
- The scripted runs (`--screenshot-after` with `--dev-crop`/`--dev-read`/`--dev-detect`/`--dev-read-all`) behave the same.
- A dry run of the release workflow produces the same artefacts.

### Phase 2: Engine foundations (the full API shape, magnifier features filled in)

- `Engine`, `Command`, `Event`, slices, waker and `pump`. **Every slice is defined now**, reading ones included, so the API doesn't churn later.
- Stream worker generalised to `SourceSpec::{Adb, Webrtc, TestPattern, Replay, Image(PathBuf)}` plus `Capabilities{zoom, torch, facing}`; WebRTC and Image have none of them.
  - `Image` is a still frame from a file, so teachers can read a scanned or photographed script with no phone.
  - Expose it cheaply in egui now: a `--open FILE` flag, drag-and-drop and paste. It's a feature, not accessibility work, and serves teachers right away.
- Freeze/capture with seq, rotation, `ViewRequest` → `render_planes`, and `display::lut`.
- `Config` via the `directories` crate, replacing the hand-rolled paths:
  - `transcribe.rs:396`, `local/models.rs:126`, `main.rs:223`.
  - **Linux paths stay byte-identical**, with a test for that.
  - New `[magnifier]` and `[display]` sections.
  - A round-trip test so neither UI drops the other's fields.
- Structured `ModelPhase` and lazy `PrepareModel`; egui keeps eager loading via a flag.
- egui uses the engine for stream, freeze and rotation only. Reads and crop stay in `app.rs` for now.

**Progress:**

- **Replay source and recorder:** done. `squigl_core::replay`, with `--record` and `--replay` on the CLI and `squigl --replay`.
- **`SourceSpec::{Phone, Replay, Image, TestPattern}` with `Capabilities`:** done in `squigl_engine::stream`.
  - `Phone` reads the config's ADB options, which are kept while another source is in use.
  - `Webrtc` is added in Phase 5, with the pairing server.
  - `squigl_core::TestPattern` replaces the CLI's own pattern.
- **egui:** `--open FILE`, drag-and-drop of an image or `.sqrec`, and "Use phone" are done; the camera controls follow `Capabilities`.
  - Pasting an image moves to Phase 4. egui-winit takes ctrl+V for itself and passes on only text, so an image-only clipboard never reaches the app. The webview's own paste event handles images.
- **`squigl_engine::paths` (`directories`):** done. It covers the config, the model cache and the pictures folder; the Linux paths are unchanged, and a test holds that.

**Exit criteria:**

- Engine tests cover the stream state machine, freeze seq, `render_planes` (every rotation × region × step against `render_region`), the table for each mode, and config path compatibility.
- egui runs from the `Replay` source with no phone.

### Phase 3: Cross-platform foundations

- **adb discovery.** `squigl_core::adb::locate()` replaces the hard-coded `"adb"` (`adb.rs:175-191`). Search order: `$SQUIGL_ADB`, next to the exe (`adb[.exe]`), `$ANDROID_HOME/platform-tools`, then PATH. Spawn with `CREATE_NO_WINDOW` on Windows so no console window flashes.
- **Linker settings per OS.** OS-conditional rpath in each binary crate's `build.rs`; today `crates/squigl-egui/build.rs` has an unconditional ELF-only `-Wl,-rpath,$ORIGIN`.
  - Linux: `$ORIGIN`.
  - macOS: `@executable_path/../Frameworks`.
  - Windows: none.
- **GUI binaries:** `windows_subsystem = "windows"`.
- **Pictures folder** via `directories::UserDirs::picture_dir()`.
- **CI matrix** adds `windows-latest` and `macos-14`, building and testing core, engine, models and egui. egui on Windows/macOS comes almost free and is a cheap smoke test of the engine there.
- **ffmpeg stays Linux-only.** Windows/macOS use openh264, capped at 3840×2160, which is enough for a magnifier.

**Exit criteria:**

- CI passes on all three OSes.
- The egui app runs against a phone over adb on Windows (in the VM). On macOS *(tester)*.
- `glmocr_handwriting --ignored` passes on CPU on Linux, on the Windows VM, and on the macOS runner (manual workflow).

### Phase 4: Tauri skeleton and the magnifier MVP

- **App shell.** `squigl-desktop`: `EngineHost`, `dispatch` and `subscribe`, the S1 transport, and the WebGL2 table renderer with Canvas2D fallback.
- **Frontend.** Svelte 5 or plain TS using **native HTML elements, with no component library**. Native elements give keyboard behaviour and focus for free, and focus is what OS magnifiers track.
  - Everything is sized in `rem`, and the OS text scale maps to webview zoom.
  - The app ships its own high-contrast UI themes, because `prefers-contrast` and `forced-colors` are uneven on WebKitGTK.
- **Magnifier features:**
  - magnification 1–30×, keyboard pan (arrows) and zoom (`+`/`-`), reset;
  - freeze (space), rotate, fullscreen;
  - display-mode picker plus contrast, brightness and threshold;
  - nearest or smooth filtering at high zoom;
  - an optional reading line or ruler overlay;
  - big, simple chrome;
  - mode changes announced through `aria-live`.
- **Connection screen** with plain-language guidance mapped from `squigl_core::Error` variants: adb not found, no device, unauthorized ("look at your phone"), `CAMERA_IN_USE`, Android too old.
- **Single-key shortcuts** can be turned off or remapped (WCAG 2.1.4). Every action has a non-drag alternative (WCAG 2.5.7).
- **Tests:**
  - Vitest with `@tauri-apps/api/mocks`;
  - `tauri-driver` end-to-end on Linux and Windows against the `Replay` source (macOS has no WebDriver);
  - a pixel check of each mode against the Rust reference table.

**Exit criteria:**

- ≥25 fps magnified from a 3840×2160 source, with ≤100 ms added latency, on both Linux boxes. On Windows and macOS *(tester)*.
- Every control works from the keyboard, with a clearly visible focus ring.
- The UI is usable at 200% OS text scale and in each high-contrast theme.
- GNOME Zoom and Windows Magnifier (in the VM) follow focus around the controls. macOS Zoom *(tester)*.
- At least two low-vision testers complete the core tasks.

### Phase 5: Phone pairing in the GUI (QR / WebRTC)

- **Extract `squigl-pairing`** from `crates/squigl-cli/src/webrtc_server.rs`:

  ```rust
  PairingServer::start(bind, port, decoder, token,
      on_session: impl FnMut(WebrtcSource) + Send + 'static) -> Result<PairingServer>
  // PairingServer { url(), qr_svg(), stop() }
  ```

  The server reuses `WebrtcSource::run(&mut sink, stop)` with any `FrameSink`, instead of `run_to_v4l2`.
- **Persisted certificate.** The cert is stored in the data dir, so the browser warning appears once per phone rather than on every run. A per-session token travels as `?t=` and is checked on `/whip`.
- **Desktop app.**
  - The server runs only while the pairing dialog is open.
  - The QR code is shown as SVG with alt text, plus a large copyable URL.
  - Accepted sessions become `UseSource(Webrtc)`; zoom and torch are hidden through `Capabilities`.
- **Optional, behind S4's findings:** zoom and torch over a data channel using `track.applyConstraints({zoom, torch})` in `webrtc_capture.html`.
- **CLI:** `squigl-cli --webrtc` uses the shared crate, with unchanged behaviour.

**Exit criteria:**

- QR pairing works on Linux and on Windows (in the VM). On macOS *(tester)*.
- Firewall prompts are documented: Windows "private networks" (from the VM), and macOS incoming connections and `NSLocalNetworkUsageDescription` (from Apple's documentation until a tester confirms them).

### Phase 6: Packaging and beta release (the magnifier beta)

- **Linux:** .deb/.rpm plus Flatpak (the GNOME runtime gives a current WebKitGTK; `--device=all` for adb USB). The egui AppImage continues alongside until switchover.
- **Windows:** NSIS installer with the WebView2 bootstrapper, signed (Azure Trusted Signing or an OV certificate) to avoid SmartScreen. The uninstaller kills `adb.exe`.
- **macOS:** .dmg with Developer ID, hardened runtime and notarisation. The bundled adb and `libwebgpu_dawn.dylib` are re-signed and placed in `Contents/Frameworks` and `Resources`. The beta marks macOS *experimental* until a tester has run it. The Apple developer fee (about $99/yr) can wait until then.
- **Bundled adb:** check the platform-tools licence first. The fallback is adb built from AOSP source (Apache-2.0). Update `NOTICE`.
- **Models are not bundled.** They download with consent on first use of reading features.
- **Release workflow:** add a three-OS matrix job for `squigl-desktop`. The existing Linux tarball, AppImage and models steps stay unchanged.
- **Docs:** a short user guide with screenshots (`docs/user-guide.md`). The README is split into user and developer docs.

**Exit criteria:** install → pair → magnify works on clean Ubuntu 24.04 and Fedora machines, and on a clean Windows 11 (the VM restored to a fresh snapshot). On macOS 14 *(tester)*.

### Phase 7: Extract read orchestration (overlaps Phases 4–6; touches only egui and engine)

- **Moves into `Engine`** from `app.rs:1000-1479`: capture, read, read_all, second_opinion, read_with, poll_read, maybe_detect, detect, follow_selection, drop_stale_*, set_rect, drag_corner, block_at, save, and apply_config (keeping its backend diffing). Also `results`, `history` and the read queue.
- **Engine tests with fakes for:**
  - read-all waiting for the capture's own detection;
  - stale results being dropped on retake;
  - `follow_selection` IoU behaviour;
  - the GPU-lost retry;
  - `CancelRead`.
- **Geometry tests** ported from `app.rs`.

**Exit criteria:**

- `app.rs` is view code only (roughly 2,000 lines or fewer).
- The `KeyFocus` harness tests still pass.
- Every dev flag still works.

### Phase 8: Reading and read-aloud in Tauri (both audiences)

**Reading parity:**

- Selection and quad editing, block mode, read and read-all, and second opinion.
- The erase brush, with a keyboard alternative.
- History, settings, the model consent and progress UI, and image open, drop and paste.

**Readings as large text, not images:**

- Readings are rendered as text plus MathML, not Typst images, using the converter S7 picks.
- They are sized for low vision: an adjustable reading size, a high-contrast reading theme, and a "reading only, full screen" view.
- Confidence tints become CSS classes with an underline or other marking, never colour alone.

**Read-aloud** (in `squigl-engine`, so egui could use it too):

- **Engine:** the native `tts` crate (speech-dispatcher, WinRT/SAPI, AVSpeech). Web Speech is unreliable in WebKitGTK.
- **Speaking readings:** a button and key to speak the current reading, and an option to speak each reading as it arrives.
- **Reading a whole page:** "Read page aloud" captures, detects blocks and reads them in the layout model's reading order, speaking each one as it finishes, with the spoken block highlighted on the preview and its words in the text.
- **Controls:** rate, voice, pause/resume and stop; skip to the next or previous block.
- **Maths:** spoken through MathCAT (ClearSpeak) from the MathML, not as raw LaTeX.
- **Uncertainty:** optionally a short audible cue before an uncertain word, so a teacher listening knows which words to check.
- **Printed pages:** a "printed page" prompt preset, so the magnifier user can have a letter or a book page read to them.

**Exit criteria:**

- A feature checklist against egui is complete.
- The `glmocr_handwriting` readings render and are spoken correctly, maths included, on Linux and Windows (the VM has a sound device). On macOS *(tester)*.
- Two low-vision testers and two teachers complete their core tasks: read a printed page aloud, and transcribe and check a handwritten script.

### Phase 9: Switchover ("Squigl 1.0")

- The Tauri app becomes the `squigl` binary and `.desktop` entry.
- `squigl-egui` is deleted, along with `squigl-math` unless it is kept for PDF export.
- The AppImage gives way to the Tauri bundles and Flatpak.
- `CLAUDE.md` is rewritten for the new layout.

### Phase 10: Teacher and lecturer workflow

- **A script as a unit.** A "script" (one student's submission) holds several pages, by capture or by file, and has its own history, ordered by page then reading order. Scripts can be named, re-opened and listed.
- **Scans and PDFs.**
  - Open a multi-page PDF of scanned submissions; rasterise with `pdfium-render` or MuPDF (check licences).
  - Walk pages with page up/down and read-all per page.
  - Batch "transcribe this folder or PDF" runs in the background with progress.
- **Checking a transcription.** Side by side: the ink region and its reading, with uncertain words marked and the two readings listed separately when a second opinion disagrees. Click a word to see its ink.
- **Export for marking and feedback.**
  - DOCX with native equations (MathML→OMML is the risk; Pandoc-style conversion is the fallback).
  - HTML+MathML, Markdown, and plain text.
  - Every export carries the student or script name and keeps uncertain words marked.
- **Privacy, stated plainly.** With the built-in model, student work never leaves the machine. The UI says so, and visibly flags when a read goes to a cloud (OpenAI-compatible) backend.

**Exit criteria:** a teacher transcribes and exports a 10-page scanned script without help.

### Phase 11 onward

- **More cameras.** USB webcams and USB document cameras (`nokhwa`), useful for both audiences.
- **Magnifier extras.**
  - A split view: the live view beside a frozen capture.
  - Saved presets per user (mode, magnification, rotation).
  - Freeze with one key or a foot pedal (a pedal that types a key).
- **Translation** (Fluent), and stating clearly which languages the models handle.

---

## Standing risks

- **WebKitGTK frame transport fails the bar.** Fall back to JPEG at reduced resolution, or a native wgpu surface under a transparent webview as a last resort.
- **WebKitGTK and GPU variation across distros.** Mitigate with Flatpak, documented environment workarounds and the Canvas2D fallback.
- **Onboarding friction.** Developer options and the Windows adb driver mean QR pairing should be promoted as the default. The certificate warning remains; a real certificate via a public wildcard DNS name is a possible future step. WebRTC has lower resolution than scrcpy and no zoom or torch.
- **Signing costs and CI secrets.** About $99/yr for Apple plus a Windows certificate.
- **Licensing.** Redistributing adb, and FFmpeg if ever bundled (LGPL). Keep `NOTICE` current.
- **The ort WebGPU concurrency bug (`RUNTIME` lock) and `GPU_LOST`** may behave differently on D3D12/Metal. Repeat the GPU-loss recovery test per OS.
- **npm in a Rust repo.** Pin Node and keep `squigl-desktop` out of `default-members`.

---

## Critical files

- `crates/squigl-egui/src/app.rs`: split in Phase 1c (geometry, render, typeset; done) and Phase 7 (orchestration).
- `crates/squigl-engine/src/stream.rs`: gains `SourceSpec`.
- `crates/squigl-models/src/{lib,models,layout}.rs`: lazy preparation and `ModelPhase`.
- `crates/squigl-engine/src/transcribe.rs`: `Config` goes to `squigl_engine::config` via `directories`.
- `crates/squigl-egui/build.rs`: OS-conditional rpath.
- `phone-cam4linux/src/{lib,sink,loopback,session,webrtc_source,error,adb}.rs`: become `squigl-core` and `squigl-v4l2`; adb gets `locate()`.
- `crates/squigl-cli/src/{main,webrtc_server}.rs`: become `squigl-pairing`.
- `.github/workflows/{ci,release}.yml`: paths, OS matrix, desktop job.
- `CLAUDE.md`, `README.md`, `NOTICE`.

## Verification (applies to every phase)

- `cargo fmt --all -- --check`, plus `cargo clippy --workspace --all-targets [--features ffmpeg] -- -D warnings` and `cargo test --workspace [--features ffmpeg]`, all green.
- The scripted egui run still works, e.g. `squigl --rotate 270 --dev-detect --dev-read-all --screenshot-after 8 --screenshot-path /tmp/gui.png`. Compare it against the phone, or against the `Replay` source once that exists.
- `squigl-cli --test-pattern --device /dev/video10`, and grabbing a frame with `gst-launch-1.0`, exercise the V4L2 crate.
- After any ort or model change: `cargo test -p squigl-models --release glmocr_handwriting -- --ignored --nocapture`.
- From Phase 4: Vitest, `tauri-driver` end-to-end against `Replay` and `Image`, the per-mode pixel check, and the OS-magnifier, scaling and keyboard checks listed in each phase's exit criteria.
- From Phase 8: speech checks on each OS (a reading spoken, maths spoken through MathCAT, page read in block order).
- Tester sessions at the phases that name them: low-vision users (4, 8) and teachers (8, 10).
- From Phase 6: install tests on clean machines for each OS.
