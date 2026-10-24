//! Pointer gestures on the picture, and the reading/settings/pairing panels.
//! Gestures mirror the Tauri overlay: drag draws a box, drag inside moves it,
//! a corner drags, a click picks the block under it (or clears), the erase
//! brush paints strokes, alt-drag pans, the wheel magnifies.

use crate::app::{mode_label, AppView};
use gpui_kit::component::button::Button;
use gpui_kit::component::Disableable;
use gpui_kit::*;
use squigl_engine::engine::Command;
use squigl_engine::geometry::{Crop, Selection};
use squigl_engine::layout::rect_quad;
use std::sync::Arc;

fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).label(label).compact()
}

impl AppView {
    fn to_view_f(&self, pos: Point<Pixels>) -> Option<[f32; 2]> {
        let p = self.placement()?;
        let (ox, oy, _, _) = self.canvas_bounds()?;
        let dpr = self.scale_factor().max(1.0);
        let view = self.view_size()?;
        let x = p.origin[0] + (pos.x.as_f32() - ox) * dpr / p.scale;
        let y = p.origin[1] + (pos.y.as_f32() - oy) * dpr / p.scale;
        Some([
            x.clamp(0.0, view.0 as f32 - 1.0),
            y.clamp(0.0, view.1 as f32 - 1.0),
        ])
    }

    pub fn mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(pt) = self.to_view_f(event.position) else {
            return;
        };
        let p = (pt[0] as usize, pt[1] as usize);
        if self.is_erasing() {
            self.set_drag_erase(vec![pt]);
            cx.notify();
            return;
        }
        if event.modifiers.alt {
            self.set_drag_pan(pt, event.position);
            return;
        }
        if let Some(selection) = self.selection() {
            let corners = selection.quad.unwrap_or_else(|| rect_quad(selection.rect));
            for (i, corner) in corners.iter().enumerate() {
                let (sx, sy) = self.to_canvas(*corner);
                let pos = self.canvas_pos(event.position);
                if (sx - pos.0).hypot(sy - pos.1) <= 12.0 {
                    self.set_drag_corner(i);
                    return;
                }
            }
            if selection.rect.contains(p.0, p.1) {
                self.set_drag_move(p, selection.rect);
                return;
            }
        }
        self.set_drag_draw(p);
    }

    pub fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(pt) = self.to_view_f(event.position) else {
            return;
        };
        match self.drag_kind() {
            Some(crate::app::DragKind::Erase) => {
                self.extend_erase(pt);
                cx.notify();
            }
            Some(crate::app::DragKind::Draw) => {
                self.update_draw((pt[0] as usize, pt[1] as usize));
                cx.notify();
            }
            Some(crate::app::DragKind::Move) => {
                self.update_move((pt[0] as usize, pt[1] as usize));
                cx.notify();
            }
            Some(crate::app::DragKind::Corner(i)) => {
                // Queued, not sent per move event: the pump applies the
                // newest at 50 Hz instead of a full command per pixel.
                self.pending_corner = Some((i, (pt[0] as usize, pt[1] as usize)));
                cx.notify();
            }
            Some(crate::app::DragKind::Pan) => {
                self.update_pan(event.position);
                cx.notify();
            }
            None => {}
        }
    }

    pub fn mouse_up(&mut self, _event: &MouseUpEvent, cx: &mut Context<Self>) {
        // Outside the picture there is no fresh point; the gesture still
        // commits from its last known point instead of vanishing.
        match self.take_drag() {
            Some(crate::app::FinishedDrag::Draw { from, to }) => {
                let p = to;
                if let Some(b) = self
                    .view_size()
                    .and_then(|v| Crop::from_corners(from, p).clamped(v.0, v.1))
                {
                    self.cmd(Command::SetSelection { selection: Some(b) }, cx);
                } else if let Some(i) = self.block_at(from) {
                    self.cmd(Command::SelectBlock { index: i }, cx);
                } else {
                    self.cmd(Command::SetSelection { selection: None }, cx);
                }
            }
            Some(crate::app::FinishedDrag::Move { shown }) => {
                self.cmd(
                    Command::SetSelection {
                        selection: Some(shown),
                    },
                    cx,
                );
            }
            Some(crate::app::FinishedDrag::Erase) => {
                let stroke = self.take_erase_stroke();
                if let Some(stroke) = stroke {
                    let mut strokes = self.erasures();
                    self.push_erase_step(strokes.len());
                    strokes.push(stroke);
                    self.cmd(Command::SetErasures { strokes }, cx);
                }
            }
            _ => {}
        }
    }

    pub fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(16.0));
        let steps = -(delta.y.as_f32() / 60.0);
        if event.modifiers.shift {
            self.pan_by(-steps * 0.1, 0.0, cx);
        } else {
            self.zoom_view(steps.clamp(-2.0, 2.0), cx);
        }
    }

    // -- panels ------------------------------------------------------------

    pub fn reading_pane(&mut self, fill: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let reading = self.engine_reading();
        let blocks = self.engine_blocks();
        let models = self.engine_models();
        let speech = self.engine_speech();
        let (pane_bg, _, _) = crate::app::theme_colours(self.config_clone().desktop.theme);
        let mut pane = div()
            .h_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .bg(rgb(pane_bg));
        pane = pane.flex_1().min_h(px(0.0));
        if !fill {
            pane = pane.w(px(400.0));
        }
        pane = pane.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().text_lg().flex_1().child("Reading"))
                .child(
                    btn("reading-max", "Maximise").on_click(cx.listener(|view, _, _, cx| {
                        view.toggle_maximise("reading", cx);
                    })),
                ),
        );
        // Backend + models.
        if reading.backends.is_empty() {
            pane =
                pane.child(div().child(
                    "No way to read is set up. Add a model or a server in the settings file.",
                ));
        } else {
            let name = reading
                .backends
                .get(reading.selected)
                .cloned()
                .unwrap_or_default();
            pane = pane.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .items_center()
                    .child(div().child(format!("Read with: {name}")))
                    .child(
                        btn("backend-next", "Next").on_click(cx.listener(|view, _, _, cx| {
                            let r = view.engine_reading();
                            let next = if r.backends.is_empty() {
                                0
                            } else {
                                (r.selected + 1) % r.backends.len()
                            };
                            view.cmd(Command::SelectBackend { index: next }, cx);
                        })),
                    ),
            );
        }
        for model in &models {
            let phase = model.phase.describe();
            let model_name = model.name.clone();
            let ready = matches!(model.phase, squigl_engine::model::ModelPhase::Ready { .. });
            let mut row = div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("{}: {}", model.name, phase)));
            if ready {
                // Nothing to do.
            } else if matches!(
                model.phase,
                squigl_engine::model::ModelPhase::Downloading { .. }
                    | squigl_engine::model::ModelPhase::Loading { .. }
                    | squigl_engine::model::ModelPhase::Locating
                    | squigl_engine::model::ModelPhase::Verifying
            ) {
                let n = model_name.clone();
                row = row.child(btn("cancel-model", "Cancel").on_click(cx.listener(
                    move |view, _, _, cx| {
                        view.cmd(Command::CancelModelDownload { name: n.clone() }, cx);
                    },
                )));
            } else {
                let n = model_name.clone();
                row = row.child(btn("prepare-model", "Get ready").on_click(cx.listener(
                    move |view, _, _, cx| {
                        view.cmd(Command::PrepareModel { name: n.clone() }, cx);
                    },
                )));
            }
            pane = pane.child(row);
        }
        // Blocks + read controls.
        let busy = reading.reading.is_some();
        let mut controls = div().flex().flex_row().flex_wrap().gap_1();
        controls = controls.child(
            btn(
                "blocks",
                if blocks.enabled {
                    "Blocks on (b)"
                } else {
                    "Find blocks (b)"
                },
            )
            .on_click(cx.listener(|view, _, _, cx| {
                let on = view.engine_blocks().enabled;
                view.cmd(Command::SetBlockMode { on: !on }, cx);
            })),
        );
        controls = controls.child(btn("read", "Read (Enter)").on_click(cx.listener(
            |view, _, _, cx| {
                view.cmd(Command::Read, cx);
            },
        )));
        controls = controls.child(btn("read-all", "Read all (a)").on_click(cx.listener(
            |view, _, _, cx| {
                view.cmd(Command::ReadAll, cx);
            },
        )));
        controls = controls.child(btn("opinion", "Second opinion (o)").on_click(cx.listener(
            |view, _, _, cx| {
                view.cmd(Command::SecondOpinion, cx);
            },
        )));
        if busy {
            controls = controls.child(btn("stop-read", "Stop").on_click(cx.listener(
                |view, _, _, cx| {
                    view.cmd(Command::CancelRead, cx);
                },
            )));
            if let Some(r) = &reading.reading {
                controls = controls.child(div().child(format!("Reading with {}...", r.backend)));
            }
        }
        pane = pane.child(controls);
        // Erase brush.
        let strokes = self.erasures().len();
        let mut erase = div().flex().flex_row().flex_wrap().gap_1().items_center();
        erase = erase.child(
            btn(
                "brush",
                if self.is_erasing() {
                    "Brush on (e)"
                } else {
                    "Erase brush (e)"
                },
            )
            .on_click(cx.listener(|view, _, _, cx| view.toggle_erasing(cx))),
        );
        erase = erase.child(
            btn("brush-minus", "Brush -")
                .on_click(cx.listener(|view, _, _, cx| view.brush_by(-4.0, cx))),
        );
        erase = erase.child(div().child(format!("{:.0}px", self.brush_radius())));
        erase = erase.child(
            btn("brush-plus", "Brush +")
                .on_click(cx.listener(|view, _, _, cx| view.brush_by(4.0, cx))),
        );
        erase = erase.child(
            btn("erase-box", "Erase box (x)")
                .on_click(cx.listener(|view, _, _, cx| view.erase_box(cx))),
        );
        erase = erase.child(
            btn("undo-erase", "Undo (u)")
                .on_click(cx.listener(|view, _, _, cx| view.undo_erase(cx))),
        );
        erase = erase.child(div().child(format!("{strokes} strokes")));
        pane = pane.child(erase);
        // Speech.
        if speech.available {
            let mut say = div().flex().flex_row().flex_wrap().gap_1();
            say = say.child(btn("speak", "Read aloud (s)").on_click(cx.listener(
                |view, _, _, cx| {
                    view.cmd(Command::Speak { result: None }, cx);
                },
            )));
            say = say.child(
                btn("aloud-page", "Read page aloud (A)").on_click(cx.listener(|view, _, _, cx| {
                    view.cmd(Command::ReadAloud, cx);
                })),
            );
            say = say.child(
                btn(
                    "pause-speech",
                    if speech.paused {
                        "Go on (.)"
                    } else {
                        "Pause (.)"
                    },
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    if view.engine_speech().paused {
                        view.cmd(Command::ResumeSpeaking, cx);
                    } else {
                        view.cmd(Command::PauseSpeaking, cx);
                    }
                })),
            );
            say = say.child(btn("stop-speech", "Stop (S)").on_click(cx.listener(
                |view, _, _, cx| {
                    view.cmd(Command::StopSpeaking, cx);
                },
            )));
            say = say.child(btn("prev-spoken", "< Prev").on_click(cx.listener(
                |view, _, _, cx| {
                    view.cmd(Command::SkipSpeech { delta: -1 }, cx);
                },
            )));
            say = say.child(btn("next-spoken", "Next >").on_click(cx.listener(
                |view, _, _, cx| {
                    view.cmd(Command::SkipSpeech { delta: 1 }, cx);
                },
            )));
            pane = pane.child(say);
            let voices = self.engine_voices();
            if !voices.is_empty() {
                let current = self.speech_voice_label();
                pane = pane.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .items_center()
                        .child(div().child(format!("Voice: {current}")))
                        .child(
                            btn("voice-next", "Next voice")
                                .on_click(cx.listener(|view, _, _, cx| view.cycle_voice(cx))),
                        ),
                );
            }
        }
        // Results (newest first unless a page's blocks in order).
        let text_px = self.config_clone().ui.reading_size * 1.5;
        let mut list = div()
            .id("results-list")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.results_scroll_handle());
        let mut items: Vec<usize> = (0..self.results_len()).collect();
        if !reading.in_page_order {
            items.reverse();
        }
        for index in items {
            let Some(detail) = self.result_detail(index) else {
                continue;
            };
            let mut card = div().flex().flex_col().gap_1().p_2().bg(rgb(0x2c2c34));
            if let Some(label) = &detail.label {
                card = card.child(div().text_sm().child(label.clone()));
            }
            if let Some(backend) = &detail.backend {
                card = card.child(
                    div()
                        .text_sm()
                        .child(format!("{backend} - {:.1}s", detail.elapsed_secs)),
                );
            }
            for (r_idx, (text, count, truncated)) in detail.readings.iter().enumerate() {
                if let Some((image, scale, w, h)) = self.typeset_image(index, r_idx) {
                    card = card.child(img(image).w(px(w as f32 / scale)).h(px(h as f32 / scale)));
                } else {
                    card = card.child(self.reading_styled_text(index, r_idx, text_px));
                }
                if *count > 1 {
                    card = card.child(
                        div()
                            .text_sm()
                            .child(format!("Read the same {count} times")),
                    );
                }
                if *truncated {
                    card = card.child(div().text_sm().child("Cut short at the token limit."));
                }
                let _ = text;
            }
            if detail.silent > 0 {
                card = card.child(div().text_sm().child(format!(
                    "{} of {} samples came back empty",
                    detail.silent, detail.samples
                )));
            }
            if let Some(error) = &detail.error {
                card = card.child(div().child(error.clone()));
                card = card.child(div().text_sm().child("Could not read this one."));
            }
            if self.speaking_result() == Some(index) {
                card = card.child(div().text_sm().child("Being read aloud"));
            }
            let text_copy = detail.copy_text();
            card = card.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(
                        btn(format!("copy-result-{index}"), "Copy").on_click(cx.listener(
                            move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text_copy.clone()));
                            },
                        )),
                    )
                    .child(
                        btn(format!("speak-result-{index}"), "Read aloud")
                            .disabled(!speech.available)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.cmd(
                                    Command::Speak {
                                        result: Some(index),
                                    },
                                    cx,
                                );
                            })),
                    ),
            );
            list = list.child(card);
        }
        pane = pane.child(list);
        // History + housekeeping.
        if !self.history_entries().is_empty() {
            pane = pane.child(div().text_base().child("History"));
            for (hi, entry) in self.history_entries().iter().enumerate() {
                let text = entry.clone();
                pane =
                    pane.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .items_center()
                            .child(div().flex_1().child(text.clone()))
                            .child(btn(format!("copy-history-{hi}"), "Copy").on_click(
                                cx.listener(move |_, _, _, cx| {
                                    // Copy without the time/what prefix's trailing text is
                                    // kept simple: copy the whole line.
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                }),
                            )),
                    );
            }
        }
        let mut foot = div().flex().flex_row().flex_wrap().gap_1();
        foot = foot.child(btn("clear", "Clear readings").on_click(cx.listener(
            |view, _, _, cx| {
                view.cmd(Command::ClearResults, cx);
            },
        )));
        foot = foot.child(btn("save-history", "Save readings").on_click(cx.listener(
            |view, _, _, cx| {
                let dir = squigl_engine::paths::pictures_dir();
                view.cmd(Command::SaveHistory { dir }, cx);
            },
        )));
        foot = foot.child(
            btn("copy-all", "Copy all").on_click(cx.listener(|view, _, _, cx| {
                let all = view.all_results_text();
                cx.write_to_clipboard(ClipboardItem::new_string(all));
            })),
        );
        foot = foot.child(div().child(format!("{} in history", self.history_len())));
        pane = pane.child(foot);
        let _ = Selection {
            rect: Crop {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            quad: None,
        };
        pane
    }

    /// A reading as styled text: unsure tokens tinted/underlined, and the
    /// sentence being read aloud highlighted. Shown until (or unless) the
    /// typeset image for the reading is ready.
    fn reading_styled_text(
        &mut self,
        index: usize,
        r_idx: usize,
        text_px: f32,
    ) -> impl IntoElement {
        let detail = self.result_detail(index);
        let Some(reading) = detail
            .as_ref()
            .and_then(|d| d.reading_objs.get(r_idx).cloned())
        else {
            return div().child("");
        };
        let tokens = reading.tokens_if_valid();
        // Token ranges run over the joined token text; use it only when it
        // is the reading's text (tokens_if_valid already guards whitespace).
        let joined: String = tokens
            .iter()
            .flat_map(|t| t.iter().map(|t| t.text.as_str()))
            .collect();
        let (text, tokens) = if !joined.is_empty() && joined == reading.text {
            (reading.text.clone(), tokens)
        } else {
            (reading.text.clone(), None)
        };
        let spoken = (r_idx == 0)
            .then(|| self.engine_speech().speaking)
            .flatten()
            .filter(|sp| sp.result == index)
            .map(|sp| {
                crate::app::utf16_to_byte(&text, sp.start)..crate::app::utf16_to_byte(&text, sp.end)
            });
        let spans = crate::app::highlight_spans(&text, tokens, spoken, &self.config_clone().ui);
        div()
            .text_size(px(text_px))
            .child(gpui_kit::StyledText::new(text).with_highlights(spans))
    }

    pub fn settings_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.config_clone();
        let (panel_bg, _, _) = crate::app::theme_colours(self.config_clone().desktop.theme);
        let mut panel = div()
            .absolute()
            .right(px(8.0))
            .top(px(48.0))
            .w(px(340.0))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(rgb(panel_bg))
            .border(px(1.0))
            .border_color(rgb(0x888888));
        panel = panel.child(div().text_lg().child("Settings"));
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("Colours: {}", mode_label(config.display.mode))))
                .child(
                    btn("set-mode", "Next")
                        .on_click(cx.listener(|view, _, _, cx| view.cycle_mode(cx))),
                ),
        );
        let frac = |kind: crate::app::SliderKind, v: f32| {
            let (lo, hi) = kind.range();
            (v - lo) / (hi - lo)
        };
        panel = panel.child(div().child("Contrast").child(crate::render::slider(
            self,
            crate::app::SliderKind::Contrast,
            frac(crate::app::SliderKind::Contrast, config.display.contrast),
            None,
            cx,
        )));
        panel = panel.child(div().child("Brightness").child(crate::render::slider(
            self,
            crate::app::SliderKind::Brightness,
            frac(
                crate::app::SliderKind::Brightness,
                config.display.brightness,
            ),
            None,
            cx,
        )));
        panel = panel.child(
            div()
                .child("Mid-tones (gamma)")
                .child(crate::render::slider(
                    self,
                    crate::app::SliderKind::Gamma,
                    frac(crate::app::SliderKind::Gamma, config.display.gamma),
                    None,
                    cx,
                )),
        );
        let two_colour = config.display.mode.colours(&config.display).is_some();
        let threshold = config.display.threshold;
        let mut trow = div()
            .flex()
            .flex_row()
            .gap_1()
            .items_center()
            .child(div().child(match threshold {
                Some(t) => format!("Two-colour cut-off at {:.0}%", t * 100.0),
                None => "Two-colour cut-off: off".to_string(),
            }))
            .child(
                btn("threshold", "Toggle")
                    .disabled(!two_colour)
                    .on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.display.threshold = if c.display.threshold.is_some() {
                            None
                        } else {
                            Some(0.5)
                        };
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
            );
        if threshold.is_some() {
            trow =
                trow.child(btn("threshold-minus", "-").disabled(!two_colour).on_click(
                    cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        if let Some(t) = c.display.threshold {
                            c.display.threshold = Some((t - 0.05).clamp(0.05, 0.95));
                        }
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    }),
                ))
                .child(
                    btn("threshold-plus", "+")
                        .disabled(!two_colour)
                        .on_click(cx.listener(|view, _, _, cx| {
                            let mut c = view.config_clone();
                            if let Some(t) = c.display.threshold {
                                c.display.threshold = Some((t + 0.05).clamp(0.05, 0.95));
                            }
                            view.cmd(
                                Command::SetConfig {
                                    config: Box::new(c),
                                },
                                cx,
                            );
                        })),
                );
        }
        if !two_colour {
            trow = trow.child(div().text_sm().child("(two-colour modes only)"));
        }
        panel = panel.child(trow);
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("Theme: {:?}", config.desktop.theme)))
                .child(btn("theme", "Next").on_click(cx.listener(|view, _, _, cx| {
                    let mut c = view.config_clone();
                    c.desktop.theme = match c.desktop.theme {
                        squigl_engine::config::Theme::Dark => squigl_engine::config::Theme::Light,
                        squigl_engine::config::Theme::Light => {
                            squigl_engine::config::Theme::HighContrastYellow
                        }
                        squigl_engine::config::Theme::HighContrastYellow => {
                            squigl_engine::config::Theme::HighContrastWhite
                        }
                        squigl_engine::config::Theme::HighContrastWhite => {
                            squigl_engine::config::Theme::Dark
                        }
                    };
                    view.cmd(
                        Command::SetConfig {
                            config: Box::new(c),
                        },
                        cx,
                    );
                }))),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("Smooth pixels: {}", config.magnifier.smooth)))
                .child(
                    btn("smooth", "Toggle").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.magnifier.smooth = !c.magnifier.smooth;
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("Reading line: {}", config.magnifier.reading_line)))
                .child(
                    btn("rline", "Toggle").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.magnifier.reading_line = !c.magnifier.reading_line;
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!(
                    "Start magnification: {:.1}x",
                    config.magnifier.magnification
                )))
                .child(
                    btn("start-mag-minus", "-").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.magnifier.magnification = (c.magnifier.magnification - 0.5).max(1.0);
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                )
                .child(
                    btn("start-mag-plus", "+").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.magnifier.magnification =
                            (c.magnifier.magnification + 0.5).min(c.magnifier.max_magnification);
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("Reading text: {:.0}pt", config.ui.reading_size)))
                .child(
                    btn("text-minus", "A-")
                        .on_click(cx.listener(|view, _, _, cx| view.reading_size_by(-2.0, cx))),
                )
                .child(
                    btn("text-plus", "A+")
                        .on_click(cx.listener(|view, _, _, cx| view.reading_size_by(2.0, cx))),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!(
                    "Single-key shortcuts: {}",
                    config.desktop.single_key_shortcuts
                )))
                .child(
                    btn("single-key", "Toggle").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.desktop.single_key_shortcuts = !c.desktop.single_key_shortcuts;
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        const PALETTE: [(&str, [u8; 3]); 5] = [
            ("black", [0, 0, 0]),
            ("white", [255, 255, 255]),
            ("yellow", [255, 255, 0]),
            ("red", [200, 30, 30]),
            ("blue", [30, 60, 200]),
        ];
        let ink_name = PALETTE
            .iter()
            .find(|(_, c)| *c == config.display.ink)
            .map_or("custom", |(n, _)| *n);
        let paper_name = PALETTE
            .iter()
            .find(|(_, c)| *c == config.display.paper)
            .map_or("custom", |(n, _)| *n);
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("My colours ink: {ink_name}")))
                .child(
                    btn("ink-next", "Next").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        let at = PALETTE
                            .iter()
                            .position(|(_, col)| *col == c.display.ink)
                            .map_or(0, |i| (i + 1) % PALETTE.len());
                        c.display.ink = PALETTE[at].1;
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("My colours paper: {paper_name}")))
                .child(
                    btn("paper-next", "Next").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        let at = PALETTE
                            .iter()
                            .position(|(_, col)| *col == c.display.paper)
                            .map_or(0, |i| (i + 1) % PALETTE.len());
                        c.display.paper = PALETTE[at].1;
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!(
                    "Pair over Tailscale HTTPS: {}",
                    config.desktop.tailscale_https
                )))
                .child(
                    btn("tailscale", "Toggle").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.desktop.tailscale_https = !c.desktop.tailscale_https;
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!("Voice speed: {:.1}x", config.speech.rate)))
                .child(
                    btn("rate-minus", "-").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.speech.rate = (c.speech.rate - 0.1).clamp(0.5, 2.0);
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                )
                .child(
                    btn("rate-plus", "+").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.speech.rate = (c.speech.rate + 0.1).clamp(0.5, 2.0);
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().child(format!(
                    "Read each new reading aloud: {}",
                    config.speech.speak_new
                )))
                .child(
                    btn("speak-new", "Toggle").on_click(cx.listener(|view, _, _, cx| {
                        let mut c = view.config_clone();
                        c.speech.speak_new = !c.speech.speak_new;
                        view.cmd(
                            Command::SetConfig {
                                config: Box::new(c),
                            },
                            cx,
                        );
                    })),
                ),
        );
        // Shortcut rebinding: pick an action, then press the new key.
        panel = panel.child(
            div()
                .text_base()
                .child(if let Some(a) = &self.rebind_pending() {
                    format!("Press a key for {a} (Esc cancels)")
                } else {
                    "Shortcuts".to_string()
                }),
        );
        for (action, _) in crate::app::ACTIONS {
            let key = crate::app::action_key(&config, action);
            let a = action.to_string();
            panel = panel.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().child(format!("{action}: {key}")))
                    .child(
                        btn(format!("rebind-{action}"), "Rebind").on_click(cx.listener(
                            move |view, _, _, cx| {
                                view.start_rebind(a.clone(), cx);
                            },
                        )),
                    ),
            );
        }
        panel = panel
            .child(div().child("Every setting here is saved to the settings file (gui.toml)."));
        panel = panel.child(
            btn("close-settings", "Close")
                .on_click(cx.listener(|view, _, _, cx| view.toggle_settings(cx))),
        );
        panel
    }

    pub fn pair_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div()
            .absolute()
            .left(relative(0.25))
            .top(relative(0.15))
            .w(relative(0.5))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(rgb(0x24242c))
            .border(px(1.0))
            .border_color(rgb(0x888888));
        panel = panel.child(div().text_lg().child("Pair a phone"));
        panel = panel.child(div().child("On the phone, open the address below in its browser and allow the camera. The picture then shows here; no app to install."));
        if !self.pair_note_text().is_empty() {
            let note = self.pair_note_text();
            panel = panel.child(div().child(note));
        }
        for offer in self.pair_offers() {
            let url = offer.url.clone();
            let mut row = div().flex().flex_col().gap_1()
                .child(div().child(format!("{} {}: {}", if offer.overlay { "Overlay" } else { "Network" }, offer.network, offer.url)))
                .child(div().child(if offer.trusted { "The phone's browser will take the certificate without a warning." } else { "The phone's browser will warn about the certificate once; allow it to continue." }));
            // QR code as an SVG image.
            let id = offer
                .url
                .bytes()
                .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
            row = row.child(
                img(Arc::new(Image {
                    format: ImageFormat::Svg,
                    bytes: offer.qr_svg.clone().into_bytes(),
                    id,
                }))
                .w(px(160.0))
                .h(px(160.0)),
            );
            row = row.child(btn("copy-url", "Copy address").on_click(cx.listener(
                move |_, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                },
            )));
            row = row.child(div().text_sm().child("If the phone is on another network, pair through an overlay (Tailscale, ZeroTier) both are on; Squigl cannot join one for the phone."));
            panel = panel.child(row);
        }
        if self.pair_offers_empty() {
            panel = panel.child(div().child("No network a phone could reach was found."));
        }
        panel = panel.child(
            btn("close-pair", "Close").on_click(cx.listener(|view, _, _, cx| view.toggle_pair(cx))),
        );
        panel
    }
}
