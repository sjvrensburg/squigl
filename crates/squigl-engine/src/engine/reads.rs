//! Reading: the selection on the view, block detection, hand erasures, reads (one,
//! every block in turn, a second opinion) on threads of their own, their results
//! and the session's history. Every front end gets the same behaviour from here; it
//! runs in [`Engine::handle`] and [`Engine::pump`] like the rest of the engine.
//!
//! Everything is in *view* space (the frame as rotated). A read is always of a
//! capture: reading a live frame freezes it first, so the answer stays next to its
//! ink. Results go with the capture and the selection (or "every block") they were
//! read from, and are dropped when either goes; the history keeps every read.

use super::{Engine, Level, ReadInProgress, ReadResult, Reply};
use crate::erase::Stroke;
use crate::geometry::{Crop, Selection};
use crate::history::{self, History};
use crate::layout::{self, Block, BlockDetector, Role};
use crate::render::{render_region, render_selection};
use crate::transcribe::{Mode, Transcription};
use crate::view::FrameRef;
use anyhow::Result;
use squigl_core::convert::Rotation;
use squigl_core::decode::YuvFrame;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest edge handed to the block detector (its own input is 800 px square).
const DETECT_MAX_EDGE: usize = 1600;
/// How often, at most, the detector runs on the live picture.
pub const LIVE_DETECT_INTERVAL: Duration = Duration::from_millis(200);
/// Blocks with a side shorter than this fraction of the view's shorter edge are
/// slivers the detector leaves at line edges, not blocks.
const MIN_BLOCK_FRACTION: f32 = 0.012;
/// How many times a read is sent again when its model refused it while reloading
/// (a lost GPU: it comes back on the CPU).
const RELOAD_RETRIES: u32 = 2;

/// What a results list belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Scope {
    /// One read of this selection (`None`: the whole page).
    One(Option<Selection>),
    /// Every block, in order.
    AllBlocks,
}

/// A read to make: by which backend, of what, how, and where it is listed.
#[derive(Debug, Clone)]
struct ReadRequest {
    backend: usize,
    selection: Option<Selection>,
    mode: Mode,
    scope: Scope,
    label: Option<String>,
    /// Times it has been sent again after a reload.
    retries: u32,
}

/// A read in flight.
struct PendingRead {
    rx: Receiver<anyhow::Result<Transcription>>,
    started: Instant,
    name: String,
    request: ReadRequest,
}

/// The reading half of the engine's state.
#[derive(Default)]
pub(super) struct Reads {
    /// In view space.
    selection: Option<Selection>,
    /// What the selection is, when it came from a block: picks the prompt.
    role: Option<Role>,
    /// Which block the selection is, for stepping on from.
    selected_block: Option<usize>,
    /// The view size the selection and blocks belong to: a frame of another size
    /// (a camera switch) drops them.
    space: Option<(usize, usize)>,
    /// Painted over with paper before anything reads the capture, oldest first.
    erased: Vec<Stroke>,
    /// The capture `erased` belongs to.
    erased_capture: u64,
    /// The capture, and it with `erased` painted over: what is shown of it.
    erased_view: Option<(Arc<YuvFrame>, Arc<YuvFrame>)>,
    block_mode: bool,
    /// Of the shown frame, in reading order.
    blocks: Vec<Block>,
    /// The frame and rotation `blocks` were found on.
    blocks_key: Option<(Arc<YuvFrame>, Rotation)>,
    detecting: Option<(Receiver<Result<Vec<Block>>>, Instant)>,
    last_detect: Option<Instant>,
    selected_backend: usize,
    pending: Option<PendingRead>,
    /// A read refused while its model reloads, sent again once it is ready.
    retry: Option<ReadRequest>,
    results: Vec<ReadResult>,
    /// What `results` were read from.
    results_key: Option<(Arc<YuvFrame>, Scope)>,
    /// Bumped whenever `results` is emptied, so a front end's per-result state can
    /// tell a new list from a longer one.
    results_generation: u64,
    /// Blocks still to read in a "read all": label, where, how -- taken when it
    /// began, so a re-detection cannot reshuffle them.
    queue: VecDeque<(String, Selection, Mode)>,
    /// A "read all" waiting for the capture's own blocks.
    read_all_armed: bool,
    history: History,
    /// [`super::Event`]s about results, for the next pump.
    pub(super) events: Vec<super::Event>,
}

impl Reads {
    pub(super) fn selected_backend(&self) -> usize {
        self.selected_backend
    }
}

impl Engine {
    // --- What a front end reads -------------------------------------------------

    /// The selection, in view space.
    pub fn selection(&self) -> Option<Selection> {
        self.reads.selection
    }

    /// The block the selection is, if it is one.
    pub fn selected_block(&self) -> Option<usize> {
        self.reads.selected_block
    }

    /// Whether the detector runs on whatever is shown.
    pub fn block_mode(&self) -> bool {
        self.reads.block_mode
    }

    /// The shown frame's blocks, in reading order, in view space.
    pub fn blocks(&self) -> &[Block] {
        &self.reads.blocks
    }

    /// Whether a detection is running.
    pub fn detecting(&self) -> bool {
        self.reads.detecting.is_some()
    }

    /// The capture's erasures, oldest first.
    pub fn erased(&self) -> &[Stroke] {
        &self.reads.erased
    }

    /// The erasures that apply to `frame`: the capture's, none on a live frame.
    pub fn erased_on(&self, frame: &Arc<YuvFrame>) -> &[Stroke] {
        match &self.capture {
            Some((_, c)) if Arc::ptr_eq(c, frame) => &self.reads.erased,
            _ => &[],
        }
    }

    /// The capture's erasures to add to: none while the picture is live (adding
    /// one captures it, and a new capture has none).
    pub fn erased_on_capture(&self) -> Vec<Stroke> {
        if self.capture.is_some() {
            self.reads.erased.clone()
        } else {
            Vec::new()
        }
    }

    /// The view size the selection belongs to, once there has been a frame.
    pub fn view_size(&self) -> Option<(usize, usize)> {
        self.reads.space
    }

    /// The mode a read of the selection takes: a formula block is read as maths,
    /// any other selection as handwriting, no selection as a page.
    pub fn read_mode(&self) -> Mode {
        match (self.reads.selection, self.reads.role) {
            (None, _) => Mode::Page,
            (Some(_), Some(Role::Formula)) => Mode::Formula,
            (Some(_), _) => Mode::Crop,
        }
    }

    pub fn selected_backend(&self) -> usize {
        self.reads.selected_backend
    }

    /// Whether the selected backend can take a read now (a built-in model may still
    /// be downloading or loading).
    pub fn backend_ready(&self) -> bool {
        self.backend_is_ready(self.reads.selected_backend)
    }

    fn backend_is_ready(&self, i: usize) -> bool {
        self.backends
            .get(i)
            .and_then(|b| b.status())
            .is_none_or(|s| s.starts_with("ready") || s.starts_with("unavailable"))
    }

    /// The read in flight (or waiting to be sent again), and when it began.
    pub fn reading(&self) -> Option<(ReadInProgress, Instant)> {
        let r = &self.reads;
        let (name, label, started) = match (&r.pending, &r.retry) {
            (Some(p), _) => (p.name.clone(), p.request.label.clone(), p.started),
            (None, Some(q)) => (
                self.backends.get(q.backend)?.name().to_string(),
                q.label.clone(),
                Instant::now(),
            ),
            (None, None) => return None,
        };
        Some((
            ReadInProgress {
                backend: name,
                label,
                queued: r.queue.len(),
            },
            started,
        ))
    }

    /// The results for the current capture and selection.
    pub fn results(&self) -> &[ReadResult] {
        &self.reads.results
    }

    /// Changes whenever the results list is emptied.
    pub fn results_generation(&self) -> u64 {
        self.reads.results_generation
    }

    /// Whether the results are every block's, in page order (else one read after
    /// another, newest last).
    pub fn results_in_page_order(&self) -> bool {
        matches!(self.reads.results_key, Some((_, Scope::AllBlocks)))
    }

    /// Every read of the session.
    pub fn history(&self) -> &History {
        &self.reads.history
    }

    /// The smallest detected block under a view point.
    pub fn block_at(&self, (x, y): (usize, usize)) -> Option<usize> {
        self.reads
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.rect.contains(x, y))
            .min_by_key(|(_, b)| b.rect.area())
            .map(|(i, _)| i)
    }

    // --- Commands ---------------------------------------------------------------

    /// A hand edit of the selection: a block's quad and role no longer apply.
    pub(super) fn set_selection(&mut self, rect: Option<Crop>) -> Reply {
        let rect = match (rect, self.reads.space) {
            (Some(r), Some((vw, vh))) => r.clamped(vw, vh),
            (r, _) => r,
        };
        let selection = rect.map(|rect| Selection { rect, quad: None });
        if selection == self.reads.selection && self.reads.role.is_none() {
            return Reply::Unchanged;
        }
        self.reads.selection = selection;
        self.reads.role = None;
        self.reads.selected_block = None;
        Reply::Done
    }

    /// Moves corner `i` (clockwise from the top-left) of the selection to `here`.
    /// A rectangle stays one (the opposite corner is fixed); a quad's corner moves
    /// on its own and the rectangle around it follows, keeping the quad.
    pub(super) fn move_corner(&mut self, i: usize, here: (usize, usize)) -> Reply {
        let (Some(sel), Some((vw, vh))) = (self.reads.selection, self.reads.space) else {
            return Reply::Unchanged;
        };
        let i = i % 4;
        match sel.quad {
            None => {
                let q = layout::rect_quad(sel.rect);
                let opposite = q[(i + 2) % 4];
                let opposite = (opposite[0] as usize, opposite[1] as usize);
                match Crop::from_corners(opposite, here).clamped(vw, vh) {
                    Some(rect) => self.set_selection(Some(rect)),
                    None => Reply::Unchanged,
                }
            }
            Some(mut q) => {
                q[i] = [here.0 as f32, here.1 as f32];
                let (x0, x1) = q.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                    (lo.min(p[0]), hi.max(p[0]))
                });
                let (y0, y1) = q.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                    (lo.min(p[1]), hi.max(p[1]))
                });
                let rect = Crop {
                    x: x0.floor() as usize,
                    y: y0.floor() as usize,
                    w: (x1.ceil() - x0.floor()) as usize,
                    h: (y1.ceil() - y0.floor()) as usize,
                };
                match rect.clamped(vw, vh) {
                    Some(rect) => {
                        self.reads.selection = Some(Selection {
                            rect,
                            quad: Some(q),
                        });
                        Reply::Done
                    }
                    None => Reply::Unchanged,
                }
            }
        }
    }

    /// Makes block `i` the selection.
    pub(super) fn select_block(&mut self, i: usize) -> Reply {
        let Some(b) = self.reads.blocks.get(i) else {
            return Reply::Unchanged;
        };
        self.reads.selection = Some(Selection {
            rect: b.rect,
            quad: b.quad,
        });
        self.reads.role = Some(b.role());
        self.reads.selected_block = Some(i);
        Reply::Done
    }

    /// The next (or previous) block in reading order.
    pub(super) fn step_block(&mut self, delta: i32) -> Reply {
        if self.reads.blocks.is_empty() {
            self.notice(Level::Info, "no blocks: turn on block detection first");
            return Reply::Unchanged;
        }
        let n = self.reads.blocks.len() as i32;
        let next = match self.reads.selected_block {
            Some(i) => (i as i32 + delta).rem_euclid(n),
            None if delta < 0 => n - 1,
            None => 0,
        };
        self.select_block(next as usize)
    }

    pub(super) fn set_block_mode(&mut self, on: bool) -> Reply {
        if on == self.reads.block_mode {
            return Reply::Unchanged;
        }
        if on {
            let Some(detector) = &self.detector else {
                self.notice(Level::Warning, "no block detector in this build");
                return Reply::Unchanged;
            };
            if let Some(status) = detector.status() {
                self.notice(Level::Info, status);
                return Reply::Unchanged;
            }
        }
        self.reads.block_mode = on;
        if !on {
            self.clear_blocks();
        }
        Reply::Done
    }

    /// Drops every erasure (a rotation: they are in view space).
    pub(super) fn reads_erase_all(&mut self) {
        self.reads.erased.clear();
        self.reads.erased_view = None;
    }

    /// The frame to show for `frame`: the capture with its erasures painted over,
    /// else `frame` itself.
    pub(super) fn shown_erased(&self, frame: Arc<YuvFrame>) -> Arc<YuvFrame> {
        match &self.reads.erased_view {
            Some((capture, erased)) if Arc::ptr_eq(capture, &frame) => erased.clone(),
            _ => frame,
        }
    }

    /// Paints the erasures over the capture once, for every view of it to show.
    fn refresh_erased_view(&mut self) {
        self.reads.erased_view = match &self.capture {
            Some((_, frame)) if !self.reads.erased.is_empty() => Some((
                frame.clone(),
                Arc::new(crate::render::erased_frame(
                    frame,
                    self.rotation,
                    &self.reads.erased,
                )),
            )),
            _ => None,
        };
    }

    pub(super) fn set_erasures(&mut self, strokes: Vec<Stroke>) -> Reply {
        if strokes == self.reads.erased {
            return Reply::Unchanged;
        }
        if !strokes.is_empty() && self.capture.is_none() {
            // Erasures belong to a capture: painting a live frame freezes it.
            if self.freeze().is_err() {
                return Reply::Unchanged;
            }
        }
        self.reads.erased = strokes;
        self.reads.erased_capture = self.capture_seq();
        self.refresh_erased_view();
        Reply::Done
    }

    pub(super) fn select_backend(&mut self, i: usize) -> Reply {
        if i == self.reads.selected_backend || i >= self.backends.len() {
            return Reply::Unchanged;
        }
        self.reads.selected_backend = i;
        Reply::Done
    }

    /// Reads the selection (or the whole view) with the selected backend.
    pub(super) fn read(&mut self) -> Reply {
        self.reads.queue.clear();
        let selection = self.reads.selection;
        self.read_with(ReadRequest {
            backend: self.reads.selected_backend,
            selection,
            mode: self.read_mode(),
            scope: Scope::One(selection),
            label: None,
            retries: 0,
        })
    }

    /// Reads every detected block in reading order, one after another. Reads are
    /// of a capture, so a live view is captured first and its own detection awaited.
    pub(super) fn read_all(&mut self) -> Reply {
        if !self.reads.block_mode {
            self.notice(Level::Info, "turn on block detection first");
            return Reply::Unchanged;
        }
        if self.capture.is_none() && self.freeze().is_err() {
            self.notice(Level::Info, "nothing to read yet");
            return Reply::Unchanged;
        }
        self.reads.read_all_armed = true;
        self.start_read_all_if_ready();
        Reply::Done
    }

    /// The selection read again by the next backend in the list, listed alongside.
    /// The selected backend does not change.
    pub(super) fn second_opinion(&mut self) -> Reply {
        if self.backends.len() < 2 {
            self.notice(
                Level::Info,
                "a second opinion needs a second backend (Settings)",
            );
            return Reply::Unchanged;
        }
        self.reads.queue.clear();
        let selection = self.reads.selection;
        // Keep the list: same scope as the reading it seconds.
        let scope = match self.reads.results_key {
            Some((_, Scope::AllBlocks)) => Scope::AllBlocks,
            _ => Scope::One(selection),
        };
        let label = self
            .reads
            .results
            .last()
            .and_then(|e| e.label.clone())
            .map(|l| format!("{l} (2nd opinion)"));
        self.read_with(ReadRequest {
            backend: (self.reads.selected_backend + 1) % self.backends.len(),
            selection,
            mode: self.read_mode(),
            scope,
            label,
            retries: 0,
        })
    }

    /// Stops waiting for the read in flight (its thread finishes unheard) and drops
    /// the rest of a "read all".
    pub(super) fn cancel_read(&mut self) -> Reply {
        let r = &mut self.reads;
        if r.pending.is_none() && r.retry.is_none() && r.queue.is_empty() && !r.read_all_armed {
            return Reply::Unchanged;
        }
        r.pending = None;
        r.retry = None;
        r.queue.clear();
        r.read_all_armed = false;
        self.notice(Level::Info, "reading stopped");
        Reply::Done
    }

    pub(super) fn clear_results(&mut self) -> Reply {
        if self.reads.results.is_empty() {
            return Reply::Unchanged;
        }
        self.empty_results();
        Reply::Done
    }

    /// Writes the selection (or the whole view) of what is shown at native
    /// resolution, turned as shown, erasures painted, into `dir`.
    pub fn save(&mut self, dir: &Path) -> Result<PathBuf> {
        let frame = self
            .frame(FrameRef::Shown)
            .ok_or_else(|| anyhow::anyhow!("nothing to save yet"))?;
        let erased = self.erased_on(&frame).to_vec();
        let (rgba, w, h) = render_selection(
            &frame,
            self.rotation,
            self.reads.selection,
            1,
            &erased,
            None,
        );
        let path = dir.join(format!(
            "squigl-{}.png",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        std::fs::create_dir_all(dir)?;
        image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8)?;
        self.notice(Level::Info, format!("saved {w}x{h} to {}", path.display()));
        Ok(path)
    }

    /// Writes the session's readings as Markdown into `dir`.
    pub fn save_history(&mut self, dir: &Path) -> Result<PathBuf> {
        let path = self.reads.history.save(dir, &self.config().ui)?;
        self.notice(Level::Info, format!("readings saved to {}", path.display()));
        Ok(path)
    }

    /// After the backends were rebuilt: the one that was selected stays selected
    /// (by name; else the first), and block mode goes if the detector did.
    pub(super) fn follow_backends(&mut self, selected: Option<String>) {
        self.reads.selected_backend = selected
            .and_then(|n| self.backends.iter().position(|b| b.name() == n))
            .unwrap_or(0);
        if self.detector.is_none() && self.reads.block_mode {
            self.reads.block_mode = false;
            self.clear_blocks();
        }
    }

    // --- In the background ------------------------------------------------------

    /// What `pump` does for reading: keeps the selection and blocks to the shown
    /// view, collects a finished read or detection, sends the next read, and starts
    /// a detection when one is due.
    pub(super) fn pump_reads(&mut self) {
        self.follow_view();
        self.collect_read();
        self.collect_detection();
        self.send_retry();
        self.maybe_detect();
    }

    /// The selection and blocks are of a view: a frame of another size drops them;
    /// erasures are of a capture, and go with it; results go with their capture.
    fn follow_view(&mut self) {
        if self.reads.erased_capture != self.capture_seq() {
            self.reads_erase_all();
            self.reads.erased_capture = self.capture_seq();
        }
        let view = self
            .frame(FrameRef::Shown)
            .map(|f| self.rotation.rotated_size(f.width, f.height));
        if let Some(view) = view {
            if self.reads.space != Some(view) {
                if self.reads.space.is_some() {
                    self.reads.selection = None;
                    self.reads.role = None;
                    self.clear_blocks();
                }
                self.reads.space = Some(view);
            }
            let clamped = self
                .reads
                .selection
                .and_then(|s| s.rect.clamped(view.0, view.1));
            if clamped != self.reads.selection.map(|s| s.rect) {
                self.set_selection(clamped);
            }
        }
        let stale_blocks = !self.reads.block_mode
            || self
                .reads
                .blocks_key
                .as_ref()
                .is_some_and(|(_, r)| *r != self.rotation);
        if stale_blocks {
            self.clear_blocks();
        }
        let stale_results = match (&self.reads.results_key, &self.capture) {
            (Some((a, _)), Some((_, b))) => !Arc::ptr_eq(a, b),
            (Some(_), None) => true,
            (None, _) => false,
        };
        if stale_results {
            self.empty_results();
            self.reads.results_key = None;
            self.reads.queue.clear();
        }
    }

    fn clear_blocks(&mut self) {
        self.reads.blocks.clear();
        self.reads.blocks_key = None;
        self.reads.selected_block = None;
    }

    fn empty_results(&mut self) {
        if !self.reads.results.is_empty() {
            self.reads.results.clear();
            self.reads.results_generation += 1;
            self.reads.events.push(super::Event::ResultsCleared);
        }
    }

    /// Results are kept while they are of the capture and `scope`; anything else
    /// starts a fresh list.
    fn set_results_key(&mut self, scope: Scope) {
        let key = self.captured().map(|f| (f, scope));
        let same = match (&self.reads.results_key, &key) {
            (Some((a, sa)), Some((b, sb))) => Arc::ptr_eq(a, b) && sa == sb,
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.empty_results();
            self.reads.results_key = key;
        }
    }

    /// Starts `request` on a thread of its own, capturing first if the picture is
    /// live.
    fn read_with(&mut self, request: ReadRequest) -> Reply {
        if self.reads.pending.is_some() {
            self.notice(Level::Info, "still reading the last one");
            return Reply::Unchanged;
        }
        let Some(backend) = self.backends.get(request.backend).cloned() else {
            self.notice(Level::Warning, "no transcription backends configured");
            return Reply::Unchanged;
        };
        if self.capture.is_none() && self.freeze().is_err() {
            self.notice(Level::Info, "nothing to read yet");
            return Reply::Unchanged;
        }
        let Some(frame) = self.captured() else {
            return Reply::Unchanged;
        };
        self.set_results_key(request.scope);
        let (vw, vh) = self.rotation.rotated_size(frame.width, frame.height);
        let enhance = self.config().enhance;
        let (rgba, w, h) = render_selection(
            &frame,
            self.rotation,
            request.selection,
            1,
            &self.reads.erased,
            Some(&enhance),
        );
        let mut png = Vec::new();
        let encoded = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
            .expect("buffer matches size")
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        if let Err(e) = encoded {
            self.notice(Level::Error, format!("encoding the crop failed: {e}"));
            return Reply::Unchanged;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let wake = Arc::clone(&self.models.notify);
        let name = backend.name().to_string();
        let prompt = self.config().prompts.for_mode(request.mode).to_string();
        let mode = request.mode;
        std::thread::Builder::new()
            .name("squigl-read".into())
            .spawn(move || {
                let result = backend.read(&png, mode, &prompt, (vw as u32, vh as u32));
                let _ = tx.send(result);
                wake();
            })
            .expect("spawning a read thread");
        self.reads.pending = Some(PendingRead {
            rx,
            started: Instant::now(),
            name,
            request,
        });
        Reply::Done
    }

    /// The armed "read all" begins once the blocks are the capture's.
    fn start_read_all_if_ready(&mut self) {
        if !self.reads.read_all_armed || self.reads.detecting.is_some() {
            return;
        }
        let Some(captured) = self.captured() else {
            self.reads.read_all_armed = false;
            return;
        };
        if !self
            .reads
            .blocks_key
            .as_ref()
            .is_some_and(|(f, _)| Arc::ptr_eq(f, &captured))
        {
            return;
        }
        self.reads.read_all_armed = false;
        if self.reads.blocks.is_empty() {
            self.notice(Level::Info, "no blocks found on this page");
            return;
        }
        self.reads.queue = self
            .reads
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let mode = if b.role() == Role::Formula {
                    Mode::Formula
                } else {
                    Mode::Crop
                };
                (
                    format!("#{} {}", i + 1, b.label),
                    Selection {
                        rect: b.rect,
                        quad: b.quad,
                    },
                    mode,
                )
            })
            .collect();
        self.set_results_key(Scope::AllBlocks);
        self.empty_results();
        self.next_queued_read();
    }

    /// Starts the next block of a "read all" once the previous one has landed.
    fn next_queued_read(&mut self) {
        if self.reads.pending.is_some() || self.reads.retry.is_some() {
            return;
        }
        if let Some((label, selection, mode)) = self.reads.queue.pop_front() {
            self.reads.selection = Some(selection);
            self.reads.selected_block = None;
            self.read_with(ReadRequest {
                backend: self.reads.selected_backend,
                selection: Some(selection),
                mode,
                scope: Scope::AllBlocks,
                label: Some(label),
                retries: 0,
            });
        }
    }

    /// Takes in a finished read: into the results and the history, unless its model
    /// refused it while reloading, when it waits to be sent again.
    fn collect_read(&mut self) {
        let Some(pending) = &self.reads.pending else {
            return;
        };
        let result = match pending.rx.try_recv() {
            Ok(result) => result.map_err(|e| format!("{e:#}")),
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the read thread died".into()),
        };
        let pending = self.reads.pending.take().expect("checked");
        let request = pending.request;
        if let Err(e) = &result {
            if e.contains("try again shortly") && request.retries < RELOAD_RETRIES {
                self.notice(
                    Level::Warning,
                    format!("{e}; reading again once it is ready"),
                );
                self.reads.retry = Some(ReadRequest {
                    retries: request.retries + 1,
                    ..request
                });
                return;
            }
        }
        let what = request.label.clone().unwrap_or_else(|| {
            if request.selection.is_some() {
                "box".into()
            } else {
                "page".into()
            }
        });
        let entry = history::Entry {
            at: chrono::Local::now(),
            capture: self.captures as u32,
            what,
            result: result.clone(),
        };
        self.reads.history.push(entry.clone());
        self.reads.events.push(super::Event::HistoryAppended(entry));
        let read = ReadResult {
            label: request.label,
            result,
        };
        self.reads.results.push(read.clone());
        self.reads.events.push(super::Event::ResultAppended(read));
        self.next_queued_read();
    }

    /// Sends a refused read again once its backend is ready.
    fn send_retry(&mut self) {
        let ready = self
            .reads
            .retry
            .as_ref()
            .is_some_and(|r| self.backend_is_ready(r.backend));
        if ready && self.reads.pending.is_none() {
            let request = self.reads.retry.take().expect("checked");
            // The capture it was of may have gone; then so has the read.
            if self.capture.is_some() {
                self.read_with(request);
            }
        }
    }

    fn collect_detection(&mut self) {
        let Some((rx, started)) = &self.reads.detecting else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("the detection thread died"))
            }
        };
        let elapsed = started.elapsed();
        self.reads.detecting = None;
        match result {
            Ok(blocks) => {
                if self.capture.is_some() {
                    self.notice(
                        Level::Info,
                        format!(
                            "{} block{} in {:.2}s",
                            blocks.len(),
                            if blocks.len() == 1 { "" } else { "s" },
                            elapsed.as_secs_f32()
                        ),
                    );
                }
                self.follow_selection(&blocks);
                self.reads.blocks = blocks;
            }
            Err(e) => {
                // Live, it would fail again every tick.
                self.reads.block_mode = false;
                self.clear_blocks();
                self.notice(Level::Error, format!("block detection failed: {e:#}"));
            }
        }
        self.start_read_all_if_ready();
    }

    /// In block mode, runs the detector over what is shown whenever it is not
    /// already running, the frame is new, and (live) the interval has passed. A read
    /// in flight has the GPU to itself.
    fn maybe_detect(&mut self) {
        let r = &self.reads;
        if !r.block_mode || r.detecting.is_some() || r.pending.is_some() {
            return;
        }
        let Some(detector) = self.detector.clone() else {
            return;
        };
        if !detector.ready() {
            return;
        }
        let Some(frame) = self.frame(FrameRef::Shown) else {
            return;
        };
        if r.blocks_key
            .as_ref()
            .is_some_and(|(f, rot)| Arc::ptr_eq(f, &frame) && *rot == self.rotation)
        {
            return;
        }
        if self.capture.is_none()
            && r.last_detect
                .is_some_and(|at| at.elapsed() < LIVE_DETECT_INTERVAL)
        {
            return;
        }
        self.detect(detector, frame);
    }

    /// Runs the detector over `frame` on a thread; the blocks land in view space.
    fn detect(&mut self, detector: Arc<dyn BlockDetector>, frame: Arc<YuvFrame>) {
        let rotation = self.rotation;
        let (vw, vh) = rotation.rotated_size(frame.width, frame.height);
        let step = vw.max(vh).div_ceil(DETECT_MAX_EDGE).max(1);
        let (rgba, w, h) = render_region(&frame, rotation, Crop::whole(vw, vh), step);
        let img = layout::rgba_to_rgb(&rgba, w, h);
        self.reads.blocks_key = Some((Arc::clone(&frame), rotation));
        let (tx, rx) = mpsc::sync_channel(1);
        let wake = Arc::clone(&self.models.notify);
        std::thread::Builder::new()
            .name("squigl-detect".into())
            .spawn(move || {
                // Back from the decimated image to view pixels.
                let min_side = (vw.min(vh) as f32 * MIN_BLOCK_FRACTION) as usize;
                let result = detector.detect(&img).map(|blocks| {
                    blocks
                        .into_iter()
                        .filter_map(|mut b| {
                            if b.rect.w * step < min_side || b.rect.h * step < min_side {
                                return None;
                            }
                            b.rect = Crop {
                                x: b.rect.x * step,
                                y: b.rect.y * step,
                                w: b.rect.w * step,
                                h: b.rect.h * step,
                            }
                            .clamped(vw, vh)?;
                            b.quad = b
                                .quad
                                .map(|q| q.map(|[x, y]| [x * step as f32, y * step as f32]));
                            Some(b)
                        })
                        .collect()
                });
                let _ = tx.send(result);
                wake();
            })
            .expect("spawning a detection thread");
        self.reads.detecting = Some((rx, Instant::now()));
        self.reads.last_detect = Some(Instant::now());
    }

    /// Carries the selected block over to a fresh detection: the new block that
    /// overlaps it most keeps the selection (so stepping goes on from there), and if
    /// the selection was still exactly that block, it follows it.
    fn follow_selection(&mut self, new: &[Block]) {
        let Some(old) = self
            .reads
            .selected_block
            .and_then(|i| self.reads.blocks.get(i))
        else {
            self.reads.selected_block = None;
            return;
        };
        let untouched = self.reads.selection
            == Some(Selection {
                rect: old.rect,
                quad: old.quad,
            });
        let best = new
            .iter()
            .enumerate()
            .map(|(j, b)| (j, old.rect.iou(b.rect)))
            .filter(|(_, iou)| *iou > 0.3)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((j, _)) => {
                self.reads.selected_block = Some(j);
                if untouched {
                    self.reads.selection = Some(Selection {
                        rect: new[j].rect,
                        quad: new[j].quad,
                    });
                }
            }
            None => self.reads.selected_block = None,
        }
    }

    /// The slices' view of reading: (blocks, reading).
    pub(super) fn reads_slices(&self) -> (super::BlocksSlice, super::ReadingSlice) {
        let r = &self.reads;
        (
            super::BlocksSlice {
                enabled: r.block_mode,
                detecting: r.detecting.is_some(),
                blocks: r.blocks.clone(),
                selected: r.selected_block,
            },
            super::ReadingSlice {
                backends: self.backends.iter().map(|b| b.name().to_string()).collect(),
                selected: r.selected_backend,
                reading: self.reading().map(|(r, _)| r),
                results: r.results.len(),
                history: r.history.entries.len(),
                mode: self.read_mode(),
                in_page_order: self.results_in_page_order(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Command, EngineDeps, EngineOptions, Event, Reply};
    use super::*;
    use crate::config::Config;
    use crate::stream::{Resolution, SourceSpec, StreamConfig};
    use crate::transcribe::{BackendConfig, Reading, Transcriber};
    use squigl_core::ConnectOptions;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};

    /// Holds a fake's work until opened.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        changed: Condvar,
    }

    impl Gate {
        fn closed() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.changed.wait(open).unwrap();
            }
        }
        fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }

    /// What a fake reader was asked: the mode, the prompt, the image's size.
    type Asked = (Mode, String, (u32, u32));

    /// A backend that answers from a script ("ok: text" or an error), noting what
    /// it was asked, optionally held at a gate, with a status the test sets.
    #[derive(Clone)]
    struct Reader {
        name: String,
        script: Arc<Mutex<VecDeque<Result<String, String>>>>,
        asked: Arc<Mutex<Vec<Asked>>>,
        status: Arc<Mutex<Option<String>>>,
        gate: Option<Arc<Gate>>,
    }

    impl Reader {
        fn new(name: &str) -> Self {
            Self {
                name: name.into(),
                script: Arc::default(),
                asked: Arc::default(),
                status: Arc::default(),
                gate: None,
            }
        }
        fn answers(self, answers: &[Result<&str, &str>]) -> Self {
            *self.script.lock().unwrap() = answers
                .iter()
                .map(|a| a.map(String::from).map_err(String::from))
                .collect();
            self
        }
        fn asked(&self) -> Vec<Asked> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Transcriber for Reader {
        fn name(&self) -> &str {
            &self.name
        }
        fn status(&self) -> Option<String> {
            self.status.lock().unwrap().clone()
        }
        fn read(
            &self,
            png: &[u8],
            mode: Mode,
            prompt: &str,
            _: (u32, u32),
        ) -> Result<Transcription> {
            let img = image::load_from_memory(png).unwrap();
            self.asked.lock().unwrap().push((
                mode,
                prompt.to_string(),
                (img.width(), img.height()),
            ));
            if let Some(gate) = &self.gate {
                gate.wait();
            }
            let answer = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok("…".into()));
            let text = answer.map_err(|e| anyhow::anyhow!(e))?;
            Ok(Transcription {
                backend: self.name.clone(),
                readings: vec![Reading {
                    text,
                    count: 1,
                    truncated: false,
                    tokens: None,
                }],
                silent: 0,
                samples: 1,
                elapsed: Duration::ZERO,
            })
        }
    }

    /// A detector that finds `blocks`, counting its runs, optionally held at a gate.
    #[derive(Clone)]
    struct Detector {
        blocks: Vec<Block>,
        runs: Arc<AtomicUsize>,
        gate: Option<Arc<Gate>>,
    }

    impl BlockDetector for Detector {
        fn status(&self) -> Option<String> {
            None
        }
        fn ready(&self) -> bool {
            true
        }
        fn detect(&self, _: &image::RgbImage) -> Result<Vec<Block>> {
            self.runs.fetch_add(1, Ordering::Relaxed);
            if let Some(gate) = &self.gate {
                gate.wait();
            }
            Ok(self.blocks.clone())
        }
    }

    fn block(label: &'static str, x: usize, y: usize, w: usize, h: usize) -> Block {
        Block {
            label,
            score: 0.9,
            rect: Crop { x, y, w, h },
            quad: None,
        }
    }

    /// An engine showing a 400x300 image, reading with `readers` (in that order)
    /// and detecting with `detector`, once the image is shown.
    fn engine(test: &str, readers: &[Reader], detector: Option<Detector>) -> Engine {
        let path =
            std::env::temp_dir().join(format!("squigl-reads-{test}-{}.png", std::process::id()));
        image::RgbaImage::from_fn(400, 300, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        })
        .save(&path)
        .unwrap();
        let backends = readers
            .iter()
            .map(|r| BackendConfig::OpenAi {
                name: r.name.clone(),
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "m".into(),
                api_key: None,
                samples: 1,
                temperature: 0.0,
                max_tokens: 16,
            })
            .collect();
        let config = Config {
            backends,
            ..Config::default()
        };
        let by_name: HashMap<String, Reader> = readers
            .iter()
            .map(|r| (r.name.clone(), r.clone()))
            .collect();
        let deps = EngineDeps {
            backends: Box::new(move |c, _| {
                let BackendConfig::OpenAi { name, .. } = c else {
                    return None;
                };
                by_name
                    .get(name)
                    .map(|r| Box::new(r.clone()) as Box<dyn Transcriber>)
            }),
            detector: Box::new(move |_, _| {
                detector
                    .clone()
                    .map(|d| Arc::new(d) as Arc<dyn BlockDetector>)
            }),
        };
        let mut engine = Engine::new(
            config,
            StreamConfig {
                source: SourceSpec::Image(path),
                options: ConnectOptions::default(),
                resolution: Resolution::PhoneDefault,
                tee_device: None,
            },
            deps,
            EngineOptions {
                eager_models: false,
                config_file: None,
            },
            Arc::new(|| {}),
        );
        pump_until(&mut engine, |e, _| e.view_size().is_some());
        engine
    }

    /// Pumps until `done` holds, collecting the events; panics after 10 s.
    fn pump_until(engine: &mut Engine, done: impl Fn(&Engine, &[Event]) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        loop {
            all.extend(engine.pump(Instant::now()));
            if done(engine, &all) {
                return all;
            }
            assert!(Instant::now() < deadline, "timed out; saw {all:#?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn texts(engine: &Engine) -> Vec<String> {
        engine
            .results()
            .iter()
            .map(|r| match &r.result {
                Ok(t) => t.readings[0].text.clone(),
                Err(e) => format!("error: {e}"),
            })
            .collect()
    }

    #[test]
    fn a_read_captures_reads_the_selection_and_lists_it() {
        let first = Reader::new("first").answers(&[Ok("one")]);
        let second = Reader::new("second").answers(&[Ok("uno")]);
        let mut engine = engine("one", &[first.clone(), second.clone()], None);
        assert_eq!(engine.read_mode(), Mode::Page);
        let rect = Crop {
            x: 10,
            y: 20,
            w: 100,
            h: 50,
        };
        engine
            .handle(Command::SetSelection {
                selection: Some(rect),
            })
            .unwrap();
        assert_eq!(engine.read_mode(), Mode::Crop);
        assert_eq!(engine.handle(Command::Read).unwrap(), Reply::Done);
        assert_eq!(engine.capture_seq(), 1, "a read freezes the live picture");
        assert!(engine.reading().is_some());
        let events = pump_until(&mut engine, |e, _| e.results().len() == 1);
        assert!(events.iter().any(|e| matches!(e, Event::ResultAppended(_))));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::HistoryAppended(_))));
        assert_eq!(texts(&engine), ["one"]);
        let prompt = Config::default().prompts.crop;
        assert_eq!(first.asked(), [(Mode::Crop, prompt, (100, 50))]);
        assert_eq!(engine.history().entries[0].what, "box");
        assert_eq!(engine.history().entries[0].capture, 1);
        assert!(!engine.results_in_page_order());

        // A second opinion goes to the next backend and is listed alongside.
        assert_eq!(engine.handle(Command::SecondOpinion).unwrap(), Reply::Done);
        pump_until(&mut engine, |e, _| e.results().len() == 2);
        assert_eq!(texts(&engine), ["one", "uno"]);
        assert_eq!(second.asked().len(), 1);
        assert_eq!(engine.selected_backend(), 0);

        // The results go with the capture; the history keeps them.
        let generation = engine.results_generation();
        engine.handle(Command::Live).unwrap();
        let events = pump_until(&mut engine, |e, _| e.results().is_empty());
        assert!(events.iter().any(|e| matches!(e, Event::ResultsCleared)));
        assert_ne!(engine.results_generation(), generation);
        assert_eq!(engine.history().entries.len(), 2);
    }

    #[test]
    fn a_read_of_another_selection_starts_a_fresh_list() {
        let reader = Reader::new("first").answers(&[Ok("page"), Ok("box")]);
        let mut engine = engine("fresh", &[reader], None);
        engine.handle(Command::Read).unwrap();
        pump_until(&mut engine, |e, _| e.results().len() == 1);
        engine
            .handle(Command::SetSelection {
                selection: Some(Crop {
                    x: 0,
                    y: 0,
                    w: 50,
                    h: 50,
                }),
            })
            .unwrap();
        engine.handle(Command::Read).unwrap();
        pump_until(&mut engine, |e, _| {
            e.reading().is_none() && texts(e) == ["box"]
        });
        assert_eq!(engine.history().entries.len(), 2);
    }

    #[test]
    fn read_all_waits_for_the_captures_blocks_then_reads_them_in_order() {
        let reader = Reader::new("first").answers(&[Ok("words"), Ok("x^2")]);
        let gate = Gate::closed();
        let detector = Detector {
            blocks: vec![
                block("text", 10, 10, 100, 40),
                block("display_formula", 10, 100, 150, 40),
            ],
            runs: Arc::default(),
            gate: Some(Arc::clone(&gate)),
        };
        let mut engine = engine("all", std::slice::from_ref(&reader), Some(detector.clone()));
        assert_eq!(
            engine.handle(Command::ReadAll).unwrap(),
            Reply::Unchanged,
            "block mode first"
        );
        engine.handle(Command::SetBlockMode { on: true }).unwrap();
        pump_until(&mut engine, |e, _| e.detecting());
        assert_eq!(engine.handle(Command::ReadAll).unwrap(), Reply::Done);
        assert!(engine.captured().is_some());
        // Nothing is read until the blocks are in.
        for _ in 0..20 {
            engine.pump(Instant::now());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(engine.reading().is_none() && reader.asked().is_empty());
        gate.open();
        pump_until(&mut engine, |e, _| e.results().len() == 2);
        assert_eq!(texts(&engine), ["words", "x^2"]);
        let labels: Vec<_> = engine.results().iter().map(|r| r.label.clone()).collect();
        assert_eq!(
            labels,
            [Some("#1 text".into()), Some("#2 display_formula".into())]
        );
        let asked = reader.asked();
        assert_eq!((asked[0].0, asked[0].2), (Mode::Crop, (100, 40)));
        assert_eq!((asked[1].0, asked[1].2), (Mode::Formula, (150, 40)));
        assert!(engine.results_in_page_order());
        assert_eq!(
            detector.runs.load(Ordering::Relaxed),
            1,
            "one frame, one run"
        );
    }

    #[test]
    fn a_fresh_detection_keeps_the_selected_block() {
        let mut engine = engine("follow", &[], None);
        engine.reads.blocks = vec![block("text", 0, 0, 100, 100), block("text", 200, 0, 50, 50)];
        engine.handle(Command::SelectBlock { index: 1 }).unwrap();
        // Moved a little and now first: the selection follows it.
        let moved = vec![block("text", 205, 2, 50, 50), block("text", 0, 0, 100, 100)];
        engine.follow_selection(&moved);
        engine.reads.blocks = moved.clone();
        assert_eq!(engine.selected_block(), Some(0));
        assert_eq!(engine.selection().unwrap().rect, moved[0].rect);
        // A hand-edited selection keeps its place; only the block number follows.
        engine.select_block(0);
        engine.reads.selection = Some(Selection {
            rect: Crop {
                x: 205,
                y: 2,
                w: 40,
                h: 50,
            },
            quad: None,
        });
        let again = vec![block("text", 200, 0, 50, 50)];
        engine.follow_selection(&again);
        assert_eq!(engine.selected_block(), Some(0));
        assert_eq!(engine.selection().unwrap().rect.w, 40);
        // Nothing overlapping enough: no block is selected.
        engine.reads.blocks = again;
        engine.follow_selection(&[block("text", 0, 200, 50, 50)]);
        assert_eq!(engine.selected_block(), None);
    }

    #[test]
    fn a_read_refused_while_the_model_reloads_is_sent_again() {
        let reader = Reader::new("built-in").answers(&[
            Err("the GPU was lost; the model is reloading on the CPU, try again shortly"),
            Ok("read on the CPU"),
        ]);
        let mut engine = engine("retry", std::slice::from_ref(&reader), None);
        *reader.status.lock().unwrap() = Some("loading on the CPU…".into());
        engine.handle(Command::Read).unwrap();
        let events = pump_until(&mut engine, |e, _| {
            e.reads.pending.is_none() && e.reads.retry.is_some()
        });
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Notice(n) if n.text.contains("reading again"))));
        assert!(engine.results().is_empty());
        assert!(
            engine.reading().is_some(),
            "still reading, as far as anyone can see"
        );
        *reader.status.lock().unwrap() = None;
        pump_until(&mut engine, |e, _| e.results().len() == 1);
        assert_eq!(texts(&engine), ["read on the CPU"]);
        assert_eq!(engine.history().entries.len(), 1);
        assert_eq!(reader.asked().len(), 2);
    }

    #[test]
    fn a_read_gives_up_after_the_retries() {
        let lost = "the GPU was lost; the model is reloading on the CPU, try again shortly";
        let reader = Reader::new("built-in").answers(&[Err(lost), Err(lost), Err(lost)]);
        let mut engine = engine("give-up", std::slice::from_ref(&reader), None);
        engine.handle(Command::Read).unwrap();
        pump_until(&mut engine, |e, _| e.results().len() == 1);
        assert!(texts(&engine)[0].starts_with("error:"));
        assert_eq!(reader.asked().len(), 1 + RELOAD_RETRIES as usize);
    }

    #[test]
    fn a_cancelled_read_is_not_listed() {
        let gate = Gate::closed();
        let mut reader = Reader::new("slow").answers(&[Ok("late"), Ok("second")]);
        reader.gate = Some(Arc::clone(&gate));
        let mut engine = engine("cancel", &[reader.clone()], None);
        assert_eq!(
            engine.handle(Command::CancelRead).unwrap(),
            Reply::Unchanged
        );
        engine.handle(Command::Read).unwrap();
        assert_eq!(engine.handle(Command::CancelRead).unwrap(), Reply::Done);
        assert!(engine.reading().is_none());
        gate.open();
        std::thread::sleep(Duration::from_millis(50));
        engine.pump(Instant::now());
        assert!(engine.results().is_empty() && engine.history().entries.is_empty());
        // Reading goes on as before.
        engine.handle(Command::Read).unwrap();
        pump_until(&mut engine, |e, _| e.results().len() == 1);
        assert_eq!(texts(&engine), ["second"]);
    }

    #[test]
    fn the_selection_is_edited_in_view_space() {
        let mut engine = engine("select", &[], None);
        // Clamped to the 400x300 view.
        engine
            .handle(Command::SetSelection {
                selection: Some(Crop {
                    x: 350,
                    y: 250,
                    w: 100,
                    h: 100,
                }),
            })
            .unwrap();
        assert_eq!(
            engine.selection().unwrap().rect,
            Crop {
                x: 350,
                y: 250,
                w: 50,
                h: 50
            }
        );
        // A rectangle's corner: the opposite one stays.
        engine
            .handle(Command::MoveCorner {
                corner: 0,
                to: (100, 100),
            })
            .unwrap();
        assert_eq!(
            engine.selection().unwrap().rect,
            Crop {
                x: 100,
                y: 100,
                w: 300,
                h: 200
            }
        );
        // A block's quad keeps its other corners, and its role keeps the prompt.
        let mut tilted = block("display_formula", 10, 10, 100, 50);
        tilted.quad = Some([[12.0, 10.0], [110.0, 14.0], [108.0, 60.0], [10.0, 56.0]]);
        engine.reads.blocks = vec![tilted, block("text", 200, 200, 50, 50)];
        engine.handle(Command::SelectBlock { index: 0 }).unwrap();
        assert_eq!(engine.read_mode(), Mode::Formula);
        engine
            .handle(Command::MoveCorner {
                corner: 2,
                to: (130, 80),
            })
            .unwrap();
        let sel = engine.selection().unwrap();
        assert_eq!(sel.quad.unwrap()[2], [130.0, 80.0]);
        assert_eq!(sel.quad.unwrap()[0], [12.0, 10.0]);
        assert_eq!(
            sel.rect,
            Crop {
                x: 10,
                y: 10,
                w: 120,
                h: 70
            }
        );
        assert_eq!(engine.read_mode(), Mode::Formula);
        // Stepping wraps; the smallest block under a point is the one.
        engine.handle(Command::StepBlock { delta: 1 }).unwrap();
        assert_eq!(engine.selected_block(), Some(1));
        engine.handle(Command::StepBlock { delta: 1 }).unwrap();
        assert_eq!(engine.selected_block(), Some(0));
        assert_eq!(engine.block_at((220, 220)), Some(1));
        assert_eq!(engine.block_at((5, 5)), None);
        // A hand edit leaves the block behind.
        engine
            .handle(Command::SetSelection {
                selection: Some(Crop {
                    x: 0,
                    y: 0,
                    w: 20,
                    h: 20,
                }),
            })
            .unwrap();
        assert_eq!(
            (engine.selected_block(), engine.read_mode()),
            (None, Mode::Crop)
        );
        // Turning the view starts over.
        engine
            .handle(Command::SetRotation {
                rotation: Rotation::Cw90,
            })
            .unwrap();
        assert_eq!(engine.selection(), None);
        assert_eq!(engine.read_mode(), Mode::Page);
    }

    #[test]
    fn erasures_belong_to_their_capture() {
        let mut engine = engine("erase", &[], None);
        let stroke = Stroke::line([10.0, 10.0], [50.0, 10.0], 4.0);
        engine
            .handle(Command::SetErasures {
                strokes: vec![stroke.clone()],
            })
            .unwrap();
        assert_eq!(
            engine.capture_seq(),
            1,
            "erasing a live picture captures it"
        );
        engine.pump(Instant::now());
        assert_eq!(engine.erased(), std::slice::from_ref(&stroke));
        assert_eq!(engine.state().capture.value.erasures, [stroke]);
        let captured = engine.captured().unwrap();
        assert_eq!(engine.erased_on(&captured).len(), 1);
        // What is shown of the capture is erased; the capture itself is not.
        let whole = |engine: &Engine| {
            let request = crate::view::ViewRequest {
                frame: FrameRef::Captured,
                region: Crop::whole(400, 300),
                step: 1,
                format: squigl_core::convert::PlaneFormat::Luma,
            };
            engine.render_planes(&request).unwrap().planes.y
        };
        let shown = whole(&engine);
        let at = |y: &[u8], x: usize, row: usize| y[row * 400 + x];
        assert_ne!(at(&shown, 30, 10), at(&captured.y, 30, 10), "painted over");
        assert_eq!(at(&shown, 30, 100), at(&captured.y, 30, 100), "left alone");
        engine
            .handle(Command::SetErasures { strokes: vec![] })
            .unwrap();
        assert_eq!(
            whole(&engine),
            captured.y,
            "an undo shows the capture again"
        );
        engine.handle(Command::Live).unwrap();
        engine.pump(Instant::now());
        assert!(engine.erased().is_empty());
    }
}
