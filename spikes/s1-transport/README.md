# Spike S1: frame transport

This spike answers one question from `docs/roadmap.md`: how should Rust send camera frames to the Tauri webview? It is a throwaway Tauri 2 app, in its own Cargo workspace, so it is not part of squigl's build or CI.

## What it does

**The source.** Rust generates synthetic luma frames, sized to a target number of bytes. Each frame carries a 32-byte header (magic, frame number, send time) and has its frame number burned into the top rows as 32 black and white squares.

**The transports.** The page pulls frames over one of three:

| Transport | How the page asks |
|---|---|
| `scheme` | `fetch("frame://localhost/next")`, served by `register_asynchronous_uri_scheme_protocol`. On Windows the URL is `http://frame.localhost/next`. |
| `ipc` | `invoke("next_frame")`, which returns a `tauri::ipc::Response` of raw bytes. |
| `ws` | A localhost WebSocket (tungstenite), one text message per request. |

**What is measured.**
- **Drawing:** each frame is uploaded as a WebGL2 `R8` texture and drawn, with `gl.finish()`, so the GPU upload is included in the timing.
- **Latency:** from the send time to after the draw.
- **Integrity:** the page checks that the burned-in frame number matches the header (`corrupt`), and counts frames that arrive out of order (`reordered`).
- **CPU:** summed over the app and every process it spawned (WebKit's web and network processes), from `/proc`. Linux only.
- **Requests:** two are kept in flight. With `S1_FPS` set, requests are spaced at that rate; otherwise each is made as soon as the last frame is drawn.

**The pass bar** (from the roadmap): at 30 fps, a frame of 3 MB or more keeps 30 fps, with p95 latency ≤ 50 ms, on under one core in total.

## Running it

Needs WebKitGTK 4.1 development files on Linux: `webkit2gtk4.1-devel` on Fedora, `libwebkit2gtk-4.1-dev` on Ubuntu. No npm is needed; the page is static and uses Tauri's global JS API.

```bash
./run.sh          # every transport x 1/3/5.5/12 MB x max/30 fps, under Wayland and X11
./run.sh 4        # 4 s per run instead of 8 (the whole sweep takes about 10 minutes at 8)
python3 summarize.py results-*.jsonl
```

- **One run:** `S1_TRANSPORT=ws S1_BYTES=3000000 S1_FPS=30 S1_SECONDS=8 src-tauri/target/release/s1-transport`
- **Results:** each run appends a line to `results-<host>.jsonl`. Bring that file back to compare machines.
- **The test window** opens and closes once per run; leave it alone while the sweep runs.

## Results

### Fedora 44, AMD Ryzen AI Max+ 395 with Radeon 8060S, WebKitGTK 2.54 (2026-10-04)

At 30 fps, by transport and frame size. Each cell shows cores used, then p95 latency.

| Transport | 1 MB | 3 MB | 5.5 MB | 12 MB |
|---|---|---|---|---|
| `ws` | 0.14, 2.0 ms | 0.21, 4.2 ms | 0.31, 7.7 ms | **0.57, 15.8 ms** |
| `scheme` | 0.23, 2.0 ms | 0.46, 4.5 ms | 0.75, 8.4 ms | 1.58, 19.1 ms (**fails**: CPU) |
| `ipc` | 0.24, 2.1 ms | 0.47, 4.6 ms | 0.74, 7.6 ms | 1.56, 19.3 ms (**fails**: CPU) |

The table is for Wayland. X11 is within a few per cent, except IPC, which is slower there (p95 9.6 ms at 3 MB).

As fast as each goes, at 3 MB: `ws` 695 fps, `scheme` 271 fps, `ipc` 250 fps. At 12 MB: 127, 71 and 55 fps.

No frame was corrupted in any run. Frames arrived out of order only with `scheme` when unthrottled, because two requests in flight are answered by separate threads. A real transport numbers its frames and drops a stale one.

**Verdict for Linux:**
- **The WebSocket.** It is the only transport that passes at 12 MB (a 4000x3000 frame), and it costs about half the CPU of the others at every size. That is the roadmap's default; nothing here argues for opening no port instead.
- **The custom scheme and IPC** are fine up to about 5 MB, which is enough for a decimated magnifier view. They spend about 3 times the CPU of the WebSocket per byte.

**Software rendering (llvmpipe, a first S3 data point).** With `LIBGL_ALWAYS_SOFTWARE=1`, WebGL2 still works. The WebSocket at 3 MB holds 30 fps (p95 5.5 ms), but costs 3.4 cores, against 0.21 on the GPU. A machine with no working GPU driver therefore needs smaller frames (a bigger step) or the Canvas2D path.

### Still to run

- **The Ubuntu box with the Nvidia GPU.** This is the most likely WebKitGTK trouble spot. Run `./run.sh` there and bring back `results-<host>.jsonl`.
- **Windows and macOS.** These need *(tester)* hardware: the Windows VM has no GPU, so its numbers would not be representative.
