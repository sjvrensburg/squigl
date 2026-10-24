//! The GPUI window over `squigl-engine`: the same commands, slices and events
//! the Tauri window used, with no webview, IPC channel or frame socket between
//! the engine and the screen. The engine lives on this thread (as in the egui
//! window); background workers (stream, reads, models, voice) call the waker,
//! which sets a flag the pump loop collects at ~50 Hz, and a new live frame is
//! converted at most once per pump from the engine's decimated planes.

use crate::frame::{erasures_key, lut_key, to_render_image, FrameKey};
use crate::pairing::{Offer, Pairing};
use gpui_kit::*;
use squigl_core::convert::PlaneFormat;
use squigl_engine::config::{Config, Theme};
use squigl_engine::display::{DisplayMode, Lut};
use squigl_engine::engine::StreamStatus;
use squigl_engine::engine::{Command, Engine, Event, ReadResult, Reply};
use squigl_engine::erase::Stroke;
use squigl_engine::geometry::{Crop, Selection};
use squigl_engine::history;
use squigl_engine::layout::Role;
use squigl_engine::stream::{Problem, SourceSpec};
use squigl_engine::view::{FrameRef, Placement, Viewport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ZOOM_FACTOR: f32 = 1.25;

/// Canvas bounds in window coordinates: (origin x, origin y, width, height).
pub type CanvasBounds = Arc<Mutex<Option<(f32, f32, f32, f32)>>>;

/// Custom slider track bounds by name: (origin x, origin y, width, height).
pub type SliderBounds = Arc<Mutex<std::collections::HashMap<&'static str, (f32, f32, f32, f32)>>>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    Draw {
        from: (usize, usize),
        to: (usize, usize),
    },
    Move {
        from: (usize, usize),
        box_at: Crop,
        shown: Crop,
    },
    Corner(usize),
    Erase,
    Pan {
        from: [f32; 2],
        centre: [f32; 2],
    },
}

/// One finished typeset job: (cache key, Some((RGBA, w, h, scale))).
pub type TypesetJob = (String, Option<(Vec<u8>, usize, usize, f32)>);

pub enum TypesetEntry {
    Pending,
    Ready(Arc<RenderImage>, f32, usize, usize),
    Failed,
}

pub struct AppView {
    pub engine: Engine,
    pub focus: FocusHandle,
    pub results: Vec<ReadResult>,
    pub history: Vec<history::Entry>,
    pub notice: String,
    pub centre: [f32; 2],
    pub magnification: f32,
    pub image: Option<Arc<RenderImage>>,
    pub image_key: Option<FrameKey>,
    pub placement: Option<Placement>,
    pub view_region: Option<Crop>,
    /// Canvas bounds in window coordinates: (origin x, origin y, w, h).
    pub canvas: CanvasBounds,
    pub scale_factor: f32,
    pub drag: Option<Drag>,
    pub erase_points: Vec<[f32; 2]>,
    pub reading_open: bool,
    pub settings_open: bool,
    pub pair_open: bool,
    pub reading_only: bool,
    pub erasing: bool,
    pub brush: f32,
    pub erase_steps: Vec<usize>,
    pub blocks_asked: bool,
    pub guidance_hidden: Option<Problem>,
    pub pairing: Arc<Pairing>,
    pub pair_offers: Vec<Offer>,
    pub pair_note: String,
    /// Custom slider track bounds by name, in window coordinates.
    pub slider_bounds: SliderBounds,
    pub active_slider: Option<SliderKind>,
    pub pending_corner: Option<(usize, (usize, usize))>,
    pub shown_image: Option<Arc<RenderImage>>,
    pub pair_rx: Option<std::sync::mpsc::Receiver<Result<Vec<Offer>, String>>>,
    pub rebind_action: Option<String>,
    pub dev_phone: Option<squigl_pairing::testing::FakePhone>,
    pub dev_phone_seen: Option<(f32, Option<bool>)>,
    /// The Zoom pane (rectified selection at native pixels, as in egui).
    pub zoom_open: bool,
    pub zoom_image: Option<Arc<RenderImage>>,
    pub zoom_shown_image: Option<Arc<RenderImage>>,
    pub zoom_key: Option<u64>,
    /// Zoom canvas bounds and the fitted image's size in logical px.
    pub zoom_canvas: CanvasBounds,
    pub zoom_display: Option<(f32, f32)>,
    /// (native width, native height, step) of the zoom picture.
    pub zoom_native: Option<(usize, usize, usize)>,
    /// Which right-hand pane fills the window, if any ("zoom"/"reading").
    pub maximised: Option<&'static str>,
    /// Eased magnification/centre target (wheel and key zooms glide).
    pub view_target: Option<(f32, [f32; 2])>,
    pub detached_preview: Arc<crate::detach::DetachedShared>,
    pub detached_zoom: Arc<crate::detach::DetachedShared>,
    pub detached_reading: Arc<crate::detach::DetachedShared>,
    pub freeze_flash: Option<Instant>,
    pub detach_pending: Option<&'static str>,
    pub reading_width: Arc<Mutex<Option<f32>>>,
    /// Typeset readings (squigl-math), rendered off-thread per reading.
    pub typesetter: Option<Arc<dyn squigl_engine::typeset::Typesetter>>,
    pub typeset_cache: std::collections::HashMap<String, TypesetEntry>,
    pub typeset_rx: Option<std::sync::mpsc::Receiver<TypesetJob>>,
    pub typeset_tx: Option<std::sync::mpsc::Sender<TypesetJob>>,
    pub dropped_images: Vec<Arc<RenderImage>>,
    pub results_generation: u64,
    pub last_pump: Instant,
    pub results_scroll: ScrollHandle,
}

impl AppView {
    pub fn new(
        engine: Engine,
        pairing: Arc<Pairing>,
        wake_flag: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // The pump: collect whatever the waker flagged, 50 times a second.
        // Between flags it still ticks at 5 Hz for countdowns, but does not
        // pump the engine (or touch the picture) on every tick.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(20))
                .await;
            let flagged = wake_flag.swap(false, Ordering::Relaxed);
            if this
                .update(cx, |view, cx| view.pump_tick(flagged, cx))
                .is_err()
            {
                break;
            }
        })
        .detach();
        Self {
            engine,
            focus,
            results: Vec::new(),
            history: Vec::new(),
            notice: String::new(),
            centre: [0.5, 0.5],
            magnification: 1.0,
            image: None,
            image_key: None,
            placement: None,
            view_region: None,
            canvas: Arc::new(Mutex::new(None)),
            scale_factor: 1.0,
            drag: None,
            erase_points: Vec::new(),
            reading_open: false,
            settings_open: false,
            pair_open: false,
            reading_only: false,
            erasing: false,
            brush: 12.0,
            erase_steps: Vec::new(),
            blocks_asked: false,
            guidance_hidden: None,
            pairing,
            pair_offers: Vec::new(),
            pair_note: String::new(),
            slider_bounds: Arc::new(Mutex::new(std::collections::HashMap::new())),
            active_slider: None,
            pending_corner: None,
            shown_image: None,
            pair_rx: None,
            rebind_action: None,
            dev_phone: None,
            dev_phone_seen: None,
            zoom_open: true,
            zoom_image: None,
            zoom_shown_image: None,
            zoom_key: None,
            zoom_canvas: Arc::new(Mutex::new(None)),
            zoom_display: None,
            zoom_native: None,
            maximised: None,
            view_target: None,
            detached_preview: crate::detach::DetachedShared::new("preview"),
            detached_zoom: crate::detach::DetachedShared::new("zoom"),
            detached_reading: crate::detach::DetachedShared::new("reading"),
            freeze_flash: None,
            detach_pending: None,
            reading_width: Arc::new(Mutex::new(None)),
            typesetter: None,
            typeset_cache: std::collections::HashMap::new(),
            typeset_rx: None,
            typeset_tx: None,
            dropped_images: Vec::new(),
            results_generation: 0,
            last_pump: Instant::now(),
            results_scroll: ScrollHandle::new(),
        }
    }

    pub fn cmd(&mut self, command: Command, cx: &mut Context<Self>) {
        // A new picture (source or rotation) opens whole, not cropped to
        // the previous picture's region.
        if matches!(
            command,
            Command::UseSource { .. } | Command::SetRotation { .. }
        ) {
            self.centre = [0.5, 0.5];
            self.magnification = self.engine.config().magnifier.magnification;
        }
        match self.engine.handle(command) {
            Ok(Reply::Done) => {}
            Ok(Reply::Unchanged) => {}
            Err(e) => self.notice = format!("{e:#}"),
        }
        self.pump(cx);
    }

    /// One pump-loop tick: pumps the engine when the waker flagged (or at
    /// 5 Hz regardless, for status countdowns), and applies a queued
    /// pairing result or corner drag. Redraws only on change.
    pub fn pump_tick(&mut self, flagged: bool, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(kind) = self.detach_pending.take() {
            self.detach_pane(kind, cx);
            changed = true;
        }
        // Freeze flash: keep redrawing while it fades (~280 ms).
        if let Some(at) = self.freeze_flash {
            if at.elapsed() < Duration::from_millis(280) {
                changed = true;
            } else {
                self.freeze_flash = None;
                changed = true;
            }
        }
        // Detached windows: run what they asked for, publish snapshots.
        for shared in [
            Arc::clone(&self.detached_preview),
            Arc::clone(&self.detached_zoom),
            Arc::clone(&self.detached_reading),
        ] {
            for command in std::mem::take(&mut *shared.commands.lock().unwrap()) {
                self.cmd(command, cx);
            }
            for action in std::mem::take(&mut *shared.actions.lock().unwrap()) {
                self.detached_action(&action, cx);
            }
            if shared.is_open() {
                *shared.config.lock().unwrap() = Some(self.engine.config().clone());
            }
        }
        self.publish_detached();
        if let Some(rx) = &self.pair_rx {
            if let Ok(result) = rx.try_recv() {
                self.pair_rx = None;
                match result {
                    Ok(offers) => {
                        self.pair_offers = offers;
                        self.pair_note.clear();
                    }
                    Err(e) => self.pair_note = e,
                }
                changed = true;
            }
        }
        if let Some(phone) = &self.dev_phone {
            let cam = phone.camera();
            let seen = (cam.zoom, cam.torch);
            if self.dev_phone_seen != Some(seen) {
                self.dev_phone_seen = Some(seen);
                log::info!(
                    "dev fake phone camera: zoom={} torch={:?}",
                    cam.zoom,
                    cam.torch
                );
            }
        }
        // Glide the view toward its target (wheel/key zooms set one).
        if let Some((target_mag, target_centre)) = self.view_target {
            let ease = |cur: f32, tgt: f32| cur + (tgt - cur) * 0.35;
            self.magnification = ease(self.magnification.ln(), target_mag.ln()).exp();
            self.centre = [
                ease(self.centre[0], target_centre[0]),
                ease(self.centre[1], target_centre[1]),
            ];
            if (self.magnification - target_mag).abs() / target_mag < 0.01
                && (self.centre[0] - target_centre[0]).abs() < 0.001
                && (self.centre[1] - target_centre[1]).abs() < 0.001
            {
                self.magnification = target_mag;
                self.centre = target_centre;
                self.view_target = None;
            }
            changed |= self.refresh_image();
        }
        // Finished typeset readings.
        if let Some(rx) = &self.typeset_rx {
            while let Ok((key, result)) = rx.try_recv() {
                changed = true;
                match result {
                    Some((rgba, w, h, scale)) => {
                        let img = crate::frame::rgba_to_render_image(rgba, w, h);
                        if let Some(TypesetEntry::Ready(old, ..)) = self
                            .typeset_cache
                            .insert(key, TypesetEntry::Ready(img, scale, w, h))
                        {
                            self.dropped_images.push(old);
                        }
                    }
                    None => {
                        self.typeset_cache.insert(key, TypesetEntry::Failed);
                    }
                }
            }
        }
        if let Some((corner, to)) = self.pending_corner.take() {
            let _ = self.engine.handle(Command::MoveCorner { corner, to });
            changed = true;
        }
        if flagged || self.last_pump.elapsed() >= Duration::from_millis(200) {
            self.last_pump = Instant::now();
            self.pump(cx);
            return;
        }
        if changed {
            cx.notify();
        }
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        // Pairing sessions arrive on their own thread; show the newest and
        // close pairing, as the desktop does when a phone pairs.
        let mut changed = false;
        if let Some(session) = self.pairing.take_newest() {
            self.engine.pair(session);
            if self.pair_open {
                self.pair_open = false;
                self.pairing.stop();
            }
            changed = true;
        }
        let events = self.engine.pump(Instant::now());
        changed |= !events.is_empty();
        for event in events {
            match event {
                Event::ResultAppended(result) => self.results.push(result),
                Event::ResultsCleared => {
                    self.results.clear();
                    self.results_generation += 1;
                    for (_, entry) in self.typeset_cache.drain() {
                        if let TypesetEntry::Ready(img, ..) = entry {
                            self.dropped_images.push(img);
                        }
                    }
                }
                Event::HistoryAppended(entry) => self.history.push(entry),
                Event::Notice(notice) => self.notice = notice.text,
                _ => {}
            }
        }
        // Guidance hides only until the problem goes away; a later failure
        // of the same kind shows it again.
        if !matches!(
            self.engine.state().stream.value.status,
            StreamStatus::Waiting { .. }
        ) {
            self.guidance_hidden = None;
        }
        changed |= self.refresh_image();
        changed |= self.refresh_zoom();
        if changed {
            cx.notify();
        }
    }

    /// The Zoom pane's picture: the selection rendered as a read would see
    /// it (erased, rectified, enhanced), at native pixels up to a 1920px
    /// edge (the egui pane's cap). Reconverted only when its inputs move.
    fn refresh_zoom(&mut self) -> bool {
        if !self.zoom_open {
            return false;
        }
        let Some(selection) = self.engine.selection() else {
            let had = self.zoom_image.take().map(|i| self.dropped_images.push(i));
            self.zoom_key = None;
            return had.is_some();
        };
        let Some(frame) = self.engine.frame(squigl_engine::view::FrameRef::Shown) else {
            return false;
        };
        let enhance = self.engine.config().enhance;
        let erasures = self.engine.state().capture.value.erasures.clone();
        let step = selection.rect.w.max(selection.rect.h).div_ceil(1920).max(1);
        // Inputs hashed cheaply: frame identity by pointer, the rest by value.
        let mut hash = 0xcbf29ce484222325u64;
        let mut mix = |v: u64| hash = (hash ^ v).wrapping_mul(0x100000001b3);
        mix(Arc::as_ptr(&frame) as u64);
        mix(self.engine.capture_seq());
        for v in [
            selection.rect.x,
            selection.rect.y,
            selection.rect.w,
            selection.rect.h,
            step,
        ] {
            mix(v as u64);
        }
        if let Some(quad) = selection.quad {
            for pt in quad {
                mix(pt[0].to_bits() as u64);
                mix(pt[1].to_bits() as u64);
            }
        }
        mix(crate::frame::erasures_key(&erasures));
        mix(enhance.mode as u64);
        mix(enhance.channel as u64);
        for f in [
            enhance.strength,
            enhance.black_point,
            enhance.white_point,
            enhance.gamma,
        ] {
            mix(f.to_bits() as u64);
        }
        if self.zoom_key == Some(hash) {
            return false;
        }
        self.zoom_key = Some(hash);
        let (rgba, w, h) = squigl_engine::render::render_selection(
            &frame,
            self.engine.rotation(),
            Some(selection),
            step,
            &erasures,
            Some(&enhance),
        );
        if let Some(old) = self.zoom_image.take() {
            self.dropped_images.push(old);
        }
        self.zoom_image = Some(crate::frame::rgba_to_render_image(rgba, w, h));
        self.zoom_native = Some((w * step, h * step, step));
        true
    }

    // -- the picture -------------------------------------------------------

    fn viewport(&self) -> Option<Viewport> {
        let (_, _, w, h) = (*self.canvas.lock().unwrap())?;
        Some(Viewport {
            width: w,
            height: h,
            dpr: self.scale_factor,
            centre: self.centre,
            magnification: self.magnification,
        })
    }

    fn refresh_image(&mut self) -> bool {
        let Some(viewport) = self.viewport() else {
            return false;
        };
        let Some(view) = self.engine.state().capture.value.view else {
            return false;
        };
        let capture = &self.engine.state().capture.value;
        let seq = capture.frozen.map_or_else(
            || self.engine.shared().latest_numbered().map_or(0, |(n, _)| n),
            |f| f.seq,
        );
        let lut = self.engine.lut();
        let smooth = self.engine.config().magnifier.smooth;
        let format = if matches!(lut, Lut::Luma(_)) {
            PlaneFormat::Luma
        } else {
            PlaneFormat::Yuv420
        };
        let request = viewport.request(view, FrameRef::Shown, format);
        self.placement = Some(viewport.placement(view));
        self.view_region = Some(request.region);
        // Cheap identity check first: region/step/rotation/LUT/erasures, so
        // an unchanged picture costs no plane extraction (render_planes
        // re-samples and re-paints erasures on every call).
        // FrameKey is in source space (the planes' header), so the cheap
        // key maps the view region back the same way render_planes does.
        let src_region = request
            .region
            .to_source(self.engine.rotation(), view.0, view.1);
        let cheap = FrameKey {
            seq,
            region: (src_region.x, src_region.y, src_region.w, src_region.h),
            step: request.step,
            rotation: self.engine.rotation(),
            lut: lut_key(&lut),
            erasures: erasures_key(&capture.erasures),
            smooth,
        };
        if self.image_key == Some(cheap) {
            return false;
        }
        let Some(planes) = self.engine.render_planes(&request) else {
            return false;
        };
        let key = FrameKey::of(&planes, cheap.lut, cheap.erasures, smooth);
        if self.image_key != Some(key) {
            self.image = Some(to_render_image(&planes, &lut, smooth));
            self.image_key = Some(key);
            true
        } else {
            false
        }
    }

    /// View pixel to canvas logical px (for the overlay).
    pub fn to_canvas(&self, point: [f32; 2]) -> (f32, f32) {
        let Some(p) = self.placement else {
            return (0.0, 0.0);
        };
        let dpr = self.scale_factor.max(1.0);
        (
            (point[0] - p.origin[0]) * p.scale / dpr,
            (point[1] - p.origin[1]) * p.scale / dpr,
        )
    }

    // -- actions -----------------------------------------------------------

    fn frozen(&self) -> bool {
        self.engine.state().capture.value.frozen.is_some()
    }

    pub fn toggle_freeze(&mut self, cx: &mut Context<Self>) {
        let freezing = !self.frozen();
        if freezing {
            self.freeze_flash = Some(Instant::now());
        }
        self.cmd(
            if freezing {
                Command::Freeze
            } else {
                Command::Live
            },
            cx,
        );
    }

    pub fn rotate(&mut self, cw: bool, cx: &mut Context<Self>) {
        let rotation = self.engine.rotation();
        self.cmd(
            Command::SetRotation {
                rotation: if cw {
                    rotation.turned_cw()
                } else {
                    rotation.turned_ccw()
                },
            },
            cx,
        );
    }

    pub fn zoom_view(&mut self, steps: f32, cx: &mut Context<Self>) {
        let max = self.engine.config().magnifier.max_magnification;
        let (mag, centre) = self
            .view_target
            .unwrap_or((self.magnification, self.centre));
        self.view_target = Some(((mag * ZOOM_FACTOR.powf(steps)).clamp(1.0, max), centre));
        cx.notify();
    }

    pub fn pan(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let (mag, centre) = self
            .view_target
            .unwrap_or((self.magnification, self.centre));
        let shown = 1.0 / mag;
        let clamp = |c: f32| c.clamp(shown / 2.0, 1.0 - shown / 2.0);
        self.view_target = Some((
            mag,
            [clamp(centre[0] + dx * shown), clamp(centre[1] + dy * shown)],
        ));
        cx.notify();
    }

    pub fn cycle_mode(&mut self, cx: &mut Context<Self>) {
        let mut config = self.engine.config().clone();
        // Custom is reached in Settings, not by cycling (the desktop's
        // QUICK_MODES does the same).
        let modes: Vec<DisplayMode> = DisplayMode::ALL
            .into_iter()
            .filter(|m| *m != DisplayMode::Custom)
            .collect();
        let at = modes
            .iter()
            .position(|m| *m == config.display.mode)
            .unwrap_or(0);
        config.display.mode = modes[(at + 1) % modes.len()];
        self.cmd(
            Command::SetConfig {
                config: Box::new(config),
            },
            cx,
        );
    }

    pub fn camera_zoom_to(&mut self, pos: f32, cx: &mut Context<Self>) {
        let Some(bounds) = zoom_bounds(
            self.engine.state().stream.value.zoom_range,
            self.engine.state().stream.value.source.clone(),
        ) else {
            return;
        };
        let zoom = from_slider(pos, bounds);
        self.cmd(Command::SetZoom { zoom }, cx);
    }

    pub fn display_slider(
        &mut self,
        kind: &str,
        value: f32,
        released: bool,
        cx: &mut Context<Self>,
    ) {
        let mut config = self.engine.config().clone();
        match kind {
            "contrast" => config.display.contrast = value,
            "brightness" => config.display.brightness = value,
            _ => config.display.gamma = value,
        }
        self.cmd(
            if released {
                Command::SetConfig {
                    config: Box::new(config),
                }
            } else {
                Command::PreviewConfig {
                    config: Box::new(config),
                }
            },
            cx,
        );
    }

    pub fn open_image_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open image".into()),
        });
        let _ = window;
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |view, cx| {
                        view.cmd(
                            Command::UseSource {
                                source: SourceSpec::Image(path),
                            },
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
    }

    pub fn open_replay_dialog(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Play recording".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |view, cx| {
                        view.cmd(
                            Command::UseSource {
                                source: SourceSpec::Replay(path),
                            },
                            cx,
                        );
                    });
                }
            }
        })
        .detach();
    }

    pub fn erase_box(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.engine.state().capture.value.selection else {
            self.notice = "Draw a box first".into();
            return;
        };
        let mut strokes = self.engine.state().capture.value.erasures.clone();
        self.erase_steps.push(strokes.len());
        strokes.extend(cover_strokes(selection));
        self.cmd(Command::SetErasures { strokes }, cx);
    }

    pub fn undo_erase(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.erase_steps.pop() else {
            self.notice = "Nothing to undo".into();
            return;
        };
        let mut strokes = self.engine.state().capture.value.erasures.clone();
        strokes.truncate(at);
        self.cmd(Command::SetErasures { strokes }, cx);
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        let dir = squigl_engine::paths::pictures_dir();
        match self.engine.handle(Command::Save { dir }) {
            Ok(_) => self.notice = "Saved the picture".into(),
            Err(e) => self.notice = format!("Could not save: {e:#}"),
        }
        self.pump(cx);
    }

    pub fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A rebind in Settings captures the next key instead of acting on it.
        if let Some(action) = self.rebind_action.take() {
            let key = key_name(event);
            if !key.is_empty() && key != "Escape" {
                self.apply_rebind(&action, &key, cx);
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // While a panel is open, shortcuts (other than closing it) are the
        // panel's, not the picture's (the desktop suppresses them too).
        if self.pair_open || self.settings_open {
            if key_name(event) == "Escape" {
                self.pair_open = false;
                self.settings_open = false;
                self.pairing.stop();
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        let Some(action) = action_for(event, self.engine.config()) else {
            return;
        };
        self.run_action(&action, window, cx);
        cx.stop_propagation();
    }

    /// Gives `action` the key `key` in `[desktop].shortcuts` (the desktop's
    /// rebind: the key leaves any other action; back at default is not stored).
    pub fn apply_rebind(&mut self, action: &str, key: &str, cx: &mut Context<Self>) {
        let mut config = self.engine.config().clone();
        let mut next = std::collections::BTreeMap::new();
        for (a, default) in ACTIONS {
            let current = config
                .desktop
                .shortcuts
                .get(a)
                .map_or(default, String::as_str);
            let k = if a == action {
                key
            } else if current == key {
                ""
            } else {
                current
            };
            if k != default && !k.is_empty() {
                next.insert(a.to_string(), k.to_string());
            } else if k.is_empty() {
                next.insert(a.to_string(), String::new());
            }
        }
        config.desktop.shortcuts = next;
        self.cmd(
            Command::SetConfig {
                config: Box::new(config),
            },
            cx,
        );
    }

    pub fn focus_clone(&self) -> FocusHandle {
        self.focus.clone()
    }
    pub fn set_start_magnification(&mut self, m: f32) {
        self.magnification = m.clamp(1.0, 30.0);
    }
    pub fn canvas_slot(&self) -> CanvasBounds {
        Arc::clone(&self.canvas)
    }
    pub fn stream_torch(&self) -> bool {
        self.engine.state().stream.value.torch
    }
    pub fn notice_text(&self) -> String {
        self.notice.clone()
    }
    pub fn hide_guidance(&mut self, cx: &mut Context<Self>) {
        if let StreamStatus::Waiting { problem, .. } = &self.engine.state().stream.value.status {
            self.guidance_hidden = *problem;
        }
        cx.notify();
    }
    pub fn toggle_pair(&mut self, cx: &mut Context<Self>) {
        self.pair_open = !self.pair_open;
        if self.pair_open {
            // Starting can issue a Tailscale certificate (seconds); do it
            // off the window thread and collect the offers in the pump.
            let pairing = Arc::clone(&self.pairing);
            let tailscale = self.engine.config().desktop.tailscale_https;
            let (tx, rx) = std::sync::mpsc::channel();
            self.pair_rx = Some(rx);
            self.pair_note = "Starting pairing...".into();
            std::thread::spawn(move || {
                let _ = tx.send(pairing.start(tailscale));
            });
        } else {
            self.pair_rx = None;
            self.pairing.stop();
        }
        cx.notify();
    }
    pub fn toggle_zoom_pane(&mut self, cx: &mut Context<Self>) {
        self.zoom_open = !self.zoom_open;
        if !self.zoom_open && self.maximised == Some("zoom") {
            self.maximised = None;
        }
        cx.notify();
    }
    pub fn toggle_maximise(&mut self, pane: &'static str, cx: &mut Context<Self>) {
        self.maximised = if self.maximised == Some(pane) {
            None
        } else {
            Some(pane)
        };
        cx.notify();
    }
    pub fn toggle_reading(&mut self, cx: &mut Context<Self>) {
        self.reading_open = !self.reading_open;
        cx.notify();
    }
    pub fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        cx.notify();
    }
    /// The drawn image's rectangle in canvas logical px: region by placement.
    pub fn image_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let p = self.placement?;
        let r = self.view_region?;
        let dpr = self.scale_factor.max(1.0);
        Some((
            (r.x as f32 - p.origin[0]) * p.scale / dpr,
            (r.y as f32 - p.origin[1]) * p.scale / dpr,
            r.w as f32 * p.scale / dpr,
            r.h as f32 * p.scale / dpr,
        ))
    }

    fn run_action(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            "save" => self.save(cx),
            "freeze" => self.toggle_freeze(cx),
            "rotate-cw" => self.rotate(true, cx),
            "rotate-ccw" => self.rotate(false, cx),
            "zoom-in" => self.zoom_view(1.0, cx),
            "zoom-out" => self.zoom_view(-1.0, cx),
            "zoom-reset" => {
                self.view_target = Some((1.0, [0.5, 0.5]));
                cx.notify();
            }
            "pan-left" => self.pan(-0.2, 0.0, cx),
            "pan-right" => self.pan(0.2, 0.0, cx),
            "pan-up" => self.pan(0.0, -0.2, cx),
            "pan-down" => self.pan(0.0, 0.2, cx),
            "next-mode" => self.cycle_mode(cx),
            "reading-line" => {
                let mut c = self.engine.config().clone();
                c.magnifier.reading_line = !c.magnifier.reading_line;
                self.cmd(
                    Command::SetConfig {
                        config: Box::new(c),
                    },
                    cx,
                );
            }
            "camera-zoom-in" => self.cmd(Command::StepZoom { steps: 4 }, cx),
            "camera-zoom-out" => self.cmd(Command::StepZoom { steps: -4 }, cx),
            "torch" => {
                let on = self.engine.state().stream.value.torch;
                self.cmd(Command::SetTorch { on: !on }, cx);
            }
            "reading-pane" => {
                self.reading_open = !self.reading_open;
                cx.notify();
            }
            "read" => self.cmd(Command::Read, cx),
            "read-all" => {
                self.reading_open = true;
                self.cmd(Command::ReadAll, cx);
            }
            "second-opinion" => self.cmd(Command::SecondOpinion, cx),
            "block-mode" => {
                let on = self.engine.state().blocks.value.enabled;
                if !on {
                    self.blocks_asked = true;
                }
                self.cmd(Command::SetBlockMode { on: !on }, cx);
            }
            "next-block" => self.cmd(Command::StepBlock { delta: 1 }, cx),
            "previous-block" => self.cmd(Command::StepBlock { delta: -1 }, cx),
            "clear-selection" => {
                if self.reading_only {
                    // Escape first leaves the readings-only view (and its
                    // full screen), as in the desktop.
                    self.reading_only = false;
                    window.toggle_fullscreen();
                    cx.notify();
                } else if self.erasing {
                    self.erasing = false;
                    cx.notify();
                } else {
                    self.cmd(Command::SetSelection { selection: None }, cx);
                }
            }
            "erase-brush" => {
                self.erasing = !self.erasing;
                cx.notify();
            }
            "erase-box" => self.erase_box(cx),
            "undo-erase" => self.undo_erase(cx),
            "reading-only" => {
                self.reading_only = !self.reading_only;
                self.reading_open = true;
                window.toggle_fullscreen();
                cx.notify();
            }
            "speak" => self.cmd(Command::Speak { result: None }, cx),
            "read-aloud" => {
                self.reading_open = true;
                self.cmd(Command::ReadAloud, cx);
            }
            "pause-speech" => {
                if self.engine.state().speech.value.paused {
                    self.cmd(Command::ResumeSpeaking, cx);
                } else {
                    self.cmd(Command::PauseSpeaking, cx);
                }
            }
            "stop-speech" => self.cmd(Command::StopSpeaking, cx),
            "next-spoken" => self.cmd(Command::SkipSpeech { delta: 1 }, cx),
            "previous-spoken" => self.cmd(Command::SkipSpeech { delta: -1 }, cx),
            "fullscreen" => window.toggle_fullscreen(),
            "settings" => {
                self.settings_open = !self.settings_open;
                cx.notify();
            }
            "zoom-pane" => {
                self.zoom_open = !self.zoom_open;
                if !self.zoom_open && self.maximised == Some("zoom") {
                    self.maximised = None;
                }
                cx.notify();
            }
            _ => {}
        }
    }
}

// The camera zoom slider's log scale (the desktop's camera.ts).
pub fn zoom_bounds(range: Option<(f32, f32)>, source: SourceSpec) -> Option<(f32, f32)> {
    let (mut lo, hi) = range?;
    if matches!(source, SourceSpec::Phone) {
        lo = lo.max(1.0);
    }
    (hi > lo).then_some((lo, hi))
}

/// The slider position for a zoom ratio. Kept with `from_slider` (and tested
/// with it) for when a slider's thumb can be moved from outside again;
/// gpui-base's SliderState cannot be set without a window handle this
/// version does not give entity updates.
#[allow(dead_code)]
pub fn to_slider(zoom: f32, (lo, hi): (f32, f32)) -> f32 {
    let at = (zoom.ln() - lo.ln()) / (hi.ln() - lo.ln());
    (at * 100.0).clamp(0.0, 100.0)
}

pub fn from_slider(pos: f32, (lo, hi): (f32, f32)) -> f32 {
    let zoom = (lo.ln() + (pos / 100.0) * (hi.ln() - lo.ln())).exp();
    ((zoom * 100.0).round() / 100.0).clamp(lo, hi)
}

pub const ACTIONS: [(&str, &str); 37] = [
    ("freeze", " "),
    ("rotate-cw", "r"),
    ("rotate-ccw", "R"),
    ("zoom-in", "+"),
    ("zoom-out", "-"),
    ("zoom-reset", "0"),
    ("pan-left", "ArrowLeft"),
    ("pan-right", "ArrowRight"),
    ("pan-up", "ArrowUp"),
    ("pan-down", "ArrowDown"),
    ("next-mode", "m"),
    ("reading-line", "l"),
    ("camera-zoom-in", "]"),
    ("camera-zoom-out", "["),
    ("torch", "t"),
    ("reading-pane", "p"),
    ("read", "Enter"),
    ("read-all", "a"),
    ("second-opinion", "o"),
    ("block-mode", "b"),
    ("next-block", "n"),
    ("previous-block", "N"),
    ("clear-selection", "Escape"),
    ("erase-brush", "e"),
    ("erase-box", "x"),
    ("undo-erase", "u"),
    ("reading-only", "F"),
    ("speak", "s"),
    ("read-aloud", "A"),
    ("pause-speech", "."),
    ("stop-speech", "S"),
    ("next-spoken", ">"),
    ("previous-spoken", "<"),
    ("fullscreen", "f"),
    ("settings", ","),
    ("zoom-pane", "z"),
    ("save", "S"),
];

/// The key an action has now (after `[desktop].shortcuts` overrides).
pub fn action_key(config: &Config, action: &str) -> String {
    if let Some(k) = config.desktop.shortcuts.get(action) {
        if !k.is_empty() {
            return k.clone();
        }
    }
    ACTIONS
        .iter()
        .find(|(a, _)| *a == action)
        .map(|(_, k)| k.to_string())
        .unwrap_or_default()
}

/// One reading pane result, laid out for display and copying.
pub struct ResultDetail {
    pub label: Option<String>,
    pub backend: Option<String>,
    pub elapsed_secs: f32,
    pub readings: Vec<(String, u32, bool)>,
    pub reading_objs: Vec<squigl_engine::transcribe::Reading>,
    pub silent: u32,
    pub samples: u32,
    pub error: Option<String>,
}

impl ResultDetail {
    pub fn copy_text(&self) -> String {
        match &self.error {
            Some(e) => e.clone(),
            None => self
                .readings
                .iter()
                .map(|(t, _, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(
                    "
",
                ),
        }
    }
}

/// The key an engine event used, in the desktop's `KeyboardEvent.key` terms.
pub fn key_name(event: &KeyDownEvent) -> String {
    let ks = &event.keystroke;
    // GPUI on Linux names the arrows "left"/"right"/"up"/"down" (its XKB
    // mapping), not the web's "ArrowLeft" the shortcut table uses.
    let mut key = match ks.key.as_str() {
        "space" => " ".to_string(),
        "enter" => "Enter".to_string(),
        "escape" => "Escape".to_string(),
        "left" | "arrowleft" => "ArrowLeft".to_string(),
        "right" | "arrowright" => "ArrowRight".to_string(),
        "up" | "arrowup" => "ArrowUp".to_string(),
        "down" | "arrowdown" => "ArrowDown".to_string(),
        "equal" => "+".to_string(),
        "minus" => "-".to_string(),
        "comma" => ",".to_string(),
        "period" => ".".to_string(),
        other => other.to_string(),
    };
    if ks.modifiers.shift {
        // Shifted punctuation is its own key (">" is Shift+"."), as a
        // browser reports it; a shifted letter is the capital.
        key = match key.as_str() {
            "." => ">".to_string(),
            "," => "<".to_string(),
            _ if key.chars().count() == 1 => key.to_uppercase(),
            _ => key,
        };
    }
    if ks.modifiers.control && key.eq_ignore_ascii_case("s") {
        return "ctrl+s".to_string();
    }
    if ks.modifiers.control && key.eq_ignore_ascii_case("z") {
        return "ctrl+z".to_string();
    }
    key
}

pub fn action_for(event: &KeyDownEvent, config: &Config) -> Option<String> {
    // A modified key is not a plain shortcut (Ctrl+F must not toggle full
    // screen); Ctrl+S is the one deliberate exception, handled below. The
    // desktop returns early on ctrl/alt/meta the same way.
    let m = &event.keystroke.modifiers;
    let named = key_name(event);
    if (m.control || m.alt || m.platform) && named != "ctrl+s" && named != "ctrl+z" {
        return None;
    }
    let key = named;
    if key == "ctrl+s" {
        return Some("save".to_string());
    }
    if key == "ctrl+z" {
        return Some("undo-erase".to_string());
    }
    let desktop = &config.desktop;
    for (action, default) in ACTIONS {
        let bound = desktop
            .shortcuts
            .get(action)
            .map_or(default, String::as_str);
        if bound.is_empty() {
            continue;
        }
        if !desktop.single_key_shortcuts && bound.chars().count() == 1 {
            continue;
        }
        if bound == key {
            return Some(action.to_string());
        }
    }
    None
}

pub fn mode_label(mode: DisplayMode) -> &'static str {
    match mode {
        DisplayMode::Normal => "Normal colours",
        DisplayMode::Grey => "Black on white",
        DisplayMode::Inverted => "Inverted colours",
        DisplayMode::YellowOnBlack => "Yellow on black",
        DisplayMode::WhiteOnBlack => "White on black",
        DisplayMode::BlackOnYellow => "Black on yellow",
        DisplayMode::Custom => "My own colours",
    }
}

pub fn theme_colours(theme: Theme) -> (u32, u32, u32) {
    // (background, text, edge)
    match theme {
        Theme::Dark => (0x1a1a1e, 0xe8e8e8, 0x44444c),
        Theme::Light => (0xf2f2f0, 0x1a1a1a, 0xbbbbbb),
        Theme::HighContrastYellow => (0x000000, 0xffff00, 0xffff00),
        Theme::HighContrastWhite => (0x000000, 0xffffff, 0xffffff),
    }
}

pub fn role_colour(role: Role) -> u32 {
    match role {
        Role::Text => 0xff5a28,
        Role::Formula => 0xc85aff,
        Role::Figure => 0x3cbfff,
        Role::Other => 0xaaaaaa,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragKind {
    Draw,
    Move,
    Corner(usize),
    Erase,
    Pan,
}

pub enum FinishedDrag {
    Draw {
        from: (usize, usize),
        to: (usize, usize),
    },
    Move {
        shown: Crop,
    },
    Erase,
    Other,
}

impl AppView {
    pub fn placement(&self) -> Option<Placement> {
        self.placement
    }
    pub fn canvas_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        *self.canvas.lock().unwrap()
    }
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }
    pub fn view_size(&self) -> Option<(usize, usize)> {
        self.engine.state().capture.value.view
    }
    pub fn canvas_pos(&self, pos: Point<Pixels>) -> (f32, f32) {
        let (ox, oy) = self
            .canvas
            .lock()
            .unwrap()
            .map_or((0.0, 0.0), |(x, y, _, _)| (x, y));
        (pos.x.as_f32() - ox, pos.y.as_f32() - oy)
    }
    pub fn is_erasing(&self) -> bool {
        self.erasing
    }
    pub fn selection(&self) -> Option<Selection> {
        self.engine.state().capture.value.selection
    }
    pub fn erasures(&self) -> Vec<Stroke> {
        self.engine.state().capture.value.erasures.clone()
    }
    pub fn push_erase_step(&mut self, at: usize) {
        self.erase_steps.push(at);
    }
    pub fn set_drag_erase(&mut self, points: Vec<[f32; 2]>) {
        self.drag = Some(Drag::Erase);
        self.erase_points = points;
    }
    pub fn set_drag_pan(&mut self, _pt: [f32; 2], pos: Point<Pixels>) {
        let (x, y) = self.canvas_pos(pos);
        self.drag = Some(Drag::Pan {
            from: [x, y],
            centre: self.centre,
        });
    }
    pub fn set_drag_corner(&mut self, i: usize) {
        self.drag = Some(Drag::Corner(i));
    }
    pub fn set_drag_move(&mut self, from: (usize, usize), box_at: Crop) {
        self.drag = Some(Drag::Move {
            from,
            box_at,
            shown: box_at,
        });
    }
    pub fn set_drag_draw(&mut self, from: (usize, usize)) {
        self.drag = Some(Drag::Draw { from, to: from });
    }
    pub fn drag_kind(&self) -> Option<DragKind> {
        self.drag.map(|d| match d {
            Drag::Draw { .. } => DragKind::Draw,
            Drag::Move { .. } => DragKind::Move,
            Drag::Corner(i) => DragKind::Corner(i),
            Drag::Erase => DragKind::Erase,
            Drag::Pan { .. } => DragKind::Pan,
        })
    }
    pub fn extend_erase(&mut self, pt: [f32; 2]) {
        let radius =
            self.brush * self.scale_factor.max(1.0) / self.placement.map_or(1.0, |p| p.scale);
        if self
            .erase_points
            .last()
            .is_none_or(|last| (last[0] - pt[0]).hypot(last[1] - pt[1]) > radius / 2.0)
        {
            self.erase_points.push(pt);
        }
    }
    pub fn update_draw(&mut self, to: (usize, usize)) {
        if let Some(Drag::Draw { to: t, .. }) = &mut self.drag {
            *t = to;
        }
    }
    pub fn update_move(&mut self, to: (usize, usize)) {
        let view = self.view_size();
        if let Some(Drag::Move {
            from,
            box_at,
            shown,
        }) = &mut self.drag
        {
            if let Some(view) = view {
                *shown = box_at.moved(
                    to.0 as i64 - from.0 as i64,
                    to.1 as i64 - from.1 as i64,
                    view.0,
                    view.1,
                );
            }
        }
    }
    pub fn update_pan(&mut self, pos: Point<Pixels>) {
        if let Some(Drag::Pan { from, centre }) = self.drag {
            let (x, y) = self.canvas_pos(pos);
            let (_, _, w, h) = self.canvas.lock().unwrap().unwrap_or((0.0, 0.0, 1.0, 1.0));
            let shown = 1.0 / self.magnification;
            let clamp = |c: f32| c.clamp(shown / 2.0, 1.0 - shown / 2.0);
            self.centre = [
                clamp(centre[0] - (x - from[0]) / w.max(1.0) * shown),
                clamp(centre[1] - (y - from[1]) / h.max(1.0) * shown),
            ];
            self.view_target = None;
            self.refresh_image();
        }
    }
    pub fn take_drag(&mut self) -> Option<FinishedDrag> {
        match self.drag.take() {
            Some(Drag::Draw { from, to }) => Some(FinishedDrag::Draw { from, to }),
            Some(Drag::Move { shown, .. }) => Some(FinishedDrag::Move { shown }),
            Some(Drag::Erase) => Some(FinishedDrag::Erase),
            Some(_) => Some(FinishedDrag::Other),
            None => None,
        }
    }
    pub fn take_erase_stroke(&mut self) -> Option<Stroke> {
        if self.erase_points.is_empty() {
            return None;
        }
        let radius =
            self.brush * self.scale_factor.max(1.0) / self.placement.map_or(1.0, |p| p.scale);
        Some(Stroke {
            points: std::mem::take(&mut self.erase_points),
            radius,
        })
    }
    pub fn block_at(&self, point: (usize, usize)) -> Option<usize> {
        // The smallest block under the point: nested blocks (a formula in a
        // text block) select the inner one the person clicked.
        self.engine
            .state()
            .blocks
            .value
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.rect.contains(point.0, point.1))
            .min_by_key(|(_, b)| b.rect.area())
            .map(|(i, _)| i)
    }
    pub fn pan_by(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        self.pan(dx, dy, cx);
    }
    pub fn toggle_erasing(&mut self, cx: &mut Context<Self>) {
        self.erasing = !self.erasing;
        cx.notify();
    }
    pub fn brush_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.brush = (self.brush + delta).clamp(4.0, 80.0);
        cx.notify();
    }
    pub fn brush_radius(&self) -> f32 {
        self.brush
    }
    pub fn engine_reading(&self) -> squigl_engine::engine::ReadingSlice {
        self.engine.state().reading.value.clone()
    }
    pub fn engine_blocks(&self) -> squigl_engine::engine::BlocksSlice {
        self.engine.state().blocks.value.clone()
    }
    pub fn engine_models(&self) -> Vec<squigl_engine::engine::ModelStatus> {
        self.engine.state().models.value.models.clone()
    }
    pub fn engine_speech(&self) -> squigl_engine::engine::SpeechSlice {
        self.engine.state().speech.value.clone()
    }
    pub fn engine_voices(&self) -> Vec<squigl_engine::speech::VoiceInfo> {
        self.engine.state().voices.value.voices.clone()
    }
    pub fn config_clone(&self) -> Config {
        self.engine.config().clone()
    }
    pub fn results_scroll_handle(&self) -> ScrollHandle {
        self.results_scroll.clone()
    }
    pub fn history_len(&self) -> usize {
        self.history.len()
    }
    pub fn speaking_result(&self) -> Option<usize> {
        self.engine
            .state()
            .speech
            .value
            .speaking
            .as_ref()
            .map(|s| s.result)
    }
    pub fn start_rebind(&mut self, action: String, cx: &mut Context<Self>) {
        self.rebind_action = Some(action);
        cx.notify();
    }
    pub fn rebind_pending(&self) -> Option<String> {
        self.rebind_action.clone()
    }
    pub fn results_len(&self) -> usize {
        self.results.len()
    }
    pub fn result_detail(&self, index: usize) -> Option<ResultDetail> {
        let r = self.results.get(index)?;
        Some(match &r.result {
            Ok(t) => ResultDetail {
                label: r.label.clone(),
                backend: Some(t.backend.clone()),
                elapsed_secs: t.elapsed.as_secs_f32(),
                readings: t
                    .readings
                    .iter()
                    .map(|x| (x.text.clone(), x.count, x.truncated))
                    .collect(),
                reading_objs: t.readings.clone(),
                silent: t.silent,
                samples: t.samples,
                error: None,
            },
            Err(e) => ResultDetail {
                label: r.label.clone(),
                backend: None,
                elapsed_secs: 0.0,
                readings: Vec::new(),
                reading_objs: Vec::new(),
                silent: 0,
                samples: 0,
                error: Some(e.clone()),
            },
        })
    }
    pub fn all_results_text(&self) -> String {
        (0..self.results.len())
            .filter_map(|i| self.result_detail(i))
            .map(|d| d.copy_text())
            .collect::<Vec<_>>()
            .join(
                "

",
            )
    }
    pub fn history_entries(&self) -> Vec<String> {
        self.history
            .iter()
            .map(|e| {
                let text = match &e.result {
                    Ok(t) => t
                        .readings
                        .first()
                        .map_or("(no reading)".to_string(), |r| r.text.clone()),
                    Err(err) => err.clone(),
                };
                format!("{} {}: {}", e.at.format("%H:%M"), e.what, text)
            })
            .collect()
    }
    pub fn reading_size_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        let mut c = self.engine.config().clone();
        c.ui.reading_size = (c.ui.reading_size + delta).clamp(10.0, 60.0);
        self.cmd(
            Command::SetConfig {
                config: Box::new(c),
            },
            cx,
        );
    }
    pub fn speech_voice_label(&self) -> String {
        match &self.engine.config().speech.voice {
            Some(id) => self
                .engine_voices()
                .iter()
                .find(|v| &v.id == id)
                .map_or_else(|| id.clone(), |v| v.name.clone()),
            None => "System default".to_string(),
        }
    }
    pub fn cycle_voice(&mut self, cx: &mut Context<Self>) {
        let voices = self.engine_voices();
        if voices.is_empty() {
            return;
        }
        let mut c = self.engine.config().clone();
        let at = c
            .speech
            .voice
            .as_ref()
            .and_then(|id| voices.iter().position(|v| &v.id == id));
        c.speech.voice = match at {
            None => Some(voices[0].id.clone()),
            Some(i) if i + 1 < voices.len() => Some(voices[i + 1].id.clone()),
            Some(_) => None,
        };
        self.cmd(
            Command::SetConfig {
                config: Box::new(c),
            },
            cx,
        );
    }
    pub fn pair_offers(&self) -> Vec<crate::pairing::Offer> {
        self.pair_offers
            .iter()
            .map(|o| crate::pairing::Offer {
                network: o.network.clone(),
                overlay: o.overlay,
                url: o.url.clone(),
                qr_svg: o.qr_svg.clone(),
                trusted: o.trusted,
            })
            .collect()
    }
    pub fn pair_note_text(&self) -> String {
        self.pair_note.clone()
    }
    pub fn pair_offers_empty(&self) -> bool {
        self.pair_offers.is_empty()
    }
}

/// The desktop's `coverStrokes` (lib/erase.ts): rows no taller than 12 view
/// pixels across the selection's quad (or its rect), each row's stroke
/// reaching the row's corners and spilling by at most a fifth of a row.
pub fn cover_strokes(selection: Selection) -> Vec<Stroke> {
    let poly: Vec<[f32; 2]> = selection
        .quad
        .unwrap_or_else(|| squigl_engine::layout::rect_quad(selection.rect))
        .to_vec();
    let across = |y: f32| -> Option<(f32, f32)> {
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            if (a[1] <= y && y <= b[1]) || (b[1] <= y && y <= a[1]) {
                let xs = if a[1] == b[1] {
                    vec![a[0], b[0]]
                } else {
                    vec![a[0] + ((y - a[1]) / (b[1] - a[1])) * (b[0] - a[0])]
                };
                for x in xs {
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
        }
        (lo <= hi).then_some((lo, hi))
    };
    let top = poly.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let bottom = poly.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    if bottom <= top {
        return Vec::new();
    }
    let rows = ((bottom - top) / 12.0).ceil().max(1.0);
    let step = (bottom - top) / rows;
    let mut strokes = Vec::new();
    for k in 0..rows as usize {
        let y0 = top + k as f32 * step;
        let y = y0 + step / 2.0;
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for at in [y0 + 1e-3, y, y0 + step - 1e-3] {
            if let Some((l, h)) = across(at) {
                lo = lo.min(l);
                hi = hi.max(h);
            }
        }
        if hi <= lo {
            continue;
        }
        if hi - lo <= step {
            strokes.push(Stroke {
                points: vec![[(lo + hi) / 2.0, y]],
                radius: ((hi - lo) / 2.0).hypot(step / 2.0),
            });
        } else {
            strokes.push(Stroke::line(
                [lo + step / 2.0, y],
                [hi - step / 2.0, y],
                step / std::f32::consts::SQRT_2,
            ));
        }
    }
    strokes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliderKind {
    Zoom,
    Contrast,
    Brightness,
    Gamma,
    Strength,
    BlackPoint,
    WhitePoint,
    EnhGamma,
}

impl SliderKind {
    pub fn name(self) -> &'static str {
        match self {
            SliderKind::Zoom => "zoom",
            SliderKind::Contrast => "contrast",
            SliderKind::Brightness => "brightness",
            SliderKind::Gamma => "gamma",
            SliderKind::Strength => "strength",
            SliderKind::BlackPoint => "black",
            SliderKind::WhitePoint => "white",
            SliderKind::EnhGamma => "enh-gamma",
        }
    }
    /// (min, max) of the value, matching the desktop's Settings ranges.
    pub fn range(self) -> (f32, f32) {
        match self {
            SliderKind::Zoom => (0.0, 100.0),
            SliderKind::Contrast => (0.5, 4.0),
            SliderKind::Brightness => (-0.5, 0.5),
            SliderKind::Gamma => (0.3, 3.0),
            SliderKind::Strength => (0.0, 1.0),
            SliderKind::BlackPoint => (0.0, 100.0),
            SliderKind::WhitePoint => (0.0, 100.0),
            SliderKind::EnhGamma => (0.3, 3.0),
        }
    }
}

impl AppView {
    /// A custom slider's pointer: fraction from its tracked track bounds.
    pub fn slider_fraction(&self, kind: SliderKind, pos: Point<Pixels>) -> Option<f32> {
        let (x, _, w, _) = *self.slider_bounds.lock().unwrap().get(kind.name())?;
        Some(((pos.x.as_f32() - x) / w.max(1.0)).clamp(0.0, 1.0))
    }
    pub fn slider_bounds_handle(&self) -> SliderBounds {
        Arc::clone(&self.slider_bounds)
    }
    pub fn clear_drag_kind(&mut self) {
        self.drag = None;
    }
    pub fn drag_is_erase(&self) -> bool {
        matches!(self.drag, Some(Drag::Erase))
    }
    pub fn erase_points_view(&self) -> Vec<[f32; 2]> {
        self.erase_points.clone()
    }

    pub fn slider_apply(
        &mut self,
        kind: SliderKind,
        fraction: f32,
        released: bool,
        cx: &mut Context<Self>,
    ) {
        let (lo, hi) = kind.range();
        let value = lo + fraction * (hi - lo);
        match kind {
            SliderKind::Zoom => self.camera_zoom_to(value, cx),
            SliderKind::Contrast => self.display_slider("contrast", value, released, cx),
            SliderKind::Brightness => self.display_slider("brightness", value, released, cx),
            SliderKind::Gamma => self.display_slider("gamma", value, released, cx),
            SliderKind::Strength
            | SliderKind::BlackPoint
            | SliderKind::WhitePoint
            | SliderKind::EnhGamma => {
                let mut config = self.engine.config().clone();
                match kind {
                    SliderKind::Strength => config.enhance.strength = value,
                    SliderKind::BlackPoint => config.enhance.black_point = value,
                    SliderKind::WhitePoint => config.enhance.white_point = value,
                    _ => config.enhance.gamma = value,
                }
                self.cmd(
                    if released {
                        Command::SetConfig {
                            config: Box::new(config),
                        }
                    } else {
                        Command::PreviewConfig {
                            config: Box::new(config),
                        }
                    },
                    cx,
                );
            }
        }
    }

    /// The in-progress gesture for the overlay: a box being drawn or moved.
    pub fn drag_box(&self) -> Option<Crop> {
        match self.drag {
            Some(Drag::Draw { from, to }) => Some(Crop::from_corners(from, to)),
            Some(Drag::Move { shown, .. }) => Some(shown),
            _ => None,
        }
    }

    /// The selection of the result being read aloud, for its outline.
    pub fn spoken_selection(&self) -> Option<Selection> {
        let speaking = self.engine.state().speech.value.speaking.as_ref()?;
        self.results.get(speaking.result)?.selection
    }
}

impl Drop for AppView {
    fn drop(&mut self) {
        // Shut the phone session and pairing server down cleanly rather
        // than relying on the worker's own drop alone.
        self.engine.stop();
        self.pairing.stop();
    }
}

impl AppView {
    /// The typeset image for result `index` reading `reading_idx`, starting
    /// its off-thread render on first ask. `None` while pending/failed/absent
    /// (the pane then shows styled text).
    pub fn typeset_image(
        &mut self,
        index: usize,
        reading_idx: usize,
    ) -> Option<(Arc<RenderImage>, f32, usize, usize)> {
        let typesetter = self.typesetter.clone()?;
        let result = self.results.get(index)?;
        let Ok(transcription) = &result.result else {
            return None;
        };
        let reading = transcription.readings.get(reading_idx)?.clone();
        let config = self.engine.config().clone();
        let (fg, _, _) = theme_colours(config.desktop.theme);
        let rgb = [
            ((fg >> 16) & 0xff) as u8,
            ((fg >> 8) & 0xff) as u8,
            (fg & 0xff) as u8,
        ];
        let size_pt = config.ui.reading_size;
        let width_pt = self
            .reading_width
            .lock()
            .unwrap()
            .map_or(360.0, |w: f32| (w - 28.0).clamp(180.0, 1400.0));
        let scale = self.scale_factor.max(1.0);
        let key = format!(
            "{}:{}:{}:{:?}:{}",
            self.results_generation,
            index,
            reading_idx,
            (width_pt * 10.0) as i32,
            (size_pt * 10.0) as i32
        ) + &reading.text.chars().take(24).collect::<String>();
        match self.typeset_cache.get(&key) {
            Some(TypesetEntry::Ready(img, s2, w, h)) => Some((img.clone(), *s2, *w, *h)),
            Some(_) => None,
            None => {
                self.typeset_cache
                    .insert(key.clone(), TypesetEntry::Pending);
                if self.typeset_tx.is_none() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.typeset_tx = Some(tx);
                    self.typeset_rx = Some(rx);
                }
                let tx = self.typeset_tx.clone().expect("just made");
                std::thread::spawn(move || {
                    let ui_cfg = config.ui.clone();
                    let (text, spans) =
                        squigl_engine::typeset::typeset_source(&reading, &ui_cfg, |c| {
                            use squigl_engine::transcribe::Confidence;
                            match c {
                                Confidence::Steady => None,
                                Confidence::Wavering => Some([255, 179, 0, 70]),
                                Confidence::Hesitant => Some([229, 57, 53, 80]),
                            }
                        });
                    let out = typesetter
                        .render(&text, &spans, width_pt, size_pt, scale, rgb)
                        .ok()
                        .map(|(img, sc)| {
                            let (w, h) = (img.width() as usize, img.height() as usize);
                            (img.into_raw(), w, h, sc)
                        });
                    let _ = tx.send((key, out));
                });
                None
            }
        }
    }
}

impl AppView {
    /// Opens (or focuses the state of) a pane's own window.
    pub fn detach_pane(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        let shared = match kind {
            "zoom" => Arc::clone(&self.detached_zoom),
            "reading" => Arc::clone(&self.detached_reading),
            _ => Arc::clone(&self.detached_preview),
        };
        if shared.is_open() {
            return;
        }
        if self.maximised == Some(kind) {
            self.maximised = None;
        }
        *shared.config.lock().unwrap() = Some(self.engine.config().clone());
        let for_view = Arc::clone(&shared);
        let title = shared.title;
        let opened = gpui_kit::open_window(
            gpui_kit::WindowOptions {
                titlebar: Some(gpui_kit::TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                let shared = Arc::clone(&for_view);
                cx.new(|cx| crate::detach::DetachedView::new(shared, window, cx))
            },
        );
        match opened {
            Ok(_) => log::info!("detached the {kind} pane into its own window"),
            Err(e) => {
                log::error!("could not open the {kind} window: {e}");
                self.notice = format!("Could not open the {kind} window: {e}");
            }
        }
        cx.notify();
    }

    pub fn pane_detached(&self, kind: &str) -> bool {
        match kind {
            "zoom" => self.detached_zoom.is_open(),
            "reading" => self.detached_reading.is_open(),
            _ => self.detached_preview.is_open(),
        }
    }

    fn publish_detached(&mut self) {
        if self.detached_preview.is_open() {
            *self.detached_preview.image.lock().unwrap() = self.image.clone();
        }
        if self.detached_zoom.is_open() {
            *self.detached_zoom.image.lock().unwrap() = self.zoom_image.clone();
        }
        if self.detached_reading.is_open() {
            let mut lines = Vec::new();
            let in_order = self.engine_reading().in_page_order;
            let mut indices: Vec<usize> = (0..self.results.len()).collect();
            if !in_order {
                indices.reverse();
            }
            for index in indices {
                let Some(detail) = self.result_detail(index) else {
                    continue;
                };
                let mut head = detail.label.clone().unwrap_or_default();
                if let Some(backend) = &detail.backend {
                    if !head.is_empty() {
                        head.push_str(" - ");
                    }
                    head.push_str(&format!("{backend} - {:.1}s", detail.elapsed_secs));
                }
                if !head.is_empty() {
                    lines.push(crate::detach::ReadingLine::Text(head));
                }
                for r_idx in 0..detail.readings.len() {
                    if let Some((img, scale, w, h)) = self.typeset_image(index, r_idx) {
                        lines.push(crate::detach::ReadingLine::Image(img, scale, w, h));
                    } else {
                        lines.push(crate::detach::ReadingLine::Text(
                            detail.readings[r_idx].0.clone(),
                        ));
                    }
                }
                if let Some(error) = &detail.error {
                    lines.push(crate::detach::ReadingLine::Text(error.clone()));
                }
            }
            if !self.history.is_empty() {
                lines.push(crate::detach::ReadingLine::Text("History".to_string()));
                for entry in self.history_entries() {
                    lines.push(crate::detach::ReadingLine::Text(entry));
                }
            }
            *self.detached_reading.lines.lock().unwrap() = lines;
        }
    }

    /// A shortcut fired in a detached window: everything window-free, run
    /// against the main window's engine and view state.
    pub fn detached_action(&mut self, action: &str, cx: &mut Context<Self>) {
        match action {
            "freeze" => self.toggle_freeze(cx),
            "rotate-cw" => self.rotate(true, cx),
            "rotate-ccw" => self.rotate(false, cx),
            "zoom-in" => self.zoom_view(1.0, cx),
            "zoom-out" => self.zoom_view(-1.0, cx),
            "pan-left" => self.pan(-0.2, 0.0, cx),
            "pan-right" => self.pan(0.2, 0.0, cx),
            "pan-up" => self.pan(0.0, -0.2, cx),
            "pan-down" => self.pan(0.0, 0.2, cx),
            "next-mode" => self.cycle_mode(cx),
            "camera-zoom-in" => self.cmd(Command::StepZoom { steps: 4 }, cx),
            "camera-zoom-out" => self.cmd(Command::StepZoom { steps: -4 }, cx),
            "torch" => {
                let on = self.stream_torch();
                self.cmd(Command::SetTorch { on: !on }, cx);
            }
            "read" => self.cmd(Command::Read, cx),
            "read-all" => self.cmd(Command::ReadAll, cx),
            "second-opinion" => self.cmd(Command::SecondOpinion, cx),
            "block-mode" => {
                let on = self.engine_blocks().enabled;
                self.cmd(Command::SetBlockMode { on: !on }, cx);
            }
            "next-block" => self.cmd(Command::StepBlock { delta: 1 }, cx),
            "previous-block" => self.cmd(Command::StepBlock { delta: -1 }, cx),
            "clear-selection" => self.cmd(Command::SetSelection { selection: None }, cx),
            "erase-box" => self.erase_box(cx),
            "undo-erase" => self.undo_erase(cx),
            "speak" => self.cmd(Command::Speak { result: None }, cx),
            "read-aloud" => self.cmd(Command::ReadAloud, cx),
            "pause-speech" => {
                if self.engine_speech().paused {
                    self.cmd(Command::ResumeSpeaking, cx);
                } else {
                    self.cmd(Command::PauseSpeaking, cx);
                }
            }
            "stop-speech" => self.cmd(Command::StopSpeaking, cx),
            "next-spoken" => self.cmd(Command::SkipSpeech { delta: 1 }, cx),
            "previous-spoken" => self.cmd(Command::SkipSpeech { delta: -1 }, cx),
            "save" => self.save(cx),
            _ => {}
        }
    }
}

/// Disjoint highlight spans over `text`: unsure tokens underlined, and the
/// sentence being read aloud (`spoken`, byte range) given a background.
/// Spans are split at the spoken boundaries so GPUI's runs never overlap.
pub fn highlight_spans(
    text: &str,
    tokens: Option<&[squigl_engine::transcribe::Token]>,
    spoken: Option<std::ops::Range<usize>>,
    ui: &squigl_engine::config::UiConfig,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    use squigl_engine::transcribe::Confidence;
    let underline = |colour: u32, wavy: bool| UnderlineStyle {
        thickness: px(1.5),
        color: Some(rgb(colour).into()),
        wavy,
    };
    // Base boundaries: token edges and the spoken range's edges.
    let mut cuts: Vec<usize> = vec![0, text.len()];
    let mut token_style: Vec<(std::ops::Range<usize>, Option<HighlightStyle>)> = Vec::new();
    if let Some(tokens) = tokens {
        let mut at = 0usize;
        for tok in tokens {
            let range = at..at + tok.text.len();
            at += tok.text.len();
            cuts.push(range.start);
            cuts.push(range.end);
            let style = match tok.confidence(ui) {
                Confidence::Steady => None,
                Confidence::Wavering => Some(HighlightStyle {
                    color: Some(rgb(0xffb300).into()),
                    underline: Some(underline(0xffb300, false)),
                    ..Default::default()
                }),
                Confidence::Hesitant => Some(HighlightStyle {
                    color: Some(rgb(0xe53935).into()),
                    underline: Some(underline(0xe53935, true)),
                    ..Default::default()
                }),
            };
            token_style.push((range, style));
        }
    }
    if let Some(sp) = &spoken {
        cuts.push(sp.start.min(text.len()));
        cuts.push(sp.end.min(text.len()));
    }
    cuts.retain(|c| text.is_char_boundary(*c));
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let seg = w[0]..w[1];
        if seg.is_empty() {
            continue;
        }
        let mut style = token_style
            .iter()
            .find(|(r, _)| r.start <= seg.start && seg.end <= r.end)
            .and_then(|(_, s)| *s)
            .unwrap_or_default();
        if let Some(sp) = &spoken {
            if sp.start <= seg.start && seg.end <= sp.end.min(text.len()) {
                style.background_color = Some(
                    gpui_kit::Rgba {
                        r: 1.0,
                        g: 0.77,
                        b: 0.0,
                        a: 0.35,
                    }
                    .into(),
                );
            }
        }
        if style.color.is_some() || style.background_color.is_some() || style.underline.is_some() {
            out.push((seg, style));
        }
    }
    out
}

/// A UTF-16 offset (speech spans are in UTF-16 units) to a byte offset.
pub fn utf16_to_byte(text: &str, units: usize) -> usize {
    let mut used = 0usize;
    for (byte, ch) in text.char_indices() {
        if used >= units {
            return byte;
        }
        used += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: gpui-kit's glob brings GPUI's own `test` macro,
    // which would shadow Rust's here.
    use super::{
        cover_strokes, from_slider, highlight_spans, to_slider, utf16_to_byte, zoom_bounds,
    };
    use squigl_engine::geometry::{Crop, Selection};
    use squigl_engine::stream::SourceSpec;

    #[test]
    fn camera_zoom_slider_is_log_and_round_trips() {
        let bounds = (1.0, 100.0);
        assert_eq!(to_slider(1.0, bounds), 0.0);
        assert_eq!(to_slider(100.0, bounds), 100.0);
        for zoom in [1.0f32, 2.0, 10.0, 55.5, 100.0] {
            let back = from_slider(to_slider(zoom, bounds), bounds);
            assert!((back - zoom).abs() < 0.02, "{zoom} -> {back}");
        }
        // Ends are exact.
        assert_eq!(from_slider(0.0, bounds), 1.0);
        assert_eq!(from_slider(100.0, bounds), 100.0);
    }

    #[test]
    fn highlights_split_at_the_spoken_sentence_and_stay_disjoint() {
        use squigl_engine::transcribe::{Confidence, Token};
        let text = "hello brave world";
        let tokens = vec![
            Token {
                text: "hello ".into(),
                prob: 0.99,
                alternates: Vec::new(),
            },
            Token {
                text: "brave ".into(),
                prob: 0.05,
                alternates: Vec::new(),
            },
            Token {
                text: "world".into(),
                prob: 0.99,
                alternates: Vec::new(),
            },
        ];
        let _ = Confidence::Steady;
        let spans = highlight_spans(text, Some(&tokens), Some(3..8), &Default::default());
        // Sorted and non-overlapping.
        for w in spans.windows(2) {
            assert!(w[0].0.end <= w[1].0.start, "{spans:?}");
        }
        // The hesitant token's range carries a style; the spoken part has a bg.
        assert!(spans
            .iter()
            .any(|(r, st)| r.start >= 6 && st.color.is_some()));
        assert!(spans.iter().any(|(_, st)| st.background_color.is_some()));
    }

    #[test]
    fn utf16_offsets_convert_past_multibyte_chars() {
        let text = "aé𝑏c"; // é: 1 unit/2 bytes, 𝑏: 2 units/4 bytes
        assert_eq!(utf16_to_byte(text, 0), 0);
        assert_eq!(utf16_to_byte(text, 2), 3);
        assert_eq!(utf16_to_byte(text, 4), 7);
        assert_eq!(utf16_to_byte(text, 99), text.len());
    }

    #[test]
    fn cover_strokes_cover_the_rect_in_twelve_pixel_rows() {
        let sel = Selection {
            rect: Crop {
                x: 0,
                y: 0,
                w: 100,
                h: 24,
            },
            quad: None,
        };
        let strokes = cover_strokes(sel);
        assert_eq!(strokes.len(), 2, "24px in 12px rows");
        for st in &strokes {
            // Each row stays inside the box, spilling at most a fifth of a row.
            for pt in &st.points {
                assert!(pt[1] >= 0.0 && pt[1] <= 24.0);
                assert!(pt[0] >= -2.5 && pt[0] <= 102.5);
            }
            assert!(st.radius <= 12.0 / std::f32::consts::SQRT_2 + 0.01);
        }
    }

    #[test]
    fn zoom_bounds_need_a_range_and_a_zoom_camera() {
        assert_eq!(zoom_bounds(None, SourceSpec::Phone), None);
        assert_eq!(zoom_bounds(Some((2.0, 2.0)), SourceSpec::Phone), None);
        assert_eq!(
            zoom_bounds(Some((0.5, 8.0)), SourceSpec::Phone),
            Some((1.0, 8.0))
        );
        assert_eq!(
            zoom_bounds(Some((0.5, 8.0)), SourceSpec::Network),
            Some((0.5, 8.0))
        );
    }
}
