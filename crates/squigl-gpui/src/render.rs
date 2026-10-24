//! Drawing the window: toolbar, GPU picture with its overlay, reading pane,
//! settings, pairing and connection guidance. State comes from the engine's
//! slices (cloned out of `engine.state()` per render, as the egui window did);
//! actions go back as engine commands.

use crate::app::{mode_label, role_colour, theme_colours, zoom_bounds, AppView};
use crate::guidance::guidance;
use gpui_kit::component::button::Button;
use gpui_kit::component::slider::Slider;
use gpui_kit::*;
use squigl_engine::engine::Command;
use squigl_engine::engine::StreamStatus;
use squigl_engine::geometry::Crop;
use squigl_engine::layout::Role;
use squigl_engine::stream::SourceSpec;

fn btn(id: &'static str, label: impl Into<SharedString>) -> Button {
    Button::new(id).label(label).compact()
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.scale_factor = window.scale_factor();
        let _ = window;
        let config = self.engine.config().clone();
        let (bg, fg, _edge) = theme_colours(config.desktop.theme);
        let stream = self.engine.state().stream.value.clone();
        let capture = self.engine.state().capture.value.clone();
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
            main = main.child(self.picture(cx));
        }
        if self.reading_open || self.reading_only {
            main = main.child(self.reading_pane(cx));
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
    // Sliders keep their own state (gpui-base's SliderState); they are
    // seeded from the config when the view is made and report Change/Release
    // back as Preview/SetConfig, so no programmatic sync is needed here.

    fn toolbar(
        &mut self,
        stream: &squigl_engine::engine::StreamSlice,
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
        if zoom_bounds(stream.zoom_range, stream.source.clone()).is_some() {
            bar = bar.child(div().w(px(140.0)).child(Slider::new(&self.zoom_slider)));
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
                    view.cmd(Command::SetTorch { on: !on }, cx);
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
                        Command::UseSource {
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

    fn picture(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let canvas_slot = self.canvas_slot();
        let capture = self.engine.state().capture.value.clone();
        let blocks = self.engine.state().blocks.value.clone();
        let config = self.engine.config().clone();
        let mut area = div()
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .bg(rgb(0x000000))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, _, cx| view.mouse_down(event, cx)),
            )
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, cx| {
                view.mouse_move(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, event: &MouseUpEvent, _, cx| view.mouse_up(event, cx)),
            )
            .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, _, cx| {
                view.scroll_wheel(event, cx);
            }));
        // Bounds bookkeeping (position + size) for pointer/viewport mapping.
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
        // Detected blocks, numbered in reading order, outlined by kind.
        if blocks.enabled {
            for (i, block) in blocks.blocks.iter().enumerate() {
                let colour = role_colour(block.role());
                let (x0, y0) = self.to_canvas([block.rect.x as f32, block.rect.y as f32]);
                let (x1, y1) = self.to_canvas([
                    (block.rect.x + block.rect.w) as f32,
                    (block.rect.y + block.rect.h) as f32,
                ]);
                let selected = blocks.selected == Some(i);
                area = area.child(
                    div()
                        .absolute()
                        .left(px(x0))
                        .top(px(y0))
                        .w(px((x1 - x0).max(2.0)))
                        .h(px((y1 - y0).max(2.0)))
                        .border(px(if selected { 3.0 } else { 2.0 }))
                        .border_color(rgb(colour))
                        .child(
                            div()
                                .text_xs()
                                .bg(rgb(colour))
                                .text_color(rgb(0x000000))
                                .child(format!("{}", i + 1)),
                        ),
                );
            }
        }
        // Selection box + corner handles (the quad when it is not a rectangle).
        if let Some(selection) = capture.selection {
            let corners: [[f32; 2]; 4] = selection.quad.unwrap_or_else(|| {
                let r: Crop = selection.rect;
                [
                    [r.x as f32, r.y as f32],
                    [(r.x + r.w) as f32, r.y as f32],
                    [(r.x + r.w) as f32, (r.y + r.h) as f32],
                    [r.x as f32, (r.y + r.h) as f32],
                ]
            });
            let pts: Vec<(f32, f32)> = corners.iter().map(|c| self.to_canvas(*c)).collect();
            let (minx, miny) = (
                pts.iter().map(|p| p.0).fold(f32::MAX, f32::min),
                pts.iter().map(|p| p.1).fold(f32::MAX, f32::min),
            );
            let (maxx, maxy) = (
                pts.iter().map(|p| p.0).fold(f32::MIN, f32::max),
                pts.iter().map(|p| p.1).fold(f32::MIN, f32::max),
            );
            area = area.child(
                div()
                    .absolute()
                    .left(px(minx))
                    .top(px(miny))
                    .w(px((maxx - minx).max(2.0)))
                    .h(px((maxy - miny).max(2.0)))
                    .border(px(2.0))
                    .border_color(rgb(0xffc400)),
            );
            for (x, y) in pts {
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
        let _ = Role::Other;
        area
    }

    fn status_bar(
        &mut self,
        stream: &squigl_engine::engine::StreamSlice,
        frozen: bool,
    ) -> impl IntoElement {
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
                        view.cmd(Command::Reconnect, cx);
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
