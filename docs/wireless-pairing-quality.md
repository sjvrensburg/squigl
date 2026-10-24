# Wireless pairing: why the picture can be poor — investigation

Branch: `investigate/wireless-pairing-quality`. Status: investigation plus one
bug fix (§8.6). Claims are marked **[confirmed]** (read in code, or observed by
running it), **[in str0m]** (read in the pinned `str0m 0.23.1` sources), or
**[hypothesis]** (needs the measurement in §4). §8 reports what the connected
handset actually did; where §3 and §8 disagree, §8 wins.

## 1. The two paths, side by side

The ADB path and the wireless (WebRTC) path deliver the same `YuvFrame`s to the
same sinks, but they control the encoder completely differently:

| Aspect | ADB / scrcpy (`CameraSession`) | Wireless (`capture.html` + `WebrtcSource`) |
|---|---|---|
| Encoder | scrcpy-server (Camera2 + MediaCodec on the phone) | The phone browser's WebRTC encoder |
| Bitrate | Explicit: `--bitrate` defaults to **30 Mbit/s** in both binaries (`crates/squigl-cli/src/linux.rs:70`, `crates/squigl-egui/src/main.rs:57`), passed as `video_bit_rate=` (`crates/squigl-core/src/session.rs:186`) | **Nothing sets it anywhere.** No `maxBitrate`, no SDP `b=AS:`, no REMB request (§3.1) |
| Resolution | GUI asks for the camera's largest usable size (`Resolution::Max`, `crates/squigl-engine/src/stream.rs:682`); CLI takes `--resolution` | `width/height: {ideal: 1920/1080}` only; the engine's resolution setting is silently ignored for `SourceSpec::Network` (§3.4) |
| Frame rate | `max_fps=` when configured (`session.rs:183`) | No `frameRate` constraint, no `maxFramerate` (§3.3) |
| Transport | TCP via `adb forward` (USB, or TCP/IP ADB): loss is retransmitted, stream arrives intact | UDP/SRTP over Wi-Fi: loss is loss (§3.2, §3.5) |
| Decode failure | 300 consecutive failures end the session with a clear error; the worker reconnects (`session.rs:304`, `stream.rs:549`) | Failure is logged at *debug* level and the frame is dropped; nothing asks for a new keyframe (§3.2) |
| Observability | Throughput report every 5 s: packets, Mbit/s, fps (`session.rs:437`) | No statistics at all (§3.6) |

That asymmetry is the story: on the wireless path squigl asks a browser —
whose defaults are tuned for *video calls between people*, not for reading
handwriting — and then accepts whatever it sends, with no feedback except what
str0m generates automatically. The rest of this document ranks the specific
places quality leaks away.

## 2. Ruled out (checked, not causes)

- **Decoder ceiling:** openh264's level-5.2 cap (`crates/squigl-core/src/decode.rs:20`)
  rejects > 3840×2160-class SPS; browser streams are ≤ 1080p in practice, so
  this does not bite wireless (it can bite the ADB path at `--resolution max`).
- **Datagram truncation:** `RECV_BUF` = 2000 (`webrtc_source.rs:42`) vs.
  ~1200–1500 B RTP packets — fits (see §3.5, low risk).
- **Post-decode pipeline:** conversion, cropping, display LUT and OCR input
  are shared with the ADB path (`convert.rs`, engine `render_planes`), so
  they cannot explain a wireless-*only* quality gap.
- **The scaling in the GUI preview** decimates for display on both paths.

## 3. Findings, ranked by likely impact on legibility

### 3.1 The encoder bitrate is nobody's decision — likely the main cause

**[confirmed]** Nothing in `crates/squigl-pairing/src/capture.html` touches
`RTCRtpSender` parameters: no `sender.setParameters()` with `maxBitrate` /
`maxFramerate`, no `degradationPreference`. Nothing in
`WebrtcSource::accept_offer` (`crates/squigl-core/src/webrtc_source.rs:194`)
adds a bandwidth hint either: a probe (FakePhone offer through
`accept_offer`, since removed) shows the answer has **no `b=AS:`/`b=TIAS:`
line**. str0m does negotiate `transport-cc` and `goog-remb` feedback and the
TWCC header extension (extmap 3 in the probe answer), and str0m enables TWCC
feedback when both appear **[in str0m: `change/sdp.rs:1240`]** — so Chrome's
congestion control (GCC) does get a signal and will ramp up from its low
starting bitrate on a clean LAN. But:

- GCC targets *available network bandwidth as it dares to use it*, reacting to
  delay and loss by backing off. On lossy Wi-Fi it settles conservatively —
  typically single-digit Mbit/s, and it can sit far lower right after any
  loss burst. The ADB path's fixed 30 Mbit/s has no such feedback loop.
  **[hypothesis — measure per §4.1]**
- squigl never calls str0m's `request_remb` (it exists on the receive stream
  **[in str0m: `streams/receive.rs:231`]**, reachable via `direct_api()`),
  so if TWCC is ever not negotiated (a browser that doesn't offer it, a
  future regression), there is no fallback floor at all and the sender stays
  near its starting bitrate.
- At 1080p, handwriting stays legible at roughly ≥ 8–10 Mbit/s; at
  1–2 Mbit/s a browser encoder smears strokes and quantises text blocks
  aggressively, especially with motion (hand moving, page being positioned).
  This matches "wired is crisp, wireless is mushy" as the expected default
  outcome, not an anomaly.

**Fix direction:** in `capture.html`, after `setRemoteDescription`, call
`sender.setParameters()` with a generous `maxBitrate` (e.g. 20–30 Mbit/s to
mirror the ADB default) and `degradationPreference: 'maintain-resolution'`;
optionally also request REMB from str0m as a floor. Verify with `getStats`
(§4.1) before/after.

### 3.2 One lost packet damages the picture until the next keyframe — and squigl never asks for one

**[confirmed]** In `webrtc_source.rs:295-304`, a `MediaData` event is fed to
the decoder; on `Err` the code logs at debug level ("skipping undecodable
WebRTC frame") and carries on. It:

- never requests a keyframe. str0m supports PLI/FIR from the receiver
  **[in str0m: `streams/receive.rs:224`, `change/direct.rs` `stream_rx()`]**,
  and squigl's negotiated SDP advertises `nack pli` / `ccm fir`, but no code
  path calls it.
- ignores `MediaData::contiguous` and `is_keyframe()` **[in str0m:
  `media/event.rs:252`, `:285`]**, which exist precisely to say "this frame
  follows a gap; the decoder's reference chain is broken".
- uses the default (openh264) decoder, whose error concealment is minimal:
  after a lost reference, subsequent P-frames decode to smearing/grey blocks
  until an IDR arrives.

Browser encoders emit keyframes on PLI and otherwise infrequently (seconds
apart, encoder-dependent **[hypothesis]**). So on real Wi-Fi — where 1–5 %
UDP loss is unremarkable, and a keyframe spans 100+ packets (§3.5), so *most*
keyframes lose a packet somewhere — the expected steady state is: frequent
multi-second stretches of corrupted picture between clean keyframes. NACK/RTX
retransmission is negotiated and str0m generates NACKs automatically
**[in str0m: `streams/receive.rs:658`]**, which repairs *some* loss, but a
retransmit that arrives after the depacketiser gave up on the frame does not
help (§3.5), and nothing escalates to PLI when repair fails.

Note the contrast with the ADB path: TCP retransmits, so this failure mode
does not exist there at all.

**Fix direction:** track decode failures and non-contiguous frames; after a
failure (or immediately on `!contiguous` for a non-keyframe), request a
keyframe via PLI (rate-limited, e.g. at most 1/s). A cheap approximation that
needs no str0m API plumbing: count consecutive decode failures and log at
warn level so the problem is at least visible (§3.6).

### 3.3 The browser optimises for faces in motion, not for a page of text

**[confirmed]** `capture.html` sets no `track.contentHint`, no
`degradationPreference`, no sender `maxFramerate`. The browser defaults are
therefore in charge **[hypothesis, per-browser values to measure]**:

- `degradationPreference` defaults to `balanced` in Chrome: under CPU or
  bandwidth pressure the encoder drops *resolution* (and/or frame rate).
  For a document camera, resolution is the product; the right setting is
  `maintain-resolution`.
- Without `contentHint = 'detail'`, Chrome's encoder treats the feed as
  generic motion video. `detail` biases it toward preserving sharp edges
  (text) and accepting a lower frame rate — exactly the trade a document
  camera wants.
- No frame-rate cap: the browser will happily encode 1080p30. Thirty frames
  a second of a mostly still page spends the (§3.1) bitrate budget on
  near-duplicate frames; 5–10 fps at the same bitrate would give each frame
  3–6× the bits. The ADB path's still scenes compress so well this hardly
  matters at 30 Mbit/s; at browser bitrates it decides whether strokes
  survive.

**Fix direction:** `track.contentHint = 'detail'`;
`sender.setParameters({ degradationPreference: 'maintain-resolution',
encodings: [{ maxBitrate, maxFramerate: 15 }] })`. All are one-liners in
`capture.html`; measure with §4.1.

### 3.4 The capture resolution itself is only a wish

**[confirmed]** `getUserMedia` is asked for `width: {ideal: 1920}, height:
{ideal: 1080}` (`capture.html:35`). `ideal` means the browser may return
anything — 1280×720 or 640×480 on devices/browsers that prefer it — and
nothing checks what arrived: `track.getSettings()` is read on the control
channel only for `zoom`/`torch` (`capture.html:93`), never for width/height/
frameRate, and it is never logged. On the squigl side the session has no
notion of a requested size at all: `run_paired` accepts "the browser's
choice" (`crates/squigl-engine/src/stream.rs:750`), and the user's
resolution setting (`Resolution::Max` in the desktop app,
`crates/squigl-desktop/src/main.rs:524`) applies only to the ADB source.
A user who paired wirelessly and sees a soft picture may simply be receiving
720p upscaled by the renderer — indistinguishable from encoder mush without
the diagnostics in §3.6.

Related: the answer SDP keeps *all* of the browser's H.264 payload types in
the browser's order (14 in the probe), including `packetization-mode=0`
variants and every profile at level `1f` (3.1) — e.g.
`profile-level-id=42001f`. Chrome in practice sends 1080p labelled 42e01f, so
this is probably benign **[hypothesis]**, but it means squigl expresses no
preference for packetization-mode 1 or a 1080p-capable level; worth one
`getStats`/Wireshark check (§4.3) rather than assuming.

**Fix direction:** log `track.getSettings()` (width/height/frameRate) on the
page and in the control-channel report; consider `min`/exact constraints
with an OverconstrainedError fallback.

### 3.5 Burst loss on the receiving socket, and a small reorder window

**[confirmed]** from code:

- `webrtc_source.rs` receives on a plain `UdpSocket` with the OS-default
  receive buffer; nobody raises `SO_RCVBUF`. The receive loop is
  single-threaded and *synchronous through the sink*: `next_frame` returns a
  decoded frame, `run` calls `sink.frame(&frame)` before reading again, and
  the engine's `deliver` (`stream.rs:845`) clones the frame (≈3 MB at 1080p)
  and locks the shared slot on that same thread. While any of that runs,
  incoming datagrams queue in the kernel buffer; a 1080p keyframe burst at
  LAN speed is hundreds of packets in a few ms. Overflow drops packets —
  preferentially destroying keyframes, which is §3.2's trigger. TCP (ADB
  path) back-pressures instead of dropping.
- str0m's video reordering window defaults to **30 packets**
  **[in str0m: `config.rs` `set_reordering_size_video`, "Default: 30" … "must
  be at least as big as the number of packets the biggest keyframe can be
  split over"]**. squigl doesn't raise it. A low-bitrate 1080p keyframe can
  exceed 30 packets easily; under Wi-Fi jitter/reordering (2.4 GHz, power-
  save buffering on the phone, overlay networks) str0m then releases frames
  with gaps — `contiguous: false` — which §3.2 shows squigl feeding to the
  decoder anyway.
- `RECV_BUF` is 2000 bytes (`webrtc_source.rs:42`). Normal RTP packets
  (≤ ~1500 B) fit; anything larger would be silently truncated by
  `recv_from`. Low risk, noted for completeness.

**Fix direction:** raise `SO_RCVBUF` (e.g. 4 MiB via a small setsockopt —
note squigl-core currently has no socket2 dependency), consider
`set_reordering_size_video(200+)`, and decouple receive from sink delivery
if measurements (§4.2) show drops.

### 3.6 There is no way to see any of this — zero observability

**[confirmed]** The ADB path logs packets/Mbit/s/fps every 5 s; the wireless
path logs nothing about the stream after "a phone paired". str0m's stats
interval is never enabled, decode failures are `log::debug!`, the page never
calls `getStats()`, and the negotiated resolution/fps/bitrate appear in no
log or UI. Any of §3.1–2.5 could be the whole story for a given user and
today there is no evidence either way. This is why the investigation leans
on code reading: the measurements in §5 don't exist yet.

**Fix direction:** mirror `StreamStats` into `webrtc_source.rs` (frames,
bytes, decode failures, non-contiguous count, 5 s cadence); print
`getStats` essentials (`frameWidth/Height`, `framesPerSecond`,
`totalPacketSendDelay`-free sender bitrate estimate) on the pairing page's
status line or the data channel.

### 3.7 Phone-side and network factors (context, mostly not squigl's code)

- **Thermal/CPU adaptation [hypothesis]:** software H.264 encoding of 1080p
  plus the live preview heats many phones; browsers respond by stepping
  resolution/fps down over minutes (`balanced`, §3.3). Symptom signature:
  starts acceptable, degrades while streaming. `maintain-resolution` +
  lower `maxFramerate` reduce the heat itself.
- **The overlay networks squigl advertises** (Tailscale/ZeroTier/Nebula,
  `crates/squigl-pairing/src/addresses.rs`): media goes over the same address
  the offer came in on. If that path is a relay (e.g. Tailscale DERP) or a
  congested tunnel, jitter/loss rise sharply vs. LAN. Pairing docs cover
  firewalls, not media quality per network.
- **Wi-Fi power save / 2.4 GHz congestion:** bursty delivery (DTIM
  buffering) feeds §3.5; nothing to fix in squigl beyond tolerating bursts.
- **Focus and exposure are the browser's:** `getUserMedia` gives squigl no
  focus/exposure controls (only zoom/torch are wired). Continuous AF hunting
  on a close page can look like "poor quality" but is not an encoding
  problem; check by comparing a frozen capture's sharpness centre vs edges.
  The ADB path has the same class of behaviour via Camera2, so this is a
  less likely *differential* cause.
- **Orientation:** a phone held portrait sends portrait video; a landscape
  page then occupies a fraction of the frame's pixels. Effective resolution
  on the page drops even though the stream is "1080p". Squigl cannot rotate
  for the phone, but §3.4's settings log would make it obvious.


## 4. How to confirm or kill each hypothesis

Ordered cheapest-first. §4.1 alone probably settles which of §3.1/§3.3/§3.4
dominates.

### 4.1 Ask the browser what it is actually sending (settles §3.1, §3.3, §3.4)

On the phone (or a desktop browser paired via `--dev-pairing-bind 127.0.0.1`),
after a minute of streaming, run in the page context / add temporarily to
`capture.html`:

```js
const stats = await pc.getStats();
for (const r of stats.values())
  if (r.type === 'outbound-rtp' && r.kind === 'video')
    console.log(r.frameWidth, r.frameHeight, r.framesPerSecond,
                r.encoderImplementation, r.qualityLimitationReason);
const [sender] = pc.getSenders();
console.log(JSON.stringify(sender.getParameters()));
console.log(JSON.stringify(track.getSettings()));
```

Readings to look for: `frameWidth/Height` below 1920×1080 → §3.4/§3.3;
`qualityLimitationReason: 'bandwidth'` → §3.1; `'cpu'` → §3.7;
bitrate from two `outbound-rtp` snapshots 10 s apart (`bytesSent` delta)
under ~4 Mbit/s at 1080p → §3.1 confirmed as the legibility ceiling.
Repeat after 10 minutes: values that decay over time → thermal (§3.7).

### 4.2 Count what the receiver throws away (settles §3.2, §3.5)

Temporarily raise the decode-failure log in
`crates/squigl-core/src/webrtc_source.rs:301` to `warn`, and log
`data.contiguous` / `data.is_keyframe()` per MediaData for one session:
correlate bursts of failures with hand/page movement (encoder stress) vs.
with keyframes (burst loss). A cleaner permanent version is §3.6's fix.
Kernel-side corroboration on Linux while streaming: `nstat -az UdpRcvbufErrors`
(or `netstat -su`) before/after — a rising receive-buffer-error count proves
§3.5's socket drops outright.

### 4.3 Check the negotiated codec (settles the §3.4 SDP question)

Capture one real Chrome offer/answer (log the SDP in `PairingServer`'s whip
handler, or chrome://webrtc-internals on the phone's browser where available):
which payload type was selected, its `packetization-mode` and
`profile-level-id`, and whether `transport-cc` survived. Compare against the
FakePhone probe in §3.1 (str0m-to-str0m is not evidence about Chrome).

### 4.4 A/B the transport (isolates Wi-Fi from encoder)

Pair a *desktop* browser on wired Ethernet (or same-machine via
`--dev-pairing-bind 127.0.0.1`) showing a printed test page to the camera, then
the phone on Wi-Fi from the same spot. If wired is crisp and Wi-Fi is not at
equal `getStats` bitrates, transport loss (§3.2/§3.5) dominates; if both are
equally soft, it's encoder settings (§3.1/§3.3/§3.4).

## 5. Suggested fix order (for a follow-up branch)

1. **Instrument first** (§3.6): receiver stats in `webrtc_source.rs` +
   `getSettings()` in the control-channel report. One small change; makes
   every later fix measurable.
2. **Tell the browser what squigl wants** (§3.1, §3.3, §3.4): `contentHint
   = 'detail'`, `setParameters` with high `maxBitrate`,
   `maintain-resolution`, a `maxFramerate` around 10–15, all in
   `capture.html`. Highest expected gain per line changed.
3. **Recover from loss** (§3.2): PLI on decode failure / non-contiguous
   frame (rate-limited), via str0m's direct API.
4. **Harden reception** (§3.5): larger `SO_RCVBUF`, larger
   `set_reordering_size_video`.
5. Re-run §4.1/§4.2 and compare; only then consider deeper work (e.g.
   honouring the engine's resolution setting for paired phones, §3.4).

## 6. Method and limits

Read in full: `crates/squigl-pairing/src/capture.html`,
`crates/squigl-pairing/src/lib.rs`, `crates/squigl-core/src/webrtc_source.rs`,
`crates/squigl-core/src/session.rs` (ADB baseline), `decode.rs`,
`crates/squigl-engine/src/stream.rs`, `crates/squigl-cli/src/webrtc_server.rs`,
plus the pinned `str0m 0.23.1` config/session/streams sources in the local
cargo registry. Ran: a throwaway FakePhone probe dumping the offer/answer SDP
through the real `accept_offer` (probe deleted afterwards; `git status` shows
only this file). Not done: no real phone/browser was available to this
investigation, so all browser-behaviour claims are marked [hypothesis] with
their falsification test in §4. The existing test suite was run to confirm
the branch is otherwise untouched (§7).

## 7. Verification of this branch

- `cargo test -p squigl-pairing -p squigl-core` — see final report.
- Only change vs. `main`: this document (plus the branch itself).

## 8. Measured on the connected handset (2026-10-24)

Rig: Samsung SM-A307FN (Galaxy A30s, Exynos 7904), LineageOS/Android 13,
Wi-Fi 5 GHz, RSSI −25 dBm, 433 Mbit/s link, phone 192.168.1.53, PC
192.168.1.169 (wired). Phone aimed at the same handwritten notebook page for
every capture. The pairing server was the user's own
`squigl-cli --webrtc` process; because this PC's firewalld rejected the phone
until its address was trusted (the [pairing.md](pairing.md) case, confirmed:
ping 1.7 ms, TCP "No route to host"), and because Jelly (below) could not be
used, the page was reached in Firefox through a local TLS proxy that only
replaced the certificate — page, WHIP endpoint and `WebrtcSource` were the
shipped code. ADB (USB) drove the phone throughout.

### 8.1 ADB baseline

`--list-sizes`: back camera up to 3840×2160, 15–30 fps, zoom 1–4×.
A 1920×1080 frame grabbed from `/dev/video10` is sharp; every handwritten
line is legible. This is the quality the wireless path is compared against.

### 8.2 The stock Lineage browser cannot pair at all

Jelly (the phone's only preinstalled browser, WebView 119):
- Its manifest does not request `android.permission.CAMERA`, so WebView can
  never grant `getUserMedia` — pairing is impossible regardless of the page.
- It renders squigl's self-signed page as a blank black screen. Root cause
  found while working around it: the rcgen certificate
  (`crates/squigl-pairing/src/lib.rs` `generate()`) has **no BasicConstraints
  extension at all** (verified with `openssl x509 -ext`), so Android's
  Conscrypt refuses it as a trust anchor even after manual installation into
  the system CA store (`CertificateUnknown` in the server log on every
  attempt). Chrome-class browsers show the usual one-tap interstitial, so
  this mainly bites WebView-based browsers — but those are exactly the
  default browser on de-Googled ROMs. Filed as a real (minor) finding:
  the generated cert should carry `CA:TRUE`/SAN properly (it does carry the
  IP SAN).

### 8.3 Firefox: VP8 works, and shows the browser-default quality ceiling

Loopback self-test (no squigl code; same `getUserMedia` constraints as
`capture.html`): capture track 1080×1920 portrait @60 (reported), encoder
output **1072×1920 at ~13 fps, 756,639 bytes in 8 s ≈ 0.76 Mbit/s**.
So Firefox honoured the requested resolution (the §3.4 worry does not apply
here), but its default encoder budget for a 2-megapixel still page is under
1 Mbit/s — handwriting at that rate is mush. This confirms §3.1/§3.3's
mechanism with a number: with no `maxBitrate`, `contentHint` or
`degradationPreference` from squigl, the browser's video-call defaults
decide, and they decide low.

### 8.4 Firefox + H.264 (squigl's actual configuration): no video at all

Same self-test with `setCodecPreferences(h264)` exactly as `capture.html`
does: negotiated H.264 (pt 126, `profile-level-id=42e01f`,
packetization-mode=1), then **`outbound: 0x0 fps=0 bytes=0 frames=0`** —
the encoder never produced a single frame, loopback, no network, no squigl.
The phone does have H.264 MediaCodec encoders (`OMX.Exynos.AVC.Encoder`,
`c2.android.avc.encoder`); the failure is in Firefox/Gecko's WebRTC encoder
path on this device/ROM. Chromium (snapshot build) also yielded no usable
run here (its process kept crashing on camera start), so the Chrome-class
path is **unverified on this handset**, not exonerated.

Against the real server this presented as: DTLS/SRTP up, control channel up
(camera report received: `zoom_range: None, torch: None` — Firefox exposes
neither), zero `MediaData`, and after exactly 10 s
`Error::StreamStalled` ("no data from the phone for 10s"), session over; the
page showed `connection: connected → disconnected → failed`. One early run
did emit frames — 320×240 black, then a resize to 176×320 — which is what
exposed §8.6. Conclusion: squigl's **H.264-only** codec policy is a hard
compatibility cliff, not merely a quality knob. Where a browser's H.264
encode path is broken (or absent), wireless pairing delivers nothing, and
neither side says why: the page logs "streaming", squigl logs a stall.
§3.6 (no observability) is what turned this into a long hunt.

### 8.5 Network/transport checks

UDP `RcvbufErrors` stayed 0 and inbound traffic matched expectations
throughout; Wi-Fi was strong (above). §3.2/§3.5 remain code-level risks for
lossy networks, unexercised here because media never flowed long enough.

### 8.6 Bug found and fixed: CLI session died on any mid-stream size change

The 320×240 → 176×320 resize ended the CLI session:
`WebRTC session ended: V4L2 sink error: frame is 176x320, sink was opened at
320x240`. `LazyV4l2Sink` (`crates/squigl-v4l2/src/sink.rs`) opened once and
never followed a size change, although `WebrtcSource` has no session-meta
size and the engine's tee already reopens on change
(`crates/squigl-engine/src/stream.rs` `open_tee`). Browsers *do* change size
mid-stream (orientation, renegotiation), so `--webrtc` CLI sessions were
fragile by construction. Fix on this branch: reopen the sink at the new
size, mirroring `open_tee`; regression test
`sink::tests::lazy_sink_follows_a_mid_stream_size_change` (uses the real
sizes above; fails before, passes after; skipped without a loopback
device). `cargo test -p squigl-v4l2`, `cargo clippy -p squigl-v4l2
--all-targets -- -D warnings`, and `cargo fmt --all -- --check` all pass.
Re-verification (after the user restarted the server on the fixed binary):
three Firefox pairing attempts each stalled at 10 s with zero frames and —
critically — no sink error, and Chromium (snapshot build) was unusable on
this ROM (renderer hung/crashed around camera start). So the resize path
could not be exercised live again: the only browser that ever produced
frames here no longer encodes H.264 at all (§8.4). The fix rests on the
regression test against the real `/dev/video10`; treat the live resize
scenario as test-verified but not re-verified on a phone.

### 8.7 Revised ranking for this device

1. H.264-only negotiation (§8.4): total failure where browser H.264 encode
   is broken; silent on both sides.
2. Browser-default encoder budget (§8.3): ~0.76 Mbit/s at 2 MP when it does
   encode — the "poor image" users would see on a working browser.
3. CLI sink size-change death (§8.6): fixed here.
4. Pairing reachability friction (firewalld default-deny, §8 preamble;
   Jelly, §8.2): not quality, but indistinguishable from it in user reports
   ("wireless doesn't work / picture never comes up").
5. §3.2/§3.5 loss-recovery and buffering: still open, still unmeasured.

## 9. Fixes implemented on this branch

Following §5's order (and §8.7's re-ranking):

1. **Browser told what squigl wants** (`capture.html`): `contentHint =
   'detail'`; sender parameters `maxBitrate` 20 Mbit/s, `maxFramerate` 15,
   `degradationPreference = 'maintain-resolution'`; H.264 codec preferences
   sorted packetization-mode 1 first; the negotiated camera settings are
   logged on the page. *Not yet measurable against a working encoder on
   this handset (see below).*
2. **Silent encoder failure now speaks** (`capture.html` `watchEncoder`,
   `webrtc_source.rs`): the page checks `getStats` after connecting and, if
   no frame was encoded, says so ("this browser is producing no video… try
   Chrome"); the receiver logs distinct warnings for "never connected"
   (firewall/network) vs. "connected but sent no video" (browser encoder).
   **Verified live** on the §8 handset: both messages appeared, server side
   at 11 s into the stalled session, page side on its status log, where
   before there was only "video stream stalled".
3. **Observability**: the receiver keeps ADB-style stream statistics
   (media packets, Mbit/s, decoded fps, decode failures, non-contiguous
   count) logged every 5 s at debug level.
4. **Loss recovery**: PLI keyframe requests on decode failure and on
   non-contiguous non-keyframe media, rate-limited to 1/s (`PliGate`,
   unit-tested); reordering window raised 30 → 300 packets; media socket
   receive buffer raised to 4 MiB (best-effort). *Untested against live
   loss — no working encoder was available; unit + compile coverage only.*
5. **CLI sink follows size changes** (§8.6, regression-tested).
6. **Certificate is a CA** (`squigl-pairing` `generate`): BasicConstraints
   CA:TRUE, unit-tested at the DER level; interstitial behaviour unchanged
   for Chrome-class browsers.

Gates: full `cargo test` (default members) green, `cargo clippy` with
`-D warnings` on the touched crates green, `cargo fmt --check` green,
rustdoc (`-D warnings`) for the touched libraries green. A workspace-wide
clippy run was blocked by a crates.io 500 on an unrelated crate download,
not by code.

Addendum (Chrome retest, same day): Chromium (main-branch build, left
installed on the phone alongside Firefox) paired with the fixed server and
behaved identically to Firefox — camera 1920x1080@60 reported, connection
up, `watchEncoder` error on the page, "connected but sent no video" on the
server, stall at 10 s. Two independent browser engines failing the same way
moves the H.264-encode failure from "Firefox/Gecko quirk" to "this
device/ROM's WebRTC encoder path": the OS encoder itself is fine
(`screenrecord` produces valid 720x1560 H.264), so the breakage is in how
both WebRTC stacks configure it on this Lineage build. A real Google Chrome
from the Play Store (absent here) uses the same WebRTC encoder glue as
Chromium, so expectations for it on this handset should be low; on stock
devices that glue is exactly what works. Squigl-side, nothing further can
be squeezed from this handset — the remaining lever for such devices is
codec fallback (VP8/VP9 decode), a product decision the report's §5 did not
originally contemplate.
