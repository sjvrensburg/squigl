//! Drawing the window: toolbar, GPU picture with its overlay, reading pane,
//! settings, pairing and connection guidance. State comes from the engine's
//! slices (cloned once per render); actions go back as engine commands.

use crate::app::{mode_label, role_colour, theme_colours, zoom_bounds, AppView, SliderKind};
use crate::guidance::guidance;
use gpui_kit::component::button::Button;
use gpui_kit::*;
use squigl_engine::engine::StreamStatus;
use squigl_engine::engine::{BlocksSlice, CaptureSlice, StreamSlice};
use squigl_engine::geometry::Crop;
use squigl_engine::stream::SourceSpec;

fn btn(id: &'static str, label: impl Into<SharedString>) -> Button {
    Button::new(id).label(label).compact()
}

/// A custom slider: a track whose bounds are tracked by a canvas, so the
/// value is always the engine's (a dragged thumb and an external change
/// agree), unlike gpui-base's SliderState in this gpui version.
pub fn slider(
    view: &mut AppView,
    kind: SliderKind,
    fraction: f32,
    width: Option<f32>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let bounds_slot = view.slider_bounds_handle();
    let name = kind.name();
    let mut track = div()
        .relative()
        .h(px(22.0))
        .flex()
        .items_center()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                view.active_slider = Some(kind);
                if let Some(f) = view.slider_fraction(kind, event.position) {
                    view.slider_apply(kind, f, false, cx);
                }
            }),
        )
        .on_mouse_move(cx.listener(move |view, event: &MouseMoveEvent, _, cx| {
            if view.active_slider == Some(kind) && event.pressed_button == Some(MouseButton::Left) {
                if let Some(f) = view.slider_fraction(kind, event.position) {
                    view.slider_apply(kind, f, false, cx);
                }
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseUpEvent, _, cx| {
                if view.active_slider == Some(kind) {
                    view.active_slider = None;
                    if let Some(f) = view.slider_fraction(kind, event.position) {
                        view.slider_apply(kind, f, true, cx);
                    }
                }
            }),
        );
    if let Some(w) = width {
        track = track.w(px(w));
    } else {
        track = track.w_full();
    }
    track = track.child(
        canvas(
            move |bounds, _, _| {
                bounds_slot.lock().unwrap().insert(
                    name,
                    (
                        bounds.origin.x.as_f32(),
                        bounds.origin.y.as_f32(),
                        bounds.size.width.as_f32(),
                        bounds.size.height.as_f32(),
                    ),
                );
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full(),
    );
    track
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .right(px(0.0))
                .top(px(10.0))
                .h(px(3.0))
                .bg(rgb(0x55555e)),
        )
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .top(px(10.0))
                .h(px(3.0))
                .w(relative(fraction.clamp(0.0, 1.0)))
                .bg(rgb(0xffc400)),
        )
        .child(
            div()
                .absolute()
                .left(relative(fraction.clamp(0.0, 1.0)))
                .top(px(4.0))
                .w(px(14.0))
                .h(px(14.0))
                .rounded_full()
                .bg(rgb(0xe8e8e8))
                .border(px(1.0))
                .border_color(rgb(0x333338)),
        )
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.scale_factor = window.scale_factor();
        // Release the replaced frame's atlas tiles (each RenderImage mints a
        // new one; without this they accumulate for the app's life).
        if self.shown_image.as_ref().map(|i| i.id) != self.image.as_ref().map(|i| i.id) {
            if let Some(old) = self.shown_image.take() {
                let _ = window.drop_image(old);
            }
            self.shown_image = self.image.clone();
        }
        let config = self.engine.config().clone();
        let (bg, fg, _edge) = theme_colours(config.desktop.theme);
        let stream = self.engine.state().stream.value.clone();
        let capture = self.engine.state().capture.value.clone();
        let blocks = self.engine.state().blocks.value.clone();
        let frozen = capture.frozen.is_some();

        let mut root = div()
            .track_focus(&self.focus_clone())
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key(event, window, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(bg))
            .text_color(rgb(fg));

        if !self.reading_only {
            root = root.child(self.toolbar(&stream, &config, frozen, cx));
        }
        let mut main = div().flex().flex_row().flex_1().min_h(px(0.0));
        if !self.reading_only {
            main = main.child(self.picture(&capture, &blocks, &config, cx));
        }
        if self.reading_open || self.reading_only {
            main = main.child(self.reading_pane(self.reading_only, cx));
        }
        root = root.child(main);
        root = root.child(self.status_bar(&stream, frozen));
        if self.settings_open {
            root = root.child(self.settings_panel(cx));
        }
        if self.pair_open {
            root = root.child(self.pair_panel(cx));
        }
        if let StreamStatus::Waiting {
            problem: Some(problem),
            reason,
            ..
        } = &stream.status
        {
            if self.guidance_hidden != Some(*problem) {
                root = root.child(self.guidance_panel(*problem, reason.clone(), cx));
            }
        }
        root
    }
}

impl AppView {
    fn toolbar(
        &mut self,
        stream: &StreamSlice,
        config: &squigl_engine::config::Config,
        frozen: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut bar = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_1()
            .p_1()
            .items_center();
        bar = bar.child(
            btn(
                "freeze",
                if frozen {
                    "Live (Space)"
                } else {
                    "Freeze (Space)"
                },
            )
            .on_click(cx.listener(|view, _, _, cx| view.toggle_freeze(cx))),
        );
        bar = bar.child(btn("rot-ccw", "Rotate left (R)").on_click(cx.listener(
            |view, _, _, cx| {
                view.rotate(false, cx);
            },
        )));
        bar = bar.child(btn("rot-cw", "Rotate right (r)").on_click(cx.listener(
            |view, _, _, cx| {
                view.rotate(true, cx);
            },
        )));
        bar = bar.child(
            btn("mode", mode_label(config.display.mode)).on_click(cx.listener(|view, _, _, cx| {
                view.cycle_mode(cx);
            })),
        );
        bar = bar.child(
            btn("zoom-out", "Zoom -").on_click(cx.listener(|view, _, _, cx| {
                view.zoom_view(-1.0, cx);
            })),
        );
        bar = bar.child(div().child(format!("{:.0}%", self.magnification * 100.0)));
        bar = bar.child(
            btn("zoom-in", "Zoom +").on_click(cx.listener(|view, _, _, cx| {
                view.zoom_view(1.0, cx);
            })),
        );
        if let Some(bounds) = zoom_bounds(stream.zoom_range, stream.source.clone()) {
            let fraction = ((stream.zoom.ln() - bounds.0.ln()) / (bounds.1.ln() - bounds.0.ln()))
                .clamp(0.0, 1.0);
            bar = bar.child(slider(self, SliderKind::Zoom, fraction, Some(140.0), cx));
            bar = bar.child(div().child(format!("Camera {:.2}x", stream.zoom)));
        }
        if stream.capabilities.torch {
            bar = bar.child(
                btn(
                    "torch",
                    if stream.torch {
                        "Torch on (t)"
                    } else {
                        "Torch (t)"
                    },
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    let on = view.stream_torch();
                    view.cmd(squigl_engine::engine::Command::SetTorch { on: !on }, cx);
                })),
            );
        }
        bar = bar.child(btn("open", "Open image...").on_click(cx.listener(
            |view, _, window, cx| {
                view.open_image_dialog(window, cx);
            },
        )));
        bar = bar.child(btn("replay", "Play recording...").on_click(cx.listener(
            |view, _, _, cx| {
                view.open_replay_dialog(cx);
            },
        )));
        if !matches!(stream.source, SourceSpec::Phone) {
            bar = bar.child(
                btn("phone", "Use phone").on_click(cx.listener(|view, _, _, cx| {
                    view.cmd(
                        squigl_engine::engine::Command::UseSource {
                            source: SourceSpec::Phone,
                        },
                        cx,
                    );
                })),
            );
        }
        bar = bar.child(
            btn("pair", "Pair phone").on_click(cx.listener(|view, _, _, cx| {
                view.toggle_pair(cx);
            })),
        );
        bar = bar.child(
            btn("reading", "Reading (p)").on_click(cx.listener(|view, _, _, cx| {
                view.toggle_reading(cx);
            })),
        );
        bar = bar.child(
            btn("save", "Save (Ctrl+S)").on_click(cx.listener(|view, _, _, cx| {
                view.save(cx);
            })),
        );
        bar = bar.child(
            btn("settings", "Settings (,)").on_click(cx.listener(|view, _, _, cx| {
                view.toggle_settings(cx);
            })),
        );
        bar = bar.child(
            btn("full", "Full screen (f)").on_click(cx.listener(|_, _, window, _| {
                window.toggle_fullscreen();
            })),
        );
        bar
    }

    fn quad_outline(
        &self,
        corners: [[f32; 2]; 4],
        colour: u32,
        width: f32,
        dashed: bool,
    ) -> impl IntoElement {
        let pts: Vec<(f32, f32)> = corners.iter().map(|c| self.to_canvas(*c)).collect();
        canvas(
            move |_, _, _| pts.clone(),
            move |bounds, pts, window, _| {
                let origin = bounds.origin;
                let mut builder = PathBuilder::stroke(px(width));
                if dashed {
                    builder = builder.dash_array(&[px(6.0), px(4.0)]);
                }
                for i in 0..4 {
                    let a = pts[i];
                    let b = pts[(i + 1) % 4];
                    builder.move_to(point(px(a.0) + origin.x, px(a.1) + origin.y));
                    builder.line_to(point(px(b.0) + origin.x, px(b.1) + origin.y));
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, rgb(colour));
                }
            },
        )
        .absolute()
        .size_full()
    }

    fn picture(
        &mut self,
        capture: &CaptureSlice,
        blocks: &BlocksSlice,
        config: &squigl_engine::config::Config,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let canvas_slot = self.canvas_slot();
        let mut area = div()
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .bg(rgb(0x000000))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, _, cx| view.mouse_down(event, cx)),
            )
            .on_mouse_move(
                cx.listener(|view, event: &MouseMoveEvent, _, cx| view.mouse_move(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, event: &MouseUpEvent, _, cx| view.mouse_up(event, cx)),
            )
            .on_scroll_wheel(
                cx.listener(|view, event: &ScrollWheelEvent, _, cx| view.scroll_wheel(event, cx)),
            );
        area = area.child(
            canvas(
                move |bounds, _, _| {
                    *canvas_slot.lock().unwrap() = Some((
                        bounds.origin.x.as_f32(),
                        bounds.origin.y.as_f32(),
                        bounds.size.width.as_f32(),
                        bounds.size.height.as_f32(),
                    ));
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        );
        if let Some(image) = self.image.clone() {
            if let Some((left, top, w, h)) = self.image_rect() {
                area = area.child(
                    img(image)
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(w))
                        .h(px(h)),
                );
            }
        } else {
            area = area.child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child("Waiting for the picture..."),
            );
        }
        // Detected blocks: their quads, numbered, kind by colour and dash.
        if blocks.enabled {
            for (i, block) in blocks.blocks.iter().enumerate() {
                let colour = role_colour(block.role());
                let corners = block
                    .quad
                    .unwrap_or_else(|| squigl_engine::layout::rect_quad(block.rect));
                // Kind by dash as well as colour (the desktop's encoding):
                // text solid, formula dashed, figure dotted-ish, other thin.
                let (width, dashed) = match block.role() {
                    squigl_engine::layout::Role::Text => {
                        (if blocks.selected == Some(i) { 3.0 } else { 2.0 }, false)
                    }
                    squigl_engine::layout::Role::Formula => (2.0, true),
                    squigl_engine::layout::Role::Figure => (2.0, true),
                    squigl_engine::layout::Role::Other => (1.0, false),
                };
                area = area.child(self.quad_outline(corners, colour, width, dashed));
                let (lx, ly) = self.to_canvas(corners[0]);
                area = area.child(
                    div()
                        .absolute()
                        .left(px(lx))
                        .top(px(ly - 16.0))
                        .text_xs()
                        .bg(rgb(colour))
                        .text_color(rgb(0x000000))
                        .child(format!("{}", i + 1)),
                );
            }
        }
        // The block being read aloud.
        if let Some(spoken) = self.spoken_selection() {
            let corners = spoken
                .quad
                .unwrap_or_else(|| squigl_engine::layout::rect_quad(spoken.rect));
            area = area.child(self.quad_outline(corners, 0xffffff, 3.0, false));
        }
        // Selection (or the in-progress drag's box), with corner handles.
        let shown_selection = match self.drag_box() {
            Some(b) => Some(squigl_engine::geometry::Selection {
                rect: b,
                quad: None,
            }),
            None => capture.selection,
        };
        if let Some(selection) = shown_selection {
            let corners: [[f32; 2]; 4] = selection.quad.unwrap_or_else(|| {
                let r: Crop = selection.rect;
                [
                    [r.x as f32, r.y as f32],
                    [(r.x + r.w) as f32, r.y as f32],
                    [(r.x + r.w) as f32, (r.y + r.h) as f32],
                    [r.x as f32, (r.y + r.h) as f32],
                ]
            });
            area = area.child(self.quad_outline(corners, 0xffc400, 2.0, false));
            for corner in corners {
                let (x, y) = self.to_canvas(corner);
                area = area.child(
                    div()
                        .absolute()
                        .left(px(x - 5.0))
                        .top(px(y - 5.0))
                        .w(px(10.0))
                        .h(px(10.0))
                        .bg(rgb(0xffc400)),
                );
            }
        }
        // The erase stroke being painted.
        if self.drag_is_erase() {
            for pt in self.erase_points_view() {
                let (x, y) = self.to_canvas(pt);
                area = area.child(
                    div()
                        .absolute()
                        .left(px(x - 3.0))
                        .top(px(y - 3.0))
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded_full()
                        .bg(rgb(0xff5a5a)),
                );
            }
        }
        if config.magnifier.reading_line {
            area = area.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(relative(0.5))
                    .h(px(1.0))
                    .bg(rgb(0xffc400)),
            );
        }
        area
    }

    fn status_bar(&mut self, stream: &StreamSlice, frozen: bool) -> impl IntoElement {
        let status = match &stream.status {
            StreamStatus::Connecting => match stream.source {
                SourceSpec::Phone => "Connecting to the phone...".to_string(),
                SourceSpec::Network => "Waiting for the paired phone...".to_string(),
                _ => "Opening...".to_string(),
            },
            StreamStatus::Streaming { width, height } => {
                format!(
                    "{} {}x{}",
                    if frozen { "Frozen" } else { "Live" },
                    width,
                    height
                )
            }
            StreamStatus::Waiting { reason, .. } => format!("{reason} - trying again shortly"),
            StreamStatus::Stopped => "Stopped".to_string(),
        };
        div()
            .flex()
            .flex_row()
            .gap_2()
            .p_1()
            .child(div().child(status))
            .child(div().flex_1().child(self.notice_text()))
    }

    fn guidance_panel(
        &mut self,
        problem: squigl_engine::stream::Problem,
        reason: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let g = guidance(problem);
        let mut panel = div()
            .absolute()
            .left(relative(0.3))
            .top(relative(0.25))
            .w(relative(0.4))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(rgb(0x24242c))
            .border(px(1.0))
            .border_color(rgb(0x888888))
            .child(div().text_lg().child(g.title));
        for step in g.steps {
            panel = panel.child(div().child(format!("- {step}")));
        }
        if problem == squigl_engine::stream::Problem::Other {
            panel = panel.child(div().child(reason));
        }
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(
                    btn("try-now", "Try now").on_click(cx.listener(|view, _, _, cx| {
                        view.hide_guidance(cx);
                        view.cmd(squigl_engine::engine::Command::Reconnect, cx);
                    })),
                )
                .child(
                    btn("open-instead", "Open an image instead").on_click(cx.listener(
                        |view, _, window, cx| {
                            view.hide_guidance(cx);
                            view.open_image_dialog(window, cx);
                        },
                    )),
                )
                .child(
                    btn("hide-guidance", "Hide").on_click(cx.listener(|view, _, _, cx| {
                        view.hide_guidance(cx);
                    })),
                ),
        );
        panel
    }
}
