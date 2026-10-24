//! Detached pane windows (the egui window's pop-out panes). The engine stays
//! on the main window's thread; a detached window is a mirror: the main view
//! publishes snapshots (frame, zoom picture, readings) into shared state each
//! pump, and the detached view sends engine `Command`s and shortcut action
//! names back through the same state for the main view to run. Closing a
//! detached window drops its view, which marks the pane closed so the main
//! window shows it again.

use gpui_kit::component::button::Button;
use gpui_kit::*;
use squigl_engine::config::Config;
use squigl_engine::engine::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum ReadingLine {
    Text(String),
    Image(Arc<RenderImage>, f32, usize, usize),
}

/// One thing a detached window asked the main window to do. Commands and
/// shortcut actions share one queue so they run in the order they were
/// pressed (a Read click followed by Escape must not swap).
pub enum DetachedMsg {
    Command(Command),
    Action(String),
}

pub struct DetachedShared {
    pub kind: &'static str,
    pub title: &'static str,
    pub open: Mutex<bool>,
    pub image: Mutex<Option<Arc<RenderImage>>>,
    pub lines: Mutex<Vec<ReadingLine>>,
    pub queue: Mutex<Vec<DetachedMsg>>,
    pub config: Mutex<Option<Config>>,
    /// The detached Reading window's content width in logical px, so a
    /// typeset reading is laid out for the window that shows it.
    pub width: Mutex<Option<f32>>,
}

impl DetachedShared {
    pub fn new(kind: &'static str) -> Arc<Self> {
        let title = match kind {
            "preview" => "Squigl - Preview",
            "zoom" => "Squigl - Zoom",
            _ => "Squigl - Reading",
        };
        Arc::new(Self {
            kind,
            title,
            open: Mutex::new(false),
            image: Mutex::new(None),
            lines: Mutex::new(Vec::new()),
            queue: Mutex::new(Vec::new()),
            config: Mutex::new(None),
            width: Mutex::new(None),
        })
    }

    pub fn is_open(&self) -> bool {
        *self.open.lock().unwrap()
    }
}

pub struct DetachedView {
    shared: Arc<DetachedShared>,
    focus: FocusHandle,
    /// Images shown by the previous render: ones no longer shown are
    /// dropped from this window's atlas instead of accumulating.
    shown: Vec<Arc<RenderImage>>,
}

impl DetachedView {
    pub fn new(shared: Arc<DetachedShared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        *shared.open.lock().unwrap() = true;
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        })
        .detach();
        Self {
            shared,
            focus,
            shown: Vec::new(),
        }
    }
}

impl Drop for DetachedView {
    fn drop(&mut self) {
        *self.shared.open.lock().unwrap() = false;
        // Nothing queued by a closed window may run later.
        self.shared.queue.lock().unwrap().clear();
    }
}

fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).label(label).compact()
}

impl Render for DetachedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shared = Arc::clone(&self.shared);
        // Release atlas tiles for images this render no longer shows.
        let mut now_shown: Vec<Arc<RenderImage>> = Vec::new();
        if let Some(image) = shared.image.lock().unwrap().clone() {
            now_shown.push(image);
        }
        for line in shared.lines.lock().unwrap().iter() {
            if let ReadingLine::Image(image, ..) = line {
                now_shown.push(image.clone());
            }
        }
        for old in std::mem::replace(&mut self.shown, now_shown) {
            if !self.shown.iter().any(|i| i.id == old.id) {
                let _ = window.drop_image(old);
            }
        }
        let mut root = div()
            .track_focus(&self.focus)
            .on_key_down({
                let shared = Arc::clone(&self.shared);
                move |event: &KeyDownEvent, _, _| {
                    if let Some(config) = shared.config.lock().unwrap().clone() {
                        if let Some(action) = crate::app::action_for(event, &config) {
                            shared
                                .queue
                                .lock()
                                .unwrap()
                                .push(DetachedMsg::Action(action));
                        }
                    }
                }
            })
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb({
                let (bg, _, _) = crate::app::theme_colours(
                    shared
                        .config
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map_or(Default::default(), |c| c.desktop.theme),
                );
                bg
            }))
            .text_color(rgb({
                let (_, fg, _) = crate::app::theme_colours(
                    shared
                        .config
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map_or(Default::default(), |c| c.desktop.theme),
                );
                fg
            }));
        match self.shared.kind {
            "reading" => {
                let mut controls = div().flex().flex_row().flex_wrap().gap_1().p_1();
                for (id, label, command) in [
                    ("d-read", "Read (Enter)", Command::Read),
                    ("d-read-all", "Read all (a)", Command::ReadAll),
                    ("d-opinion", "Second opinion (o)", Command::SecondOpinion),
                    ("d-stop-read", "Stop", Command::CancelRead),
                    ("d-speak", "Read aloud (s)", Command::Speak { result: None }),
                    ("d-stop-speech", "Stop speaking (S)", Command::StopSpeaking),
                ] {
                    let shared = Arc::clone(&self.shared);
                    controls = controls.child(btn(id, label).on_click(move |_, _, _| {
                        shared
                            .queue
                            .lock()
                            .unwrap()
                            .push(DetachedMsg::Command(command.clone()));
                    }));
                }
                let shared_clear = Arc::clone(&self.shared);
                controls =
                    controls.child(btn("d-clear", "Clear readings").on_click(move |_, _, _| {
                        shared_clear
                            .queue
                            .lock()
                            .unwrap()
                            .push(DetachedMsg::Command(Command::ClearResults));
                    }));
                let _ = cx;
                root = root.child(controls);
                let width_slot = Arc::clone(&self.shared);
                let mut list = div()
                    .id("detached-readings")
                    .relative()
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                *width_slot.width.lock().unwrap() =
                                    Some(bounds.size.width.as_f32());
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2()
                    .flex_1()
                    .overflow_y_scroll();
                for line in shared.lines.lock().unwrap().iter() {
                    match line {
                        ReadingLine::Text(text) => {
                            list = list.child(div().text_size(px(21.0)).child(text.clone()));
                        }
                        ReadingLine::Image(image, scale, w, h) => {
                            list = list.child(
                                img(image.clone())
                                    .w(px(*w as f32 / *scale))
                                    .h(px(*h as f32 / *scale)),
                            );
                        }
                    }
                }
                root = root.child(list);
            }
            _ => {
                if let Some(image) = shared.image.lock().unwrap().clone() {
                    root = root.child(img(image).size_full().object_fit(ObjectFit::Contain));
                } else {
                    root = root.child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child("Waiting for the picture..."),
                    );
                }
            }
        }
        root
    }
}
