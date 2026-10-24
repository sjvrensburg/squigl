//! The Zoom pane (the egui window's centrepiece): the selection shown as a
//! read will see it -- rectified, erased, enhanced, at native pixels -- with
//! the ink-enhancement knobs live over it and paint-to-erase on the crop.

use crate::app::{theme_colours, AppView, SliderKind};
use crate::render::slider;
use gpui_kit::component::button::Button;
use gpui_kit::*;
use squigl_engine::engine::Command;
use squigl_engine::enhance::{Channel, EnhanceMode};
use squigl_engine::geometry::zoom_to_view;

fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).label(label).compact()
}

impl AppView {
    pub fn zoom_pane(&mut self, fill: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.config_clone();
        let (pane_bg, _, _) = theme_colours(config.desktop.theme);
        let selection = self.selection();
        let mut pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap_1()
            .p_2()
            .bg(rgb(pane_bg));
        if !fill {
            pane = pane.w(px(400.0));
        }
        // Header: title, native size, maximise/close.
        let info = match (selection, self.zoom_native) {
            (Some(sel), Some((nw, nh, step))) => {
                let block = self
                    .engine_blocks()
                    .selected
                    .and_then(|i| {
                        self.engine_blocks()
                            .blocks
                            .get(i)
                            .map(|b| format!(" - block {} ({})", i + 1, b.label))
                    })
                    .unwrap_or_default();
                format!(
                    "{nw}x{nh} px at ({}, {}){}{}{block}",
                    sel.rect.x,
                    sel.rect.y,
                    if sel.quad.is_some() {
                        ", rectified"
                    } else {
                        ""
                    },
                    if step > 1 {
                        format!(", shown at 1/{step}")
                    } else {
                        String::new()
                    },
                )
            }
            _ => "Drag a box on the picture to zoom to a region.".to_string(),
        };
        pane = pane.child(div().text_sm().child(info));
        pane = pane.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .items_center()
                .child(div().text_lg().flex_1().child("Zoom"))
                .child(
                    btn("zoom-max", "Max").on_click(cx.listener(|view, _, _, cx| {
                        view.toggle_maximise("zoom", cx);
                    })),
                )
                .child(
                    btn("zoom-detach", "Pop out").on_click(cx.listener(|view, _, _, cx| {
                        view.detach_pane("zoom", cx);
                    })),
                )
                .child(
                    btn("zoom-close", "Close").on_click(cx.listener(|view, _, _, cx| {
                        view.toggle_zoom_pane(cx);
                    })),
                ),
        );
        // Enhancement: mode, ink channel, and the live knobs. Writes go to
        // [enhance] (previewed while dragging), which is what a read uses.
        let enhance = config.enhance;
        let mut erow = div().flex().flex_row().flex_wrap().gap_1().items_center();
        erow = erow.child(div().child(format!("Enhance: {}", enhance.mode.label())));
        erow = erow.child(
            btn("enh-mode", "Next").on_click(cx.listener(|view, _, _, cx| {
                let mut c = view.config_clone();
                c.enhance.mode = c.enhance.mode.cycle();
                view.cmd(
                    Command::SetConfig {
                        config: Box::new(c),
                    },
                    cx,
                );
            })),
        );
        if enhance.mode == EnhanceMode::Ink {
            erow = erow.child(div().child(format!("Ink: {}", enhance.channel.label())));
            erow = erow.child(btn("enh-channel", "Next").on_click(cx.listener(
                |view, _, _, cx| {
                    let mut c = view.config_clone();
                    let at = Channel::ALL
                        .iter()
                        .position(|ch| *ch == c.enhance.channel)
                        .unwrap_or(0);
                    c.enhance.channel = Channel::ALL[(at + 1) % Channel::ALL.len()];
                    view.cmd(
                        Command::SetConfig {
                            config: Box::new(c),
                        },
                        cx,
                    );
                },
            )));
        }
        pane = pane.child(erow);
        if enhance.mode != EnhanceMode::Off {
            let frac = |kind: SliderKind, v: f32| {
                let (lo, hi) = kind.range();
                (v - lo) / (hi - lo)
            };
            pane = pane.child(
                div()
                    .child(format!("Strength {:.2}", enhance.strength))
                    .child(slider(
                        self,
                        SliderKind::Strength,
                        frac(SliderKind::Strength, enhance.strength),
                        None,
                        cx,
                    )),
            );
            pane = pane.child(
                div()
                    .child(format!("Black point {:.0}%", enhance.black_point))
                    .child(slider(
                        self,
                        SliderKind::BlackPoint,
                        frac(SliderKind::BlackPoint, enhance.black_point),
                        None,
                        cx,
                    )),
            );
            pane = pane.child(
                div()
                    .child(format!("White point {:.0}%", enhance.white_point))
                    .child(slider(
                        self,
                        SliderKind::WhitePoint,
                        frac(SliderKind::WhitePoint, enhance.white_point),
                        None,
                        cx,
                    )),
            );
            pane = pane.child(
                div()
                    .child(format!("Gamma {:.2}", enhance.gamma))
                    .child(slider(
                        self,
                        SliderKind::EnhGamma,
                        frac(SliderKind::EnhGamma, enhance.gamma),
                        None,
                        cx,
                    )),
            );
        }
        pane = pane.child(self.zoom_picture(cx));
        pane = pane.child(div().text_sm().child(
            "Wheel here resizes the box. With the erase brush on, paint here to erase under the crop (Ctrl+Z undoes).",
        ));
        pane
    }

    fn zoom_picture(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let slot = std::sync::Arc::clone(&self.zoom_canvas);
        let mut area = div()
            .relative()
            .flex_1()
            .min_h(px(120.0))
            .bg(rgb(0x000000))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, _, cx| view.zoom_mouse_down(event, cx)),
            )
            .on_mouse_move(
                cx.listener(|view, event: &MouseMoveEvent, _, cx| view.zoom_mouse_move(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, event: &MouseUpEvent, _, cx| view.zoom_mouse_up(event, cx)),
            )
            .on_scroll_wheel(
                cx.listener(|view, event: &ScrollWheelEvent, _, cx| view.zoom_scroll(event, cx)),
            );
        area = area.child(
            canvas(
                move |bounds, _, _| {
                    *slot.lock().unwrap() = Some((
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
        if let Some(image) = self.zoom_image.clone() {
            if let Some((dw, dh)) = self.zoom_fit() {
                self.zoom_display = Some((dw, dh));
                area = area.child(
                    img(image)
                        .absolute()
                        .left(px(0.0))
                        .top(px(0.0))
                        .w(px(dw))
                        .h(px(dh)),
                );
            }
        }
        // The stroke being painted, in image fractions mapped to the display.
        if self.drag_is_erase() {
            if let (Some(sel), Some((dw, dh))) = (self.selection(), self.zoom_display) {
                let _ = (dw, dh);
                for pt in self.erase_points_view() {
                    // View point back to an image fraction via the rect.
                    let fx = (pt[0] - sel.rect.x as f32) / sel.rect.w.max(1) as f32;
                    let fy = (pt[1] - sel.rect.y as f32) / sel.rect.h.max(1) as f32;
                    area = area.child(
                        div()
                            .absolute()
                            .left(relative(fx.clamp(0.0, 1.0)))
                            .top(relative(fy.clamp(0.0, 1.0)))
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(rgb(0xff5a5a)),
                    );
                }
            }
        }
        area
    }

    /// The zoom image's fitted size in logical px within its canvas.
    pub fn zoom_fit(&self) -> Option<(f32, f32)> {
        let (_, _, cw, ch) = (*self.zoom_canvas.lock().unwrap())?;
        let (nw, nh, _) = self.zoom_native?;
        if cw <= 0.0 || ch <= 0.0 || nw == 0 || nh == 0 {
            return None;
        }
        let scale = (cw / nw as f32).min(ch / nh as f32);
        Some((nw as f32 * scale, nh as f32 * scale))
    }

    fn zoom_frac(&self, pos: Point<Pixels>) -> Option<[f32; 2]> {
        let (ox, oy, _, _) = (*self.zoom_canvas.lock().unwrap())?;
        let (dw, dh) = self.zoom_display?;
        Some([
            ((pos.x.as_f32() - ox) / dw.max(1.0)).clamp(0.0, 1.0),
            ((pos.y.as_f32() - oy) / dh.max(1.0)).clamp(0.0, 1.0),
        ])
    }

    pub fn zoom_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if !self.is_erasing() {
            return;
        }
        let Some(sel) = self.selection() else { return };
        let Some(frac) = self.zoom_frac(event.position) else {
            return;
        };
        if let Some(pt) = zoom_to_view(sel, frac) {
            self.set_drag_erase(vec![pt]);
            cx.notify();
        }
    }

    pub fn zoom_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.drag_is_erase() {
            return;
        }
        let Some(sel) = self.selection() else { return };
        let Some(frac) = self.zoom_frac(event.position) else {
            return;
        };
        if let Some(pt) = zoom_to_view(sel, frac) {
            self.extend_erase(pt);
            cx.notify();
        }
    }

    pub fn zoom_mouse_up(&mut self, _event: &MouseUpEvent, cx: &mut Context<Self>) {
        if !self.drag_is_erase() {
            return;
        }
        self.clear_drag_kind();
        if let Some(mut stroke) = self.take_erase_stroke() {
            // Brush radius: screen px on the zoom image to view px.
            if let (Some(sel), Some((dw, _))) = (self.selection(), self.zoom_display) {
                stroke.radius = self.brush_radius() * (sel.rect.w as f32 / dw.max(1.0));
            }
            let mut strokes = self.erasures();
            self.push_erase_step(strokes.len());
            strokes.push(stroke);
            self.cmd(Command::SetErasures { strokes }, cx);
        }
    }

    pub fn zoom_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        // Wheel over the zoomed crop grows/shrinks the box about its centre.
        let Some(sel) = self.selection() else { return };
        let Some(view) = self.view_size() else { return };
        let delta = event.delta.pixel_delta(px(16.0)).y.as_f32();
        if delta.abs() < 4.0 {
            return;
        }
        let factor = if delta < 0.0 { 1.1 } else { 1.0 / 1.1 };
        let scaled = sel.rect.scaled(factor, view.0, view.1);
        self.cmd(
            Command::SetSelection {
                selection: Some(scaled),
            },
            cx,
        );
    }
}
