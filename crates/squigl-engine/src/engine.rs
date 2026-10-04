//! The engine a front end drives: it sends [`Command`]s to [`Engine::handle`], reads
//! [`Engine::state`], and calls [`Engine::pump`] whenever the waker it gave
//! [`Engine::new`] fires (and once per frame it draws, if it likes), turning
//! whatever happened in the background -- frames, a status change, a model's
//! progress -- into [`Event`]s.
//!
//! The state is a set of versioned slices ([`EngineState`]), each re-sent whole when
//! it changes, so a front end that keeps the last of each is always current. They
//! are all defined here, the reading ones included, though in this phase the engine
//! fills only the stream, capture, model and config slices: block detection and
//! reading still run in the egui window and move in later.
//!
//! The engine has no thread of its own. Its stream worker and the built-in models
//! run on theirs and call the waker; everything else happens in `handle` and `pump`
//! on the caller's thread.

use crate::config::{Config, LayoutConfig};
use crate::display::{self, Lut};
use crate::history;
use crate::layout::{Block, BlockDetector};
use crate::model::{ModelContext, ModelPhase};
use crate::stream::{Capabilities, Shared, SourceSpec, Status, StreamConfig, Worker};
use crate::transcribe::{BackendConfig, Transcriber, Transcription};
use crate::view::{render_planes, FrameRef, ViewPlanes, ViewRequest};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use squigl_core::convert::Rotation;
use squigl_core::decode::YuvFrame;
use squigl_core::Facing;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Called from any thread when there is something for [`Engine::pump`] to collect.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Builds a transcription backend from its config entry; `None` for one this build
/// cannot provide. [`BackendConfig::build`] covers the HTTP ones; a front end with
/// the built-in model adds it.
pub type BackendFactory =
    Box<dyn Fn(&BackendConfig, &ModelContext) -> Option<Box<dyn Transcriber>>>;

/// Builds the block detector for a layout config; `None` when the build has none or
/// it is disabled.
pub type DetectorFactory =
    Box<dyn Fn(&LayoutConfig, &ModelContext) -> Option<Arc<dyn BlockDetector>>>;

/// What the engine cannot build itself: whatever implements its traits from outside.
pub struct EngineDeps {
    pub backends: BackendFactory,
    pub detector: DetectorFactory,
}

impl EngineDeps {
    /// The HTTP backends only, and no detector: a build without the built-in models.
    pub fn remote_only() -> Self {
        Self {
            backends: Box::new(|b, _| b.build()),
            detector: Box::new(|_, _| None),
        }
    }
}

pub struct EngineOptions {
    /// Prepare the built-in models as soon as they are built (the egui window), or
    /// only on [`Command::PrepareModel`] (a front end that asks first).
    pub eager_models: bool,
    /// Where [`Command::SetConfig`] saves the settings; `None` keeps them in memory.
    pub config_file: Option<PathBuf>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            eager_models: false,
            config_file: Some(Config::path()),
        }
    }
}

/// Everything a front end can ask of the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Command {
    /// Freezes the newest live frame: the capture, until [`Command::Live`].
    Freeze,
    /// Back to the live picture.
    Live,
    SetRotation {
        rotation: Rotation,
    },
    /// Switches the source; a capture of the old one is dropped.
    UseSource {
        source: SourceSpec,
    },
    SetFacing {
        facing: Facing,
    },
    /// The phone's own zoom (see [`crate::stream::Shared::set_zoom`]); a change
    /// goes live, since the capture no longer shows what the camera sees.
    SetZoom {
        zoom: f32,
    },
    StepZoom {
        steps: i32,
    },
    SetTorch {
        on: bool,
    },
    /// Ends the current session and connects again.
    Reconnect,
    /// Puts the settings in force -- backends whose entry is unchanged are kept, so
    /// a loaded model is not reloaded; the detector is rebuilt if its section
    /// changed -- and saves them.
    SetConfig {
        config: Box<Config>,
    },
    /// Gets a built-in model ready (downloading it if need be), by its name in
    /// [`ModelsSlice`].
    PrepareModel {
        name: String,
    },
    CancelModelDownload {
        name: String,
    },
}

/// What a command did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reply {
    Done,
    /// It asked for what already was.
    Unchanged,
}

/// A slice and how many times it has changed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Versioned<T> {
    pub version: u64,
    pub value: T,
}

impl<T: PartialEq> Versioned<T> {
    fn new(value: T) -> Self {
        Self { version: 1, value }
    }

    /// Takes `value`; true (and a new version) if it differs.
    fn update(&mut self, value: T) -> bool {
        if self.value == value {
            return false;
        }
        self.value = value;
        self.version += 1;
        true
    }
}

/// The stream worker's state, without the instants.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum StreamStatus {
    Connecting,
    Streaming {
        width: u32,
        height: u32,
    },
    /// The last attempt failed or the stream ended; retrying in `retry_in_ms` (as
    /// of when the status changed).
    Waiting {
        reason: String,
        retry_in_ms: u64,
    },
    Stopped,
}

impl StreamStatus {
    fn from_status(status: Status, now: Instant) -> Self {
        match status {
            Status::Connecting => StreamStatus::Connecting,
            Status::Streaming { width, height } => StreamStatus::Streaming { width, height },
            Status::Waiting { reason, retry_at } => StreamStatus::Waiting {
                reason,
                retry_in_ms: retry_at.saturating_duration_since(now).as_millis() as u64,
            },
            Status::Stopped => StreamStatus::Stopped,
        }
    }
}

/// The source and its camera.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamSlice {
    pub source: SourceSpec,
    pub status: StreamStatus,
    pub capabilities: Capabilities,
    pub facing: Facing,
    /// The zoom asked for, and where the phone is believed to be.
    pub zoom: f32,
    pub zoom_applied: f32,
    /// The camera's zoom range, once the phone has reported it.
    pub zoom_range: Option<(f32, f32)>,
    pub torch: bool,
}

/// A frozen frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Frozen {
    /// Counting captures from 1 over the engine's life.
    pub seq: u64,
    pub width: usize,
    pub height: usize,
}

/// What is shown: frozen or live, and turned how.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaptureSlice {
    pub frozen: Option<Frozen>,
    pub rotation: Rotation,
    /// The shown frame's size after the rotation, once there is a frame.
    pub view: Option<(usize, usize)>,
}

/// Block detection on the shown frame. Filled from a later phase; empty until then.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct BlocksSlice {
    /// Block mode is on: the detector runs on whatever is shown.
    pub enabled: bool,
    pub detecting: bool,
    /// In view space, in reading order.
    pub blocks: Vec<Block>,
    pub selected: Option<usize>,
}

/// A read in progress.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadInProgress {
    pub backend: String,
    /// The block's label in a "read all".
    pub label: Option<String>,
    /// Blocks still waiting in a "read all".
    pub queued: usize,
}

/// Reading. The backends are listed from now; the rest is filled from a later
/// phase. The results themselves come as [`Event::ResultAppended`].
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct ReadingSlice {
    pub backends: Vec<String>,
    pub selected: usize,
    pub reading: Option<ReadInProgress>,
    /// How many results the current list holds, and the session's history.
    pub results: usize,
    pub history: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelKind {
    Transcriber,
    Detector,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelStatus {
    /// What [`Command::PrepareModel`] takes.
    pub name: String,
    pub kind: ModelKind,
    pub phase: ModelPhase,
}

/// The built-in models and where each is.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct ModelsSlice {
    pub models: Vec<ModelStatus>,
}

/// Every slice, as of the last [`Engine::pump`] (or command).
#[derive(Debug, Clone, Serialize)]
pub struct EngineState {
    pub stream: Versioned<StreamSlice>,
    pub capture: Versioned<CaptureSlice>,
    pub blocks: Versioned<BlocksSlice>,
    pub reading: Versioned<ReadingSlice>,
    pub models: Versioned<ModelsSlice>,
    pub config: Versioned<Config>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// A message for the person: the window's status line, or a web UI's `aria-live`
/// region.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notice {
    pub text: String,
    pub level: Level,
}

/// One entry of the results list.
#[derive(Debug, Clone, Serialize)]
pub struct ReadResult {
    pub label: Option<String>,
    pub result: Result<Transcription, String>,
}

/// What [`Engine::pump`] reports.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum Event {
    Stream(Versioned<StreamSlice>),
    Capture(Versioned<CaptureSlice>),
    Blocks(Versioned<BlocksSlice>),
    Reading(Versioned<ReadingSlice>),
    Models(Versioned<ModelsSlice>),
    Config(Versioned<Config>),
    /// A new live frame, by number: fetch it with [`Engine::render_planes`].
    Frame {
        seq: u64,
    },
    ResultAppended(ReadResult),
    ResultsCleared,
    HistoryAppended(history::Entry),
    Notice(Notice),
}

pub struct Engine {
    deps: EngineDeps,
    options: EngineOptions,
    models: ModelContext,
    worker: Worker,
    rotation: Rotation,
    /// The frozen frame and its number.
    capture: Option<(u64, Arc<YuvFrame>)>,
    captures: u64,
    /// The backends in force and the config entry each came from.
    backends: Vec<Arc<dyn Transcriber>>,
    backend_configs: Vec<BackendConfig>,
    detector: Option<Arc<dyn BlockDetector>>,
    state: EngineState,
    /// The worker's status count and newest frame number when last looked at.
    seen_status: Option<u64>,
    seen_frame: u64,
    notices: Vec<Notice>,
    /// Each slice's version when [`pump`](Self::pump) last sent it (0: never).
    sent: Sent,
}

#[derive(Default)]
struct Sent {
    stream: u64,
    capture: u64,
    blocks: u64,
    reading: u64,
    models: u64,
    config: u64,
}

impl Engine {
    /// Starts streaming `stream` and builds the backends and detector `config` lists.
    /// `wake` is called, from any thread, whenever [`pump`](Self::pump) has
    /// something to collect.
    pub fn new(
        config: Config,
        stream: StreamConfig,
        deps: EngineDeps,
        options: EngineOptions,
        wake: Waker,
    ) -> Self {
        let models = ModelContext {
            eager: options.eager_models,
            notify: Arc::clone(&wake),
        };
        let worker_wake = Arc::clone(&wake);
        let worker = Worker::start(stream, move || worker_wake());
        let now = Instant::now();
        let stream_slice = stream_slice(&worker.shared, now);
        let mut engine = Self {
            deps,
            options,
            models,
            worker,
            rotation: Rotation::None,
            capture: None,
            captures: 0,
            backends: Vec::new(),
            backend_configs: Vec::new(),
            detector: None,
            state: EngineState {
                stream: Versioned::new(stream_slice),
                capture: Versioned::new(CaptureSlice {
                    frozen: None,
                    rotation: Rotation::None,
                    view: None,
                }),
                blocks: Versioned::new(BlocksSlice::default()),
                reading: Versioned::new(ReadingSlice::default()),
                models: Versioned::new(ModelsSlice::default()),
                config: Versioned::new(config.clone()),
            },
            seen_status: None,
            seen_frame: 0,
            notices: Vec::new(),
            sent: Sent::default(),
        };
        engine.build(&config, true);
        engine.refresh(now);
        engine
    }

    pub fn state(&self) -> &EngineState {
        &self.state
    }

    pub fn config(&self) -> &Config {
        &self.state.config.value
    }

    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// The stream worker's shared state, for reads the slices do not carry (the
    /// camera listing); change things through commands.
    pub fn shared(&self) -> &Arc<Shared> {
        &self.worker.shared
    }

    pub fn backends(&self) -> &[Arc<dyn Transcriber>] {
        &self.backends
    }

    pub fn detector(&self) -> Option<&Arc<dyn BlockDetector>> {
        self.detector.as_ref()
    }

    /// The frozen frame, if any: the same `Arc` every time until it changes.
    pub fn captured(&self) -> Option<Arc<YuvFrame>> {
        self.capture.as_ref().map(|(_, f)| Arc::clone(f))
    }

    /// The capture's number: 0 before the first, and it never repeats.
    pub fn capture_seq(&self) -> u64 {
        self.capture.as_ref().map_or(0, |(n, _)| *n)
    }

    /// How many captures there have been.
    pub fn captures(&self) -> u64 {
        self.captures
    }

    pub fn frame(&self, which: FrameRef) -> Option<Arc<YuvFrame>> {
        self.numbered(which).map(|(_, f)| f)
    }

    /// A frame with its number: a live frame's count from the source, or the
    /// capture's.
    fn numbered(&self, which: FrameRef) -> Option<(u64, Arc<YuvFrame>)> {
        let captured = || self.capture.clone();
        let live = || self.worker.shared.latest_numbered();
        match which {
            FrameRef::Live => live(),
            FrameRef::Captured => captured(),
            FrameRef::Shown => captured().or_else(live),
        }
    }

    /// The planes for `request`, from the frame it names at the current rotation;
    /// `None` before there is such a frame.
    pub fn render_planes(&self, request: &ViewRequest) -> Option<ViewPlanes> {
        let (seq, frame) = self.numbered(request.frame)?;
        Some(render_planes(&frame, seq, self.rotation, request))
    }

    /// The table for the configured display mode.
    pub fn lut(&self) -> Lut {
        display::lut(&self.config().display)
    }

    /// Stops the stream worker (shutting a phone session down); the engine is done.
    pub fn stop(&mut self) {
        self.worker.stop();
    }

    pub fn handle(&mut self, command: Command) -> Result<Reply> {
        let reply = match command {
            Command::Freeze => {
                if self.capture.is_some() {
                    Reply::Unchanged
                } else {
                    let Some((_, frame)) = self.worker.shared.latest_numbered() else {
                        bail!("nothing to capture yet");
                    };
                    self.captures += 1;
                    self.capture = Some((self.captures, frame));
                    Reply::Done
                }
            }
            Command::Live => {
                if self.capture.take().is_some() {
                    Reply::Done
                } else {
                    Reply::Unchanged
                }
            }
            Command::SetRotation { rotation } => {
                if rotation == self.rotation {
                    Reply::Unchanged
                } else {
                    self.rotation = rotation;
                    Reply::Done
                }
            }
            Command::UseSource { source } => {
                if source == self.worker.shared.source() {
                    Reply::Unchanged
                } else {
                    self.capture = None;
                    let what = source.describe();
                    self.worker.shared.use_source(source);
                    self.notice(Level::Info, format!("showing {what}"));
                    Reply::Done
                }
            }
            Command::SetFacing { facing } => {
                if facing == self.worker.shared.facing() {
                    Reply::Unchanged
                } else {
                    self.worker.shared.set_facing(facing);
                    Reply::Done
                }
            }
            Command::SetZoom { zoom } => {
                let moved = self.worker.shared.set_zoom(zoom);
                self.zoomed(moved)
            }
            Command::StepZoom { steps } => {
                let moved = self.worker.shared.step_zoom(steps);
                self.zoomed(moved)
            }
            Command::SetTorch { on } => {
                if on == self.worker.shared.torch() || !self.worker.shared.capabilities().torch {
                    Reply::Unchanged
                } else {
                    self.worker.shared.set_torch(on);
                    Reply::Done
                }
            }
            Command::Reconnect => {
                self.worker.shared.restart();
                Reply::Done
            }
            Command::SetConfig { config } => {
                if *config == *self.config() {
                    Reply::Unchanged
                } else {
                    self.build(&config, false);
                    self.state.config.update(*config);
                    if let Some(path) = &self.options.config_file {
                        self.config().save_to(path)?;
                    }
                    Reply::Done
                }
            }
            Command::PrepareModel { name } => {
                self.model(&name)?.prepare();
                Reply::Done
            }
            Command::CancelModelDownload { name } => {
                self.model(&name)?.cancel();
                Reply::Done
            }
        };
        self.refresh(Instant::now());
        Ok(reply)
    }

    /// What a zoom command did: a move goes live.
    fn zoomed(&mut self, moved: bool) -> Reply {
        if !moved {
            return Reply::Unchanged;
        }
        if self.capture.take().is_some() {
            self.notice(Level::Info, "live again (zoom changed)");
        }
        Reply::Done
    }

    fn notice(&mut self, level: Level, text: impl Into<String>) {
        self.notices.push(Notice {
            text: text.into(),
            level,
        });
    }

    /// The built-in model called `name`, to prepare or cancel.
    fn model(&self, name: &str) -> Result<ModelHandle<'_>> {
        if let Some(b) = self
            .backends
            .iter()
            .find(|b| b.name() == name && b.phase().is_some())
        {
            return Ok(ModelHandle::Transcriber(b.as_ref()));
        }
        match &self.detector {
            Some(d) if d.name() == name && d.phase().is_some() => {
                Ok(ModelHandle::Detector(d.as_ref()))
            }
            _ => bail!("no built-in model called {name:?}"),
        }
    }

    /// Builds what `config` lists: backends whose entry is unchanged are kept, the
    /// rest built; the detector is rebuilt when its section changed (or `first`).
    fn build(&mut self, config: &Config, first: bool) {
        let mut backends = Vec::new();
        let mut configs = Vec::new();
        for b in &config.backends {
            let existing = self
                .backend_configs
                .iter()
                .position(|c| c == b)
                .map(|i| Arc::clone(&self.backends[i]));
            let built = existing.or_else(|| (self.deps.backends)(b, &self.models).map(Arc::from));
            if let Some(t) = built {
                backends.push(t);
                configs.push(b.clone());
            }
        }
        self.backends = backends;
        self.backend_configs = configs;
        if first || config.layout != self.config().layout {
            self.detector = (self.deps.detector)(&config.layout, &self.models);
        }
    }

    /// Recomputes every slice from the worker, the capture and the models, bumping
    /// the version of each that changed.
    fn refresh(&mut self, now: Instant) {
        let shared = Arc::clone(&self.worker.shared);
        let status_changes = shared.status_changes();
        let mut stream = stream_slice(&shared, now);
        // A waiting status counts down; it is only news when the status itself
        // changed.
        if self.seen_status == Some(status_changes) {
            stream.status = self.state.stream.value.status.clone();
        }
        self.seen_status = Some(status_changes);
        self.state.stream.update(stream);

        let shown = self.numbered(FrameRef::Shown);
        self.state.capture.update(CaptureSlice {
            frozen: self.capture.as_ref().map(|(seq, f)| Frozen {
                seq: *seq,
                width: f.width,
                height: f.height,
            }),
            rotation: self.rotation,
            view: shown.map(|(_, f)| self.rotation.rotated_size(f.width, f.height)),
        });

        let mut models: Vec<ModelStatus> = self
            .backends
            .iter()
            .filter_map(|b| {
                Some(ModelStatus {
                    name: b.name().to_string(),
                    kind: ModelKind::Transcriber,
                    phase: b.phase()?,
                })
            })
            .collect();
        if let Some(d) = &self.detector {
            if let Some(phase) = d.phase() {
                models.push(ModelStatus {
                    name: d.name().to_string(),
                    kind: ModelKind::Detector,
                    phase,
                });
            }
        }
        self.state.models.update(ModelsSlice { models });

        let names: Vec<String> = self.backends.iter().map(|b| b.name().to_string()).collect();
        self.state.reading.update(ReadingSlice {
            backends: names,
            ..self.state.reading.value.clone()
        });
    }

    /// Collects what happened since the last call: every slice whose version moved
    /// since it was last sent (by a command or in the background), a new frame,
    /// notices. The first call sends every slice.
    pub fn pump(&mut self, now: Instant) -> Vec<Event> {
        self.refresh(now);
        let mut events = Vec::new();
        let (s, sent) = (&self.state, &mut self.sent);
        fn moved<T>(slice: &Versioned<T>, sent: &mut u64) -> bool {
            std::mem::replace(sent, slice.version) != slice.version
        }
        if moved(&s.stream, &mut sent.stream) {
            events.push(Event::Stream(s.stream.clone()));
        }
        if moved(&s.capture, &mut sent.capture) {
            events.push(Event::Capture(s.capture.clone()));
        }
        if moved(&s.blocks, &mut sent.blocks) {
            events.push(Event::Blocks(s.blocks.clone()));
        }
        if moved(&s.reading, &mut sent.reading) {
            events.push(Event::Reading(s.reading.clone()));
        }
        if moved(&s.models, &mut sent.models) {
            events.push(Event::Models(s.models.clone()));
        }
        if moved(&s.config, &mut sent.config) {
            events.push(Event::Config(s.config.clone()));
        }
        let frames = self.worker.shared.frames();
        if frames > self.seen_frame {
            self.seen_frame = frames;
            events.push(Event::Frame { seq: frames });
        }
        events.extend(self.notices.drain(..).map(Event::Notice));
        events
    }
}

enum ModelHandle<'a> {
    Transcriber(&'a dyn Transcriber),
    Detector(&'a dyn BlockDetector),
}

impl ModelHandle<'_> {
    fn prepare(&self) {
        match self {
            ModelHandle::Transcriber(t) => t.prepare(),
            ModelHandle::Detector(d) => d.prepare(),
        }
    }

    fn cancel(&self) {
        match self {
            ModelHandle::Transcriber(t) => t.cancel_prepare(),
            ModelHandle::Detector(d) => d.cancel_prepare(),
        }
    }
}

fn stream_slice(shared: &Shared, now: Instant) -> StreamSlice {
    StreamSlice {
        source: shared.source(),
        status: StreamStatus::from_status(shared.status(), now),
        capabilities: shared.capabilities(),
        facing: shared.facing(),
        zoom: shared.zoom(),
        zoom_applied: shared.zoom_applied(),
        zoom_range: shared.camera().and_then(|c| c.zoom_range),
        torch: shared.torch(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PhaseCell;
    use crate::stream::Resolution;
    use squigl_core::convert::PlaneFormat;
    use squigl_core::ConnectOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A built-in model stand-in: prepares at once, never reads.
    struct FakeModel {
        name: String,
        phase: PhaseCell,
    }

    impl Transcriber for FakeModel {
        fn name(&self) -> &str {
            &self.name
        }
        fn phase(&self) -> Option<ModelPhase> {
            Some(self.phase.get())
        }
        fn prepare(&self) {
            self.phase.set(ModelPhase::Ready {
                device: "test".into(),
            });
        }
        fn cancel_prepare(&self) {
            self.phase.set(ModelPhase::NotInstalled { size: 1 });
        }
        fn read(
            &self,
            _: &[u8],
            _: crate::transcribe::Mode,
            _: &str,
            _: (u32, u32),
        ) -> Result<Transcription> {
            bail!("a fake reads nothing")
        }
    }

    struct Counts {
        wakes: Arc<AtomicUsize>,
        backends_built: Arc<AtomicUsize>,
        detectors_built: Arc<AtomicUsize>,
    }

    /// The built-in model as a [`FakeModel`], the HTTP ones as they are, and a
    /// detector that is only counted.
    fn deps(counts: &Counts) -> EngineDeps {
        let (b, d) = (
            Arc::clone(&counts.backends_built),
            Arc::clone(&counts.detectors_built),
        );
        EngineDeps {
            backends: Box::new(move |config, ctx| {
                b.fetch_add(1, Ordering::Relaxed);
                match config {
                    BackendConfig::Local { name, .. } => Some(Box::new(FakeModel {
                        name: name.clone(),
                        phase: PhaseCell::new(
                            ModelPhase::NotInstalled { size: 1 },
                            Arc::clone(&ctx.notify),
                        ),
                    })),
                    other => other.build(),
                }
            }),
            detector: Box::new(move |_, _| {
                d.fetch_add(1, Ordering::Relaxed);
                None
            }),
        }
    }

    /// A PNG of `w`x`h`, named for the test so tests can run side by side.
    fn image(test: &str, w: u32, h: u32) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("squigl-engine-{test}-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 200, 200, 255]))
            .save(&path)
            .unwrap();
        path
    }

    fn start(source: SourceSpec, config: Config, config_file: Option<PathBuf>) -> (Engine, Counts) {
        let counts = Counts {
            wakes: Arc::new(AtomicUsize::new(0)),
            backends_built: Arc::new(AtomicUsize::new(0)),
            detectors_built: Arc::new(AtomicUsize::new(0)),
        };
        let wakes = Arc::clone(&counts.wakes);
        let engine = Engine::new(
            config,
            StreamConfig {
                source,
                options: ConnectOptions::default(),
                resolution: Resolution::PhoneDefault,
                tee_device: None,
            },
            deps(&counts),
            EngineOptions {
                eager_models: false,
                config_file,
            },
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            }),
        );
        (engine, counts)
    }

    /// Pumps until `done` holds of an event, collecting everything; panics after 10 s.
    fn pump_until(engine: &mut Engine, done: impl Fn(&Event) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        loop {
            let events = engine.pump(Instant::now());
            let hit = events.iter().any(&done);
            all.extend(events);
            if hit {
                return all;
            }
            assert!(Instant::now() < deadline, "timed out; saw {all:#?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn kinds(events: &[Event]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e {
                Event::Stream(_) => "stream",
                Event::Capture(_) => "capture",
                Event::Blocks(_) => "blocks",
                Event::Reading(_) => "reading",
                Event::Models(_) => "models",
                Event::Config(_) => "config",
                Event::Frame { .. } => "frame",
                Event::ResultAppended(_) => "result",
                Event::ResultsCleared => "cleared",
                Event::HistoryAppended(_) => "history",
                Event::Notice(_) => "notice",
            })
            .collect()
    }

    #[test]
    fn the_first_pump_sends_every_slice_and_then_only_changes() {
        let path = image("first", 8, 6);
        let (mut engine, counts) = start(SourceSpec::Image(path.clone()), Config::default(), None);
        let first = engine.pump(Instant::now());
        for kind in ["stream", "capture", "blocks", "reading", "models", "config"] {
            assert!(
                kinds(&first).contains(&kind),
                "{kind} missing from {:?}",
                kinds(&first)
            );
        }
        pump_until(&mut engine, |e| matches!(e, Event::Frame { .. }));
        assert!(counts.wakes.load(Ordering::Relaxed) > 0);
        // An image sends one frame; once its status and size are in, all is quiet.
        let quiet = (0..200).any(|_| {
            std::thread::sleep(Duration::from_millis(5));
            engine.pump(Instant::now()).is_empty()
        });
        assert!(quiet);
        assert_eq!(engine.state().capture.value.view, Some((8, 6)));
        assert!(matches!(
            engine.state().stream.value.status,
            StreamStatus::Streaming {
                width: 8,
                height: 6
            }
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn captures_are_numbered_and_keep_their_frame() {
        let path = image("capture", 8, 6);
        let (mut engine, _) = start(SourceSpec::Image(path.clone()), Config::default(), None);
        assert!(engine.handle(Command::Freeze).is_err(), "no frame yet");
        pump_until(&mut engine, |e| matches!(e, Event::Frame { .. }));
        assert_eq!(engine.handle(Command::Freeze).unwrap(), Reply::Done);
        assert_eq!(engine.handle(Command::Freeze).unwrap(), Reply::Unchanged);
        let captured = engine.captured().unwrap();
        assert!(Arc::ptr_eq(
            &captured,
            &engine.frame(FrameRef::Shown).unwrap()
        ));
        assert_eq!(engine.capture_seq(), 1);
        let events = engine.pump(Instant::now());
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Capture(Versioned {
                value: CaptureSlice {
                    frozen: Some(Frozen { seq: 1, .. }),
                    ..
                },
                ..
            })
        )));
        assert_eq!(engine.handle(Command::Live).unwrap(), Reply::Done);
        assert_eq!(engine.handle(Command::Live).unwrap(), Reply::Unchanged);
        assert_eq!(engine.capture_seq(), 0);
        engine.handle(Command::Freeze).unwrap();
        assert_eq!(engine.capture_seq(), 2);
        // Planes of the capture carry its number.
        let request = ViewRequest {
            frame: FrameRef::Captured,
            region: crate::geometry::Crop::whole(8, 6),
            step: 1,
            format: PlaneFormat::Luma,
        };
        assert_eq!(engine.render_planes(&request).unwrap().header.seq, 2);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rotation_turns_the_view() {
        let path = image("rotate", 8, 6);
        let (mut engine, _) = start(SourceSpec::Image(path.clone()), Config::default(), None);
        pump_until(&mut engine, |e| matches!(e, Event::Frame { .. }));
        let version = engine.state().capture.version;
        let rotation = Rotation::Cw90;
        assert_eq!(
            engine.handle(Command::SetRotation { rotation }).unwrap(),
            Reply::Done
        );
        assert_eq!(
            engine.handle(Command::SetRotation { rotation }).unwrap(),
            Reply::Unchanged
        );
        assert_eq!(engine.state().capture.value.view, Some((6, 8)));
        assert_eq!(engine.state().capture.version, version + 1);
        // The change made by the command is still sent by the next pump.
        assert!(kinds(&engine.pump(Instant::now())).contains(&"capture"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_new_source_drops_the_capture_and_has_no_camera_controls() {
        let path = image("source", 8, 6);
        let (mut engine, _) = start(SourceSpec::Image(path.clone()), Config::default(), None);
        pump_until(&mut engine, |e| matches!(e, Event::Frame { .. }));
        engine.handle(Command::Freeze).unwrap();
        let source = SourceSpec::TestPattern;
        assert_eq!(
            engine
                .handle(Command::UseSource {
                    source: source.clone()
                })
                .unwrap(),
            Reply::Done
        );
        assert!(engine.captured().is_none());
        let events = pump_until(
            &mut engine,
            |e| matches!(e, Event::Stream(s) if matches!(s.value.status, StreamStatus::Streaming { width: 640, .. })),
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Notice(n) if n.text == "showing test pattern")));
        assert_eq!(engine.state().stream.value.source, source);
        assert!(!engine.state().stream.value.capabilities.zoom);
        assert_eq!(
            engine.handle(Command::SetZoom { zoom: 2.0 }).unwrap(),
            Reply::Unchanged
        );
        assert_eq!(
            engine.handle(Command::SetTorch { on: true }).unwrap(),
            Reply::Unchanged
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn set_config_keeps_unchanged_backends_rebuilds_the_rest_and_saves() {
        let file =
            std::env::temp_dir().join(format!("squigl-engine-config-{}.toml", std::process::id()));
        let (mut engine, counts) = start(
            SourceSpec::TestPattern,
            Config::default(),
            Some(file.clone()),
        );
        // Built in, the hint API and llama-server; one detector.
        assert_eq!(counts.backends_built.load(Ordering::Relaxed), 3);
        assert_eq!(counts.detectors_built.load(Ordering::Relaxed), 1);
        let built_in = Arc::clone(&engine.backends()[0]);

        let mut config = engine.config().clone();
        config.ui.scale = 1.5;
        assert_eq!(
            engine
                .handle(Command::SetConfig {
                    config: Box::new(config.clone())
                })
                .unwrap(),
            Reply::Done
        );
        assert_eq!(
            counts.backends_built.load(Ordering::Relaxed),
            3,
            "nothing rebuilt"
        );
        assert_eq!(counts.detectors_built.load(Ordering::Relaxed), 1);
        assert!(Arc::ptr_eq(&built_in, &engine.backends()[0]));
        let saved: Config = toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved, config);

        config.backends.truncate(2);
        if let BackendConfig::HintApi { samples, .. } = &mut config.backends[1] {
            *samples = 5;
        }
        config.layout.threshold = 0.3;
        engine
            .handle(Command::SetConfig {
                config: Box::new(config.clone()),
            })
            .unwrap();
        assert_eq!(
            counts.backends_built.load(Ordering::Relaxed),
            4,
            "only the changed one"
        );
        assert_eq!(counts.detectors_built.load(Ordering::Relaxed), 2);
        assert_eq!(engine.backends().len(), 2);
        assert!(Arc::ptr_eq(&built_in, &engine.backends()[0]));
        assert_eq!(engine.state().reading.value.backends.len(), 2);
        assert_eq!(
            engine
                .handle(Command::SetConfig {
                    config: Box::new(config)
                })
                .unwrap(),
            Reply::Unchanged
        );
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn a_model_is_prepared_on_request_and_its_progress_wakes_the_front_end() {
        let (mut engine, counts) = start(SourceSpec::TestPattern, Config::default(), None);
        let name = engine.backends()[0].name().to_string();
        engine.pump(Instant::now());
        // Lazy: listed, not prepared; the remote backends are not models.
        let models = &engine.state().models.value.models;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, name);
        assert_eq!(models[0].phase, ModelPhase::NotInstalled { size: 1 });

        let wakes = counts.wakes.load(Ordering::Relaxed);
        engine
            .handle(Command::PrepareModel { name: name.clone() })
            .unwrap();
        assert!(counts.wakes.load(Ordering::Relaxed) > wakes);
        let events = engine.pump(Instant::now());
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Models(m) if m.value.models[0].phase.is_ready()
        )));
        engine
            .handle(Command::CancelModelDownload { name })
            .unwrap();
        assert!(engine.state().models.value.models[0].phase.is_idle());
        assert!(
            engine
                .handle(Command::PrepareModel {
                    name: "workbench GLM-OCR".into()
                })
                .is_err(),
            "a remote backend has nothing to prepare"
        );
    }

    #[test]
    fn commands_and_events_have_a_stable_wire_form() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"set-rotation","rotation":"cw90"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::SetRotation {
                rotation: Rotation::Cw90
            }
        );
        let cmd: Command = serde_json::from_str(
            r#"{"type":"use-source","source":{"kind":"image","path":"/scans/p1.png"}}"#,
        )
        .unwrap();
        assert_eq!(
            cmd,
            Command::UseSource {
                source: SourceSpec::Image("/scans/p1.png".into())
            }
        );
        let event = Event::Frame { seq: 3 };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"frame","data":{"seq":3}}"#
        );
    }
}
