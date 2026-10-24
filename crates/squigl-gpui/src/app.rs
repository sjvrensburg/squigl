//! The GPUI window over `squigl-engine`: the same commands, slices and events
//! the Tauri window used, with no webview, IPC channel or frame socket between
//! the engine and the screen. The engine lives on this thread (as in the egui
//! window); background workers (stream, reads, models, voice) call the waker,
//! which sets a flag the pump loop collects at ~50 Hz, and a new live frame is
//! converted at most once per pump from the engine's decimated planes.

use crate::frame::{lut_key, to_render_image, FrameKey};
use crate::pairing::{Offer, Pairing};
use gpui_kit::component::slider::{SliderEvent, SliderState, SliderValue};
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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ZOOM_FACTOR: f32 = 1.25;

/// Canvas bounds in window coordinates: (origin x, origin y, width, height).
pub type CanvasBounds = Arc<Mutex<Option<(f32, f32, f32, f32)>>>;

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
    pub zoom_slider: Entity<SliderState>,
    pub contrast_slider: Entity<SliderState>,
    pub brightness_slider: Entity<SliderState>,
    pub gamma_slider: Entity<SliderState>,
    /// Set when a slider value must be pushed into its state (engine moved).
    pub sliders_dirty: bool,
    pub results_scroll: ScrollHandle,
    pub _subscriptions: Vec<Subscription>,
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
        let display = engine.config().display;
        let zoom_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(50.0)
        });
        let contrast_slider = cx.new(move |_| {
            SliderState::new()
                .min(0.2)
                .max(3.0)
                .step(0.05)
                .default_value(display.contrast)
        });
        let brightness_slider = cx.new(move |_| {
            SliderState::new()
                .min(-1.0)
                .max(1.0)
                .step(0.05)
                .default_value(display.brightness)
        });
        let gamma_slider = cx.new(move |_| {
            SliderState::new()
                .min(0.2)
                .max(3.0)
                .step(0.05)
                .default_value(display.gamma)
        });
        let mut subs = Vec::new();
        subs.push(
            cx.subscribe(&zoom_slider, |view, _, event: &SliderEvent, cx| {
                if let SliderEvent::Change(SliderValue::Single(pos)) = event {
                    view.camera_zoom_to(*pos, false, cx);
                }
                if let SliderEvent::Release(SliderValue::Single(pos)) = event {
                    view.camera_zoom_to(*pos, true, cx);
                }
            }),
        );
        for (slider, kind) in [
            (contrast_slider.clone(), 0),
            (brightness_slider.clone(), 1),
            (gamma_slider.clone(), 2),
        ] {
            subs.push(
                cx.subscribe(&slider, move |view, _, event: &SliderEvent, cx| {
                    let (value, released) = match event {
                        SliderEvent::Change(SliderValue::Single(v)) => (*v, false),
                        SliderEvent::Release(SliderValue::Single(v)) => (*v, true),
                        _ => return,
                    };
                    view.display_slider(kind, value, released, cx);
                }),
            );
        }
        // The pump: collect whatever the waker flagged, 50 times a second.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(20))
                .await;
            wake_flag.store(false, Ordering::Relaxed);
            if this.update(cx, |view, cx| view.pump(cx)).is_err() {
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
            zoom_slider,
            contrast_slider,
            brightness_slider,
            gamma_slider,
            sliders_dirty: true,
            results_scroll: ScrollHandle::new(),
            _subscriptions: subs,
        }
    }

    pub fn cmd(&mut self, command: Command, cx: &mut Context<Self>) {
        match self.engine.handle(command) {
            Ok(Reply::Done) => {}
            Ok(Reply::Unchanged) => {}
            Err(e) => self.notice = format!("{e:#}"),
        }
        self.pump(cx);
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        // Pairing sessions arrive on their own thread; show the newest.
        let mut changed = false;
        while let Some(session) = self.pairing.take_session() {
            self.engine.pair(session);
            changed = true;
        }
        let events = self.engine.pump(Instant::now());
        changed |= !events.is_empty();
        for event in events {
            match event {
                Event::ResultAppended(result) => self.results.push(result),
                Event::ResultsCleared => self.results.clear(),
                Event::HistoryAppended(entry) => self.history.push(entry),
                Event::Notice(notice) => self.notice = notice.text,
                Event::Frame { .. } | Event::Capture(_) => {}
                Event::Config(_) | Event::Stream(_) => self.sliders_dirty = true,
                _ => {}
            }
        }
        changed |= self.refresh_image();
        // Redraw only on change: at 50 Hz an idle window would otherwise
        // rebuild its whole element tree for nothing.
        if changed {
            cx.notify();
        }
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
        let lut = self.engine.lut();
        let format = if matches!(lut, Lut::Luma(_)) {
            PlaneFormat::Luma
        } else {
            PlaneFormat::Yuv420
        };
        let request = viewport.request(view, FrameRef::Shown, format);
        let Some(planes) = self.engine.render_planes(&request) else {
            return false;
        };
        self.placement = Some(viewport.placement(view));
        self.view_region = Some(request.region);
        let key = FrameKey::of(&planes, lut_key(&lut));
        if self.image_key != Some(key) {
            self.image = Some(to_render_image(&planes, &lut));
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
        self.cmd(
            if self.frozen() {
                Command::Live
            } else {
                Command::Freeze
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
        self.magnification = (self.magnification * ZOOM_FACTOR.powf(steps)).clamp(1.0, max);
        self.pump(cx);
    }

    pub fn pan(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let shown = 1.0 / self.magnification;
        let clamp = |c: f32| c.clamp(shown / 2.0, 1.0 - shown / 2.0);
        self.centre = [
            clamp(self.centre[0] + dx * shown),
            clamp(self.centre[1] + dy * shown),
        ];
        self.pump(cx);
    }

    pub fn cycle_mode(&mut self, cx: &mut Context<Self>) {
        let mut config = self.engine.config().clone();
        let modes = DisplayMode::ALL;
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

    fn camera_zoom_to(&mut self, pos: f32, _release: bool, cx: &mut Context<Self>) {
        let Some(bounds) = zoom_bounds(
            self.engine.state().stream.value.zoom_range,
            self.engine.state().stream.value.source.clone(),
        ) else {
            return;
        };
        let zoom = from_slider(pos, bounds);
        self.cmd(Command::SetZoom { zoom }, cx);
    }

    fn display_slider(&mut self, kind: usize, value: f32, released: bool, cx: &mut Context<Self>) {
        let mut config = self.engine.config().clone();
        match kind {
            0 => config.display.contrast = value,
            1 => config.display.brightness = value,
            _ => config.display.gamma = value,
        }
        self.sliders_dirty = false;
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
        // Rows across the box, spilling by a fifth of a row (the desktop's
        // coverStrokes): the engine paints these into what a read sees.
        let r = selection.rect;
        let rows = 8usize;
        let step = r.h as f32 / rows as f32;
        let mut strokes = self.engine.state().capture.value.erasures.clone();
        self.erase_steps.push(strokes.len());
        for i in 0..rows {
            let y = r.y as f32 + step * (i as f32 + 0.5);
            strokes.push(Stroke::line(
                [r.x as f32 - step * 0.2, y],
                [(r.x + r.w) as f32 + step * 0.2, y],
                step * 0.6,
            ));
        }
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
        let dir: PathBuf = squigl_engine::paths::pictures_dir();
        self.cmd(Command::Save { dir }, cx);
        self.notice = "Saved the picture".into();
    }

    pub fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = action_for(event, self.engine.config()) else {
            return;
        };
        self.run_action(&action, window, cx);
        cx.stop_propagation();
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
            match self
                .pairing
                .start(self.engine.config().desktop.tailscale_https)
            {
                Ok(offers) => {
                    self.pair_offers = offers;
                    self.pair_note.clear();
                }
                Err(e) => self.pair_note = e,
            }
        } else {
            self.pairing.stop();
        }
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
            "freeze" => self.toggle_freeze(cx),
            "rotate-cw" => self.rotate(true, cx),
            "rotate-ccw" => self.rotate(false, cx),
            "zoom-in" => self.zoom_view(1.0, cx),
            "zoom-out" => self.zoom_view(-1.0, cx),
            "zoom-reset" => {
                self.magnification = 1.0;
                self.centre = [0.5, 0.5];
                self.pump(cx);
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
                if self.erasing {
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

const ACTIONS: [(&str, &str); 36] = [
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
    ("save", "S"),
];

/// The key an engine event used, in the desktop's `KeyboardEvent.key` terms.
fn key_name(event: &KeyDownEvent) -> String {
    let ks = &event.keystroke;
    let mut key = match ks.key.as_str() {
        "space" => " ".to_string(),
        "enter" => "Enter".to_string(),
        "escape" => "Escape".to_string(),
        "arrowleft" => "ArrowLeft".to_string(),
        "arrowright" => "ArrowRight".to_string(),
        "arrowup" => "ArrowUp".to_string(),
        "arrowdown" => "ArrowDown".to_string(),
        "equal" => "+".to_string(),
        "minus" => "-".to_string(),
        "comma" => ",".to_string(),
        "period" => ".".to_string(),
        other => other.to_string(),
    };
    if ks.modifiers.shift && key.chars().count() == 1 {
        key = key.to_uppercase();
    }
    // Ctrl+S is Save, told apart from plain S (stop speech) by the modifier:
    // the desktop keeps them as different keys the same way.
    if ks.modifiers.control && key.eq_ignore_ascii_case("s") {
        return "ctrl+s".to_string();
    }
    key
}

fn action_for(event: &KeyDownEvent, config: &Config) -> Option<String> {
    let key = key_name(event);
    if key == "ctrl+s" {
        return Some("save".to_string());
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
    Draw { from: (usize, usize) },
    Move { shown: Crop },
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
            self.refresh_image();
        }
    }
    pub fn clear_drag(&mut self) {
        self.drag = None;
    }
    pub fn take_drag(&mut self) -> Option<FinishedDrag> {
        match self.drag.take() {
            Some(Drag::Draw { from, .. }) => Some(FinishedDrag::Draw { from }),
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
        self.engine
            .state()
            .blocks
            .value
            .blocks
            .iter()
            .position(|b| b.rect.contains(point.0, point.1))
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
    pub fn result_label(&self, index: usize) -> Option<String> {
        self.results.get(index).and_then(|r| r.label.clone())
    }
    pub fn result_text(&self, index: usize) -> Option<String> {
        let r = self.results.get(index)?;
        match &r.result {
            Ok(t) => Some(
                t.readings
                    .first()
                    .map_or_else(|| "(no reading)".to_string(), |x| x.text.clone()),
            ),
            Err(e) => Some(e.clone()),
        }
    }
    pub fn results_list(&self) -> Vec<(usize, String, bool)> {
        self.results
            .iter()
            .enumerate()
            .map(|(i, r)| match &r.result {
                Ok(t) => (
                    i,
                    t.readings
                        .first()
                        .map_or("(no reading)", |x| x.text.as_str())
                        .to_string(),
                    false,
                ),
                Err(e) => (i, e.clone(), true),
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

pub fn slider_slot_contrast(view: &AppView) -> gpui_kit::component::slider::Slider {
    gpui_kit::component::slider::Slider::new(&view.contrast_slider)
}
pub fn slider_slot_brightness(view: &AppView) -> gpui_kit::component::slider::Slider {
    gpui_kit::component::slider::Slider::new(&view.brightness_slider)
}
pub fn slider_slot_gamma(view: &AppView) -> gpui_kit::component::slider::Slider {
    gpui_kit::component::slider::Slider::new(&view.gamma_slider)
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: gpui-kit's glob brings GPUI's own `test` macro,
    // which would shadow Rust's here.
    use super::{from_slider, to_slider, zoom_bounds};
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
