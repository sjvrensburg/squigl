//! The camera worker thread: connects to the phone (or plays a recording of it),
//! decodes, and publishes the latest frame for the UI, reconnecting with backoff when
//! the stream drops (the same policy as the `squigl-cli` CLI). Optionally tees every
//! frame to a V4L2 device too.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use squigl_core::cameras::is_usable_size;
use squigl_core::convert::rgba_to_i420;
use squigl_core::decode::YuvFrame;
use squigl_core::sink::FrameSink;
use squigl_core::{
    adb::AdbDevice, CameraControl, CameraInfo, CameraSession, ConnectOptions, Facing, Replay,
    TestPattern, WebrtcControl, WebrtcSource, ZOOM_STEP,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What the worker is doing, for the status bar.
#[derive(Debug, Clone)]
pub enum Status {
    Connecting,
    Streaming {
        width: u32,
        height: u32,
    },
    /// The last attempt failed (or the stream ended); retrying after the backoff.
    Waiting {
        reason: String,
        retry_at: Instant,
        /// What a person can do about it, when it is something they can.
        problem: Option<Problem>,
    },
    Stopped,
}

#[derive(Debug, Clone, Copy)]
pub enum Resolution {
    PhoneDefault,
    Max,
    Fixed(u32, u32),
}

/// Why a source is not delivering, in terms of what a person can do about it: a
/// front end's guidance follows it. Worked out from the error by [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Problem {
    /// Android's adb tool is not installed (or not found).
    AdbMissing,
    /// No phone is connected, or USB debugging is off.
    NoPhone,
    /// The phone is waiting for its user to allow USB debugging.
    Unauthorized,
    /// The phone is connected but not answering.
    Offline,
    /// More than one phone is connected.
    SeveralPhones,
    /// Another app has the phone's camera.
    CameraInUse,
    /// The phone's Android is older than the camera needs (12).
    AndroidTooOld,
    /// The file to show or play is not there or cannot be read.
    FileMissing,
    /// Something else; the reason says what.
    Other,
}

/// What [`Problem`] an error from a source is.
pub fn classify(error: &anyhow::Error) -> Problem {
    use squigl_core::Error as E;
    for cause in error.chain() {
        if let Some(e) = cause.downcast_ref::<E>() {
            match e {
                E::AdbNotFound => return Problem::AdbMissing,
                E::NoDevice => return Problem::NoPhone,
                E::DeviceUnauthorized => return Problem::Unauthorized,
                E::DeviceOffline => return Problem::Offline,
                E::Recording(_) => return Problem::FileMissing,
                _ => {}
            }
        }
        if cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            || cause
                .downcast_ref::<image::ImageError>()
                .is_some_and(|e| matches!(e, image::ImageError::IoError(_)))
        {
            return Problem::FileMissing;
        }
    }
    // The rest is in words: adb's own messages, and the scrcpy server's output
    // folded into the error.
    let text = format!("{error:#}");
    if text.contains("multiple devices") || text.contains("more than one device") {
        Problem::SeveralPhones
    } else if text.contains("unauthorized") {
        Problem::Unauthorized
    } else if text.contains("device offline") {
        Problem::Offline
    } else if text.contains("not found") && text.contains("device") {
        Problem::NoPhone
    } else if text.contains("not supported before Android 12") {
        Problem::AndroidTooOld
    } else if text.contains("CAMERA_IN_USE") || text.contains("MAX_CAMERAS_IN_USE") {
        Problem::CameraInUse
    } else {
        Problem::Other
    }
}

/// Where the worker's frames come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "kebab-case")]
pub enum SourceSpec {
    /// The phone over ADB, per [`StreamConfig::options`] and `resolution`.
    Phone,
    /// A recording made with `squigl-cli --record`, looping.
    Replay(PathBuf),
    /// A still image -- a scanned or photographed page -- shown as one frame.
    Image(PathBuf),
    /// Synthetic colour bars.
    TestPattern,
    /// A phone's browser over WebRTC, paired by QR code: the sessions
    /// [`Shared::pair`] hands over, one after another.
    Network,
}

/// Which camera controls the current source has; a front end shows only these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub zoom: bool,
    pub torch: bool,
    pub facing: bool,
}

impl SourceSpec {
    pub fn capabilities(&self) -> Capabilities {
        let phone = *self == SourceSpec::Phone;
        Capabilities {
            zoom: phone,
            torch: phone,
            facing: phone,
        }
    }

    /// The source for a file a user picked or dropped: a recording (`.sqrec`) plays,
    /// anything else is opened as an image.
    pub fn for_file(path: PathBuf) -> Self {
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("sqrec"))
        {
            SourceSpec::Replay(path)
        } else {
            SourceSpec::Image(path)
        }
    }

    /// What to call it in a status line: "phone", or the file's name.
    pub fn describe(&self) -> String {
        match self {
            SourceSpec::Phone => "phone".to_string(),
            SourceSpec::Replay(path) | SourceSpec::Image(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into(),
            ),
            SourceSpec::TestPattern => "test pattern".to_string(),
            SourceSpec::Network => "paired phone".to_string(),
        }
    }
}

/// Everything the worker needs to (re)connect. Changing the facing or the source
/// takes effect on the next connection, which [`Shared::restart`] forces.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Where frames come from. The phone settings below are kept whichever it is,
    /// so going back to the phone picks them up again.
    pub source: SourceSpec,
    pub options: ConnectOptions,
    pub resolution: Resolution,
    /// Also write frames to this v4l2loopback device (Linux only; always `None`
    /// elsewhere).
    pub tee_device: Option<PathBuf>,
}

pub struct Shared {
    stop: AtomicBool,
    restart: AtomicBool,
    frames: AtomicU64,
    /// The newest frame and its number (the `frames` count when it arrived).
    latest: Mutex<Option<(u64, Arc<YuvFrame>)>>,
    status: Mutex<Status>,
    /// Bumped on every status change, so a watcher can tell without comparing.
    status_changes: AtomicU64,
    config: Mutex<StreamConfig>,
    /// The phone's camera listing, fetched once per worker (it costs a server
    /// round-trip) and reused for `max` resolution and the zoom range.
    cameras: Mutex<Vec<CameraInfo>>,
    /// The live session's control channel, while one is up.
    control: Mutex<Option<CameraControl>>,
    /// Zoom in [`ZOOM_STEP`] steps from 1.0: where the phone is believed to be (the
    /// control channel only moves relatively and the phone never reports back) and
    /// where the user wants it. A dedicated thread walks `applied` towards `target`
    /// one message at a time, so a fast slider drag queues one step, not fifty.
    zoom: Mutex<ZoomState>,
    zoom_changed: Condvar,
    /// A paired phone's session, from [`Shared::pair`] until the worker takes it.
    paired: Mutex<Option<WebrtcSource>>,
    /// The streaming paired phone's camera controls (its zoom and torch, which it
    /// reports itself: unlike the ADB phone's, nothing is tracked here).
    remote: Mutex<Option<WebrtcControl>>,
}

#[derive(Debug, Clone, Copy)]
struct ZoomState {
    applied: i32,
    target: i32,
}

/// The zoom grid position the server snaps `ratio` to.
fn zoom_to_steps(ratio: f32) -> i32 {
    (ratio.max(1.0).ln() / ZOOM_STEP.ln()).round() as i32
}

fn steps_to_zoom(steps: i32) -> f32 {
    ZOOM_STEP.powi(steps)
}

impl Shared {
    /// The most recently decoded frame, if any.
    pub fn latest(&self) -> Option<Arc<YuvFrame>> {
        self.latest
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, f)| Arc::clone(f))
    }

    /// The most recently decoded frame with its number (counting from 1 over the
    /// worker's life).
    pub fn latest_numbered(&self) -> Option<(u64, Arc<YuvFrame>)> {
        self.latest.lock().unwrap().clone()
    }

    /// How many times the status has changed; a different number means
    /// [`status`](Self::status) may say something new.
    pub fn status_changes(&self) -> u64 {
        self.status_changes.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Frames decoded since the worker started (all sessions).
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn facing(&self) -> Facing {
        self.config.lock().unwrap().options.facing
    }

    pub fn source(&self) -> SourceSpec {
        self.config.lock().unwrap().source.clone()
    }

    /// What the source can do; for a paired phone, what its camera reported.
    pub fn capabilities(&self) -> Capabilities {
        let source = self.source();
        if source != SourceSpec::Network {
            return source.capabilities();
        }
        let camera = self.remote().and_then(|r| r.camera());
        Capabilities {
            zoom: camera.and_then(|c| c.zoom_range).is_some(),
            torch: camera.and_then(|c| c.torch).is_some(),
            facing: false,
        }
    }

    /// The paired phone's controls, while it is the source and streaming.
    fn remote(&self) -> Option<WebrtcControl> {
        if self.source() != SourceSpec::Network {
            return None;
        }
        self.remote.lock().unwrap().clone()
    }

    /// The camera's zoom range, once known.
    pub fn zoom_range(&self) -> Option<(f32, f32)> {
        match self.remote() {
            Some(remote) => remote.camera().and_then(|c| c.zoom_range),
            None => self.camera().and_then(|c| c.zoom_range),
        }
    }

    /// Switches to `source` (through a restart) unless it is already in use.
    pub fn use_source(&self, source: SourceSpec) {
        if source != SourceSpec::Network {
            // A session handed over but not yet taken would be stale by the time
            // the paired phone is the source again.
            self.paired.lock().unwrap().take();
        }
        let mut cfg = self.config.lock().unwrap();
        if cfg.source != source {
            cfg.source = source;
            drop(cfg);
            self.restart();
        }
    }

    /// Shows a paired phone's session ([`SourceSpec::Network`]), in place of
    /// whatever was showing (another paired phone included).
    pub fn pair(&self, session: WebrtcSource) {
        *self.paired.lock().unwrap() = Some(session);
        if self.source() == SourceSpec::Network {
            self.restart();
        } else {
            self.use_source(SourceSpec::Network);
        }
    }

    /// What the phone reported about the current camera, once known; `None` while
    /// another source is in use.
    pub fn camera(&self) -> Option<CameraInfo> {
        if self.source() != SourceSpec::Phone {
            return None;
        }
        let facing = self.facing();
        self.cameras
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.facing == Some(facing))
            .cloned()
    }

    /// The zoom ratio the user asked for (what a slider should show).
    pub fn zoom(&self) -> f32 {
        if let Some(remote) = self.remote() {
            return remote.zoom().unwrap_or(1.0);
        }
        steps_to_zoom(self.zoom.lock().unwrap().target)
    }

    /// The zoom ratio the phone is believed to be at right now.
    pub fn zoom_applied(&self) -> f32 {
        if let Some(remote) = self.remote() {
            return remote.camera().map_or(1.0, |c| c.zoom);
        }
        steps_to_zoom(self.zoom.lock().unwrap().applied)
    }

    /// Asks for the camera zoom `zoom` (snapped to the phone's x1.0625 grid; the
    /// phone clamps to its range). Applied live by the zoom thread when a session is
    /// up, and remembered for the next connection either way. Returns whether the
    /// target moved: a value that snaps back to the current grid step (a slider
    /// re-rounding what it shows) is no change, and so is any zoom while the source
    /// has none. A paired phone takes any ratio in its range, asked of it at once.
    pub fn set_zoom(&self, zoom: f32) -> bool {
        if !self.capabilities().zoom {
            return false;
        }
        if let Some(remote) = self.remote() {
            let (min, max) = self.zoom_range().unwrap_or((1.0, 1.0));
            if (remote.zoom().unwrap_or(1.0) - zoom.clamp(min, max)).abs() < 0.005 {
                return false;
            }
            return remote.set_zoom(zoom).is_some();
        }
        let target = zoom_to_steps(zoom);
        let mut state = self.zoom.lock().unwrap();
        if state.target == target {
            return false;
        }
        state.target = target;
        let ratio = steps_to_zoom(target);
        self.config.lock().unwrap().options.zoom = (ratio > 1.0).then_some(ratio);
        self.zoom_changed.notify_all();
        true
    }

    /// One grid step in (`+1`) or out (`-1`) from the current target. Returns
    /// whether the target moved (it does not below 1x).
    pub fn step_zoom(&self, steps: i32) -> bool {
        if let Some(remote) = self.remote() {
            return self.set_zoom(remote.zoom().unwrap_or(1.0) * ZOOM_STEP.powi(steps));
        }
        let target = self.zoom.lock().unwrap().target + steps;
        self.set_zoom(steps_to_zoom(target.max(0)))
    }

    /// The zoom thread: walk the applied zoom towards the target, one control message
    /// at a time, waiting when there is nothing to do.
    fn run_zoom(&self) {
        let mut state = self.zoom.lock().unwrap();
        while !self.stop.load(Ordering::Relaxed) {
            if state.applied == state.target {
                state = self
                    .zoom_changed
                    .wait_timeout(state, Duration::from_millis(250))
                    .unwrap()
                    .0;
                continue;
            }
            let control = self.control.lock().unwrap().clone();
            let Some(control) = control else {
                // No session: it will start at the target (see run_session).
                state.applied = state.target;
                continue;
            };
            let dir = (state.target - state.applied).signum();
            let sent = if dir > 0 {
                control.zoom_in()
            } else {
                control.zoom_out()
            };
            match sent {
                Ok(()) => state.applied += dir,
                Err(e) => {
                    log::warn!("zoom over the control channel failed: {e}");
                    state.applied = state.target;
                }
            }
        }
    }

    pub fn torch(&self) -> bool {
        if let Some(remote) = self.remote() {
            return remote.torch().unwrap_or(false);
        }
        self.config.lock().unwrap().options.torch
    }

    /// Torch on/off: live when a session is up, and remembered for the next one
    /// (a paired phone's only while it streams).
    pub fn set_torch(&self, on: bool) {
        if let Some(remote) = self.remote() {
            remote.set_torch(on);
            return;
        }
        self.config.lock().unwrap().options.torch = on;
        if let Some(control) = self.control.lock().unwrap().clone() {
            if let Err(e) = control.set_torch(on) {
                log::warn!("torch over the control channel failed: {e}");
            }
        }
    }

    /// Switches camera: takes effect through a reconnect (when the phone is the
    /// source; otherwise the next time it is).
    pub fn set_facing(&self, facing: Facing) {
        let mut cfg = self.config.lock().unwrap();
        if cfg.options.facing != facing {
            cfg.options.facing = facing;
            let phone = cfg.source == SourceSpec::Phone;
            drop(cfg);
            if phone {
                self.restart();
            }
        }
    }

    /// Ends the current session (if any) and connects again with the current config.
    pub fn restart(&self) {
        self.restart.store(true, Ordering::Relaxed);
    }

    /// The state for a worker that has not connected yet, starting at the
    /// configured zoom.
    fn new(config: StreamConfig) -> Self {
        let start = zoom_to_steps(config.options.zoom.unwrap_or(1.0));
        Self {
            stop: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            latest: Mutex::new(None),
            status_changes: AtomicU64::new(0),
            status: Mutex::new(Status::Connecting),
            config: Mutex::new(config),
            cameras: Mutex::new(Vec::new()),
            control: Mutex::new(None),
            zoom: Mutex::new(ZoomState {
                applied: start,
                target: start,
            }),
            zoom_changed: Condvar::new(),
            paired: Mutex::new(None),
            remote: Mutex::new(None),
        }
    }

    fn set_status(&self, status: Status) {
        *self.status.lock().unwrap() = status;
        self.status_changes.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct Worker {
    pub shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts streaming in the background. `wake` is called after every frame and
    /// status change so the UI can repaint.
    pub fn start(config: StreamConfig, wake: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared::new(config));
        let zoom_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("squigl-zoom".into())
            .spawn(move || zoom_shared.run_zoom())
            .expect("spawning zoom thread");
        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("squigl-stream".into())
            .spawn(move || run_loop(&thread_shared, &wake))
            .expect("spawning stream thread");
        Self {
            shared,
            handle: Some(handle),
        }
    }

    /// Asks the worker to stop and waits for it (which shuts the scrcpy server down
    /// and removes the adb forward).
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.zoom_changed.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_loop(shared: &Arc<Shared>, wake: &dyn Fn()) {
    const MIN_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_BACKOFF: Duration = Duration::from_secs(10);
    const RESTART_GRACE: Duration = Duration::from_millis(1500);

    let mut backoff = MIN_BACKOFF;
    let mut tee: Option<Tee> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        shared.restart.store(false, Ordering::Relaxed);
        shared.set_status(Status::Connecting);
        wake();

        let config = shared.config.lock().unwrap().clone();
        let outcome = run_session(shared, &config, &mut tee, wake, &mut backoff);
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        // Only the phone needs a moment to let go of its camera.
        let to_phone = config.source == SourceSpec::Phone && shared.source() == SourceSpec::Phone;
        if shared.restart.load(Ordering::Relaxed) && !to_phone {
            backoff = MIN_BACKOFF;
            continue;
        }
        if shared.restart.load(Ordering::Relaxed) {
            // The phone releases the camera a moment after the server goes; opening
            // the other camera straight away fails with "device is in the error state".
            shared.set_status(Status::Waiting {
                reason: "switching camera".to_string(),
                retry_at: Instant::now() + RESTART_GRACE,
                problem: None,
            });
            wake();
            sleep_unless(RESTART_GRACE, || shared.stop.load(Ordering::Relaxed));
            continue;
        }
        let (reason, problem) = match outcome {
            Ok(()) => ("stream ended".to_string(), None),
            Err(e) => (format!("{e:#}"), Some(classify(&e))),
        };
        log::warn!("{reason}; reconnecting in {backoff:?}");
        shared.set_status(Status::Waiting {
            reason,
            retry_at: Instant::now() + backoff,
            problem,
        });
        wake();
        sleep_unless(backoff, || {
            shared.stop.load(Ordering::Relaxed) || shared.restart.load(Ordering::Relaxed)
        });
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
    shared.set_status(Status::Stopped);
    wake();
}

fn run_session(
    shared: &Arc<Shared>,
    config: &StreamConfig,
    tee: &mut Option<Tee>,
    wake: &dyn Fn(),
    backoff: &mut Duration,
) -> Result<()> {
    // The session's own stop flag: raised by the outer stop or by a restart request,
    // both of which end `run()` at the next packet.
    let session_stop = AtomicBool::new(false);
    let session_stop = &session_stop;
    let frames_before = shared.frames();
    let result = match &config.source {
        SourceSpec::Phone => run_phone(shared, config, session_stop, tee, wake),
        SourceSpec::Replay(path) => {
            let mut replay = Replay::open(path, config.options.decoder)?.looping(true);
            let size = (replay.meta.width, replay.meta.height);
            log::info!("replaying {} ({}x{})", path.display(), size.0, size.1);
            run_local(shared, config, size, session_stop, tee, wake, |sink| {
                replay.run(sink, session_stop)
            })
        }
        SourceSpec::Image(path) => {
            let frame = load_image(path)?;
            let size = (frame.width as u32, frame.height as u32);
            log::info!("showing {} ({}x{})", path.display(), size.0, size.1);
            run_local(shared, config, size, session_stop, tee, wake, |sink| {
                sink.frame(&frame)?;
                // One frame is the whole source: hold it until told otherwise.
                while !shared.stop.load(Ordering::Relaxed)
                    && !shared.restart.load(Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Ok(())
            })
        }
        SourceSpec::TestPattern => {
            let mut pattern = TestPattern::new(640, 480);
            let (w, h) = pattern.size();
            run_local(
                shared,
                config,
                (w as u32, h as u32),
                session_stop,
                tee,
                wake,
                |sink| pattern.run(sink, session_stop),
            )
        }
        SourceSpec::Network => run_paired(shared, config, session_stop, tee, wake),
    };
    // Only a session that delivered frames resets the backoff; one that fails right
    // after the handshake must keep backing off.
    if shared.frames() > frames_before {
        *backoff = Duration::from_secs(1);
    }
    result
}

/// A session with the phone: list its cameras (once), resolve the resolution,
/// connect and stream.
fn run_phone(
    shared: &Arc<Shared>,
    config: &StreamConfig,
    session_stop: &AtomicBool,
    tee: &mut Option<Tee>,
    wake: &dyn Fn(),
) -> Result<()> {
    // List the cameras once: it answers both "what is max" and "what zoom is there".
    if shared.cameras.lock().unwrap().is_empty() {
        let device = select_device(&config.options)?;
        let cameras = squigl_core::list_cameras(&device).context("listing cameras")?;
        *shared.cameras.lock().unwrap() = cameras;
    }
    let resolution = match config.resolution {
        Resolution::PhoneDefault => None,
        Resolution::Fixed(w, h) => Some((w, h)),
        Resolution::Max => {
            let decoder = config.options.decoder;
            let size = shared
                .camera()
                .ok_or_else(|| {
                    anyhow::anyhow!("phone reports no {:?}-facing camera", config.options.facing)
                })?
                .largest_size(|w, h| is_usable_size(w, h, decoder))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "camera offers no size the {} decoder can handle",
                        decoder.name()
                    )
                })?;
            log::info!("maximum resolution resolved to {}x{}", size.0, size.1);
            Some(size)
        }
    };
    let options = ConnectOptions {
        resolution,
        ..config.options.clone()
    };
    let mut session = CameraSession::connect_with_stop(options, session_stop)
        .context("connecting to phone camera")?;
    let (w, h) = (session.meta.width, session.meta.height);
    log::info!("streaming {w}x{h}");
    // The phone starts this session at the configured zoom (snapped to its grid);
    // anything asked for since is caught up by the zoom thread.
    shared.zoom.lock().unwrap().applied = zoom_to_steps(config.options.zoom.unwrap_or(1.0));
    *shared.control.lock().unwrap() = session.control();
    shared.zoom_changed.notify_all();

    if let Some(path) = &config.tee_device {
        open_tee(tee, path, w, h)?;
    }

    shared.set_status(Status::Streaming {
        width: w,
        height: h,
    });
    let mut sink = |frame: &YuvFrame| deliver(shared, tee, session_stop, wake, frame);
    let result = session.run(&mut sink, session_stop).context("streaming");
    *shared.control.lock().unwrap() = None;
    result
}

/// A session with a source that needs no phone: open the tee at its `size`, report
/// it as streaming, and hand `run` the worker's frame delivery.
fn run_local(
    shared: &Shared,
    config: &StreamConfig,
    (w, h): (u32, u32),
    session_stop: &AtomicBool,
    tee: &mut Option<Tee>,
    wake: &dyn Fn(),
    run: impl FnOnce(&mut dyn FrameSink) -> squigl_core::Result<()>,
) -> Result<()> {
    if let Some(path) = &config.tee_device {
        open_tee(tee, path, w, h)?;
    }
    shared.set_status(Status::Streaming {
        width: w,
        height: h,
    });
    let mut sink = |frame: &YuvFrame| deliver(shared, tee, session_stop, wake, frame);
    run(&mut sink).with_context(|| config.source.describe())
}

/// Paired phones, one session after another: each ends when its phone goes, and
/// the next waits for [`Shared::pair`]. The size is the browser's choice, known at
/// the first frame (and it may change: a phone turned on its side).
fn run_paired(
    shared: &Shared,
    config: &StreamConfig,
    session_stop: &AtomicBool,
    tee: &mut Option<Tee>,
    wake: &dyn Fn(),
) -> Result<()> {
    let ended = || shared.stop.load(Ordering::Relaxed) || shared.restart.load(Ordering::Relaxed);
    loop {
        let mut session = loop {
            if let Some(session) = shared.paired.lock().unwrap().take() {
                break session;
            }
            if ended() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let mut size = None;
        let mut sink = |frame: &YuvFrame| {
            let now = (frame.width as u32, frame.height as u32);
            if size != Some(now) {
                size = Some(now);
                if let Some(path) = &config.tee_device {
                    open_tee(tee, path, now.0, now.1).map_err(|e| {
                        squigl_core::Error::Io(std::io::Error::other(format!("{e:#}")))
                    })?;
                }
                shared.set_status(Status::Streaming {
                    width: now.0,
                    height: now.1,
                });
            }
            deliver(shared, tee, session_stop, wake, frame)
        };
        log::info!("a paired phone is streaming");
        *shared.remote.lock().unwrap() = Some(session.control());
        // Until frames flow, nothing else would pass a stop or a restart on.
        let done = AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    if ended() {
                        session_stop.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let result = session.run(&mut sink, session_stop);
            done.store(true, Ordering::Relaxed);
            result
        });
        shared.remote.lock().unwrap().take();
        result.context("paired phone")?;
        if ended() {
            return Ok(());
        }
        log::info!("the paired phone went; waiting for another");
        shared.set_status(Status::Connecting);
        wake();
    }
}

/// A still image as a frame: turned upright by its EXIF orientation (a phone photo
/// is stored sideways), and trimmed to even dimensions for 4:2:0 chroma.
fn load_image(path: &Path) -> Result<YuvFrame> {
    use image::ImageDecoder;
    let open = || -> image::ImageResult<image::DynamicImage> {
        let mut decoder = image::ImageReader::open(path)?
            .with_guessed_format()?
            .into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut img = image::DynamicImage::from_decoder(decoder)?;
        img.apply_orientation(orientation);
        Ok(img)
    };
    let img = open()
        .with_context(|| format!("opening {}", path.display()))?
        .to_rgba8();
    let (w, h) = (img.width() & !1, img.height() & !1);
    anyhow::ensure!(w > 0 && h > 0, "{} is too small to show", path.display());
    let img = if (w, h) == img.dimensions() {
        img
    } else {
        image::imageops::crop_imm(&img, 0, 0, w, h).to_image()
    };
    Ok(rgba_to_i420(img.as_raw(), w as usize, h as usize))
}

/// Publishes a decoded frame to the window (and the tee), and turns a stop or
/// restart request into `session_stop`, which ends the source's `run` at its next
/// packet.
fn deliver(
    shared: &Shared,
    tee: &mut Option<Tee>,
    session_stop: &AtomicBool,
    wake: &dyn Fn(),
    frame: &YuvFrame,
) -> squigl_core::Result<()> {
    // A copy per frame (12 MB at 4K, well under a millisecond) keeps the decoder
    // free to overwrite its buffers while the UI reads this one.
    let number = shared.frames.fetch_add(1, Ordering::Relaxed) + 1;
    *shared.latest.lock().unwrap() = Some((number, Arc::new(frame.clone())));
    if let Some(sink) = tee.as_mut() {
        sink.frame(frame)?;
    }
    if shared.stop.load(Ordering::Relaxed) || shared.restart.load(Ordering::Relaxed) {
        session_stop.store(true, Ordering::Relaxed);
    }
    wake();
    Ok(())
}

/// The optional V4L2 copy of the stream ([`StreamConfig::tee_device`]).
#[cfg(target_os = "linux")]
type Tee = squigl_v4l2::V4l2Sink;

/// Keeps `tee` open at `w`x`h`, reopening it if the stream changed size.
#[cfg(target_os = "linux")]
fn open_tee(tee: &mut Option<Tee>, path: &Path, w: u32, h: u32) -> Result<()> {
    if tee.as_ref().is_some_and(|s| s.size() != (w, h)) {
        *tee = None;
    }
    if tee.is_none() {
        *tee = Some(Tee::open(path, w, h).context("opening V4L2 device")?);
    }
    Ok(())
}

/// V4L2 is Linux-only, so elsewhere there is never a tee to write to.
#[cfg(not(target_os = "linux"))]
enum Tee {}

#[cfg(not(target_os = "linux"))]
impl FrameSink for Tee {
    fn frame(&mut self, _: &YuvFrame) -> squigl_core::Result<()> {
        match *self {}
    }
}

#[cfg(not(target_os = "linux"))]
fn open_tee(_: &mut Option<Tee>, _: &Path, _: u32, _: u32) -> Result<()> {
    anyhow::bail!("writing to a V4L2 device is only possible on Linux")
}

fn select_device(options: &ConnectOptions) -> Result<AdbDevice> {
    Ok(match (&options.tcp_address, &options.serial) {
        (Some(addr), _) => AdbDevice::connect_tcp(addr).context("connecting over Wi-Fi")?,
        (None, Some(s)) => AdbDevice::with_serial(s),
        (None, None) => AdbDevice::autodetect()?,
    })
}

fn sleep_unless(total: Duration, done: impl Fn() -> bool) {
    let deadline = Instant::now() + total;
    while !done() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        Shared::new(StreamConfig {
            options: ConnectOptions::default(),
            resolution: Resolution::PhoneDefault,
            tee_device: None,
            source: SourceSpec::Phone,
        })
    }

    /// A worker given a recording streams it with no phone: the status reports its
    /// size and frames keep arriving past the end, since it loops.
    #[test]
    fn a_worker_plays_a_recording_in_a_loop() {
        use openh264::encoder::Encoder;
        use openh264::formats::YUVBuffer;
        use squigl_core::protocol::{CodecMeta, FramePacket};

        let (w, h) = (64, 48);
        let path = std::env::temp_dir().join(format!("squigl-replay-{}.sqrec", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let mut rec = squigl_core::Recorder::new(
            file,
            CodecMeta {
                width: w as u32,
                height: h as u32,
            },
        )
        .unwrap();
        let mut encoder = Encoder::new().unwrap();
        for i in 0..3u64 {
            let yuv = YUVBuffer::from_vec(vec![128; w * h * 3 / 2], w, h);
            rec.packet(&FramePacket {
                is_config: false,
                is_key_frame: i == 0,
                pts_us: i * 1_000,
                data: encoder.encode(&yuv).unwrap().to_vec(),
            })
            .unwrap();
        }
        rec.finish().unwrap();

        let mut worker = Worker::start(
            StreamConfig {
                options: ConnectOptions::default(),
                resolution: Resolution::PhoneDefault,
                tee_device: None,
                source: SourceSpec::Replay(path.clone()),
            },
            || {},
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.shared.frames() < 10 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = worker.shared.status();
        worker.stop();
        std::fs::remove_file(&path).unwrap();
        assert!(worker.shared.frames() >= 10, "{status:?}");
        assert!(
            matches!(
                status,
                Status::Streaming {
                    width: 64,
                    height: 48
                }
            ),
            "{status:?}"
        );
        assert_eq!(worker.shared.latest().unwrap().width, w);
    }

    /// The zoom slider shows the target rounded to two decimals and writes that back
    /// every frame; at any zoom off 1x that must not count as a change, or a capture
    /// is dropped the frame after it is taken.
    #[test]
    fn a_rounded_slider_value_is_no_zoom_change() {
        let s = shared();
        for steps in 1..60 {
            assert!(s.set_zoom(steps_to_zoom(steps)));
            let shown = (s.zoom() * 100.0).round() / 100.0;
            assert!(
                !s.set_zoom(shown),
                "{shown} moved the zoom off step {steps}"
            );
            assert_eq!(zoom_to_steps(s.zoom()), steps);
        }
    }

    #[test]
    fn stepping_reports_whether_the_zoom_moved() {
        let s = shared();
        assert!(!s.step_zoom(-2), "already at 1x");
        assert!(s.step_zoom(2));
        assert!(!s.set_zoom(s.zoom()));
        assert!(s.set_zoom(1.0));
    }

    /// Starts a worker on `source` with no phone settings to speak of.
    fn worker(source: SourceSpec) -> Worker {
        Worker::start(
            StreamConfig {
                source,
                options: ConnectOptions::default(),
                resolution: Resolution::PhoneDefault,
                tee_device: None,
            },
            || {},
        )
    }

    /// Waits up to 10 s for `done`.
    fn wait_for(done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn errors_are_classified_by_what_a_person_can_do() {
        use anyhow::Context;
        use squigl_core::Error as E;
        let wrapped = |e: E| {
            Err::<(), _>(e)
                .context("connecting to phone camera")
                .unwrap_err()
        };
        assert_eq!(classify(&wrapped(E::AdbNotFound)), Problem::AdbMissing);
        assert_eq!(classify(&wrapped(E::NoDevice)), Problem::NoPhone);
        assert_eq!(
            classify(&wrapped(E::DeviceUnauthorized)),
            Problem::Unauthorized
        );
        assert_eq!(classify(&wrapped(E::DeviceOffline)), Problem::Offline);
        let adb = |m: &str| wrapped(E::AdbCommand(m.into()));
        assert_eq!(
            classify(&adb(
                "multiple devices attached ([\"A\", \"B\"]); pass a serial explicitly"
            )),
            Problem::SeveralPhones
        );
        assert_eq!(
            classify(&adb("`adb shell` failed: adb: device unauthorized.")),
            Problem::Unauthorized
        );
        assert_eq!(
            classify(&adb("`adb push` failed: adb: device 'XYZ' not found")),
            Problem::NoPhone
        );
        // The server's own words, folded into the error.
        let server = |m: &str| {
            wrapped(E::Protocol(format!(
                "no packets\nscrcpy server output:\n{m}"
            )))
        };
        assert_eq!(
            classify(&server(
                "[server] ERROR: Camera mirroring is not supported before Android 12"
            )),
            Problem::AndroidTooOld
        );
        assert_eq!(
            classify(&server(
                "CameraAccessException: CAMERA_IN_USE (4): connect: camera in use"
            )),
            Problem::CameraInUse
        );
        let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        assert_eq!(
            classify(&anyhow::Error::from(missing).context("opening scan.png")),
            Problem::FileMissing
        );
        assert_eq!(classify(&anyhow::anyhow!("something odd")), Problem::Other);
    }

    #[test]
    fn only_the_phone_has_camera_controls() {
        let all = Capabilities {
            zoom: true,
            torch: true,
            facing: true,
        };
        assert_eq!(SourceSpec::Phone.capabilities(), all);
        for other in [
            SourceSpec::Replay("a.sqrec".into()),
            SourceSpec::Image("a.png".into()),
            SourceSpec::TestPattern,
            SourceSpec::Network,
        ] {
            assert_eq!(
                other.capabilities(),
                Capabilities {
                    zoom: false,
                    torch: false,
                    facing: false
                }
            );
        }
        assert_eq!(
            SourceSpec::Image("/scans/p1.png".into()).describe(),
            "p1.png"
        );
        assert_eq!(
            SourceSpec::for_file("desk.SQREC".into()),
            SourceSpec::Replay("desk.SQREC".into())
        );
        assert_eq!(
            SourceSpec::for_file("scan.jpg".into()),
            SourceSpec::Image("scan.jpg".into())
        );
    }

    /// An image is shown upright and trimmed to even dimensions, and switching from
    /// it to another source happens straight away (no phone camera to release).
    #[test]
    fn a_worker_shows_an_image_then_switches_source() {
        let path = std::env::temp_dir().join(format!("squigl-image-{}.png", std::process::id()));
        // 5x3, odd both ways: white, with a red top-left pixel.
        let mut img = image::RgbaImage::from_pixel(5, 3, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.save(&path).unwrap();

        let mut worker = worker(SourceSpec::Image(path.clone()));
        wait_for(|| worker.shared.frames() > 0);
        let frame = worker.shared.latest().expect("the image as a frame");
        assert_eq!((frame.width, frame.height), (4, 2));
        // White luma, and the red corner's chroma leans to V.
        assert!(frame.y[3] >= 230, "{:?}", frame.y);
        assert!(frame.v[0] > 150, "{:?}", frame.v);
        assert!(worker.shared.camera().is_none());
        assert!(!worker.shared.capabilities().zoom);
        assert!(!worker.shared.set_zoom(2.0), "an image has no zoom");

        let t = Instant::now();
        worker.shared.use_source(SourceSpec::TestPattern);
        wait_for(|| worker.shared.latest().is_some_and(|f| f.width == 640));
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert!(matches!(
            worker.shared.status(),
            Status::Streaming {
                width: 640,
                height: 480
            }
        ));
        worker.stop();
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_network_source_waits_for_a_phone_and_lets_go_at_once() {
        assert_eq!(
            serde_json::to_string(&SourceSpec::Network).unwrap(),
            r#"{"kind":"network"}"#
        );
        let mut worker = worker(SourceSpec::Network);
        std::thread::sleep(Duration::from_millis(300));
        assert!(matches!(worker.shared.status(), Status::Connecting));
        assert_eq!(worker.shared.frames(), 0);

        let t = Instant::now();
        worker.shared.use_source(SourceSpec::TestPattern);
        wait_for(|| worker.shared.frames() > 0);
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        worker.stop();
    }

    /// A session as `squigl-pairing` hands one over, from a fake phone.
    fn paired_phone(size: (usize, usize)) -> (WebrtcSource, squigl_pairing::testing::FakePhone) {
        let mut session = None;
        let phone = squigl_pairing::testing::FakePhone::connect(size, |offer| {
            let local = std::net::Ipv4Addr::LOCALHOST.into();
            let (accepted, answer) = WebrtcSource::accept_offer(offer, local, Default::default())
                .map_err(|e| e.to_string())?;
            session = Some(accepted);
            Ok(answer)
        })
        .unwrap();
        (session.unwrap(), phone)
    }

    #[test]
    fn a_paired_phone_shows_and_a_second_takes_its_place() {
        let mut worker = worker(SourceSpec::TestPattern);
        wait_for(|| worker.shared.frames() > 0);

        let (session, _first) = paired_phone((320, 240));
        worker.shared.pair(session);
        assert_eq!(worker.shared.source(), SourceSpec::Network);
        wait_for(|| worker.shared.latest().is_some_and(|f| f.width == 320));
        assert!(matches!(
            worker.shared.status(),
            Status::Streaming {
                width: 320,
                height: 240
            }
        ));

        let (session, _second) = paired_phone((160, 120));
        worker.shared.pair(session);
        wait_for(|| worker.shared.latest().is_some_and(|f| f.width == 160));
        assert_eq!(worker.shared.latest().unwrap().height, 120);
        worker.stop();
    }

    /// A paired phone's zoom and torch are what it reports, and go to it live.
    #[test]
    fn a_paired_phone_has_the_zoom_and_torch_it_reports() {
        let mut worker = worker(SourceSpec::TestPattern);
        let shared = Arc::clone(&worker.shared);
        assert!(!shared.set_zoom(2.0), "colour bars have no zoom");

        let (session, phone) = paired_phone((160, 120));
        shared.pair(session);
        wait_for(|| shared.capabilities().zoom);
        assert_eq!(
            shared.capabilities(),
            Capabilities {
                zoom: true,
                torch: true,
                facing: false
            }
        );
        assert_eq!(shared.zoom_range(), Some((1.0, 8.0)));
        assert!(shared.set_zoom(2.5));
        assert!(!shared.set_zoom(2.501), "a slider re-rounding is no change");
        assert_eq!(shared.zoom(), 2.5);
        shared.set_torch(true);
        assert!(shared.torch());
        wait_for(|| phone.camera().zoom == 2.5 && phone.camera().torch == Some(true));
        wait_for(|| shared.zoom_applied() == 2.5);
        assert!(shared.step_zoom(1));
        assert!((shared.zoom() - 2.5 * ZOOM_STEP).abs() < 1e-4);
        assert!(shared.set_zoom(100.0));
        assert_eq!(shared.zoom(), 8.0, "clamped to the phone's range");
        assert!(!shared.step_zoom(1), "nothing beyond it");

        // Another source has none of it, and the ADB phone's settings are untouched.
        shared.use_source(SourceSpec::TestPattern);
        assert_eq!(
            shared.capabilities(),
            SourceSpec::TestPattern.capabilities()
        );
        assert_eq!(shared.zoom(), 1.0);
        assert!(!shared.torch());
        assert_eq!(shared.config.lock().unwrap().options.zoom, None);
        worker.stop();
    }

    #[test]
    fn a_missing_image_is_reported_and_retried() {
        let mut worker = worker(SourceSpec::Image("/nonexistent/scan.png".into()));
        wait_for(|| matches!(worker.shared.status(), Status::Waiting { .. }));
        let status = worker.shared.status();
        worker.stop();
        let Status::Waiting {
            reason, problem, ..
        } = status
        else {
            panic!("{status:?}");
        };
        assert!(reason.contains("scan.png"), "{reason}");
        assert_eq!(problem, Some(Problem::FileMissing));
    }
}
