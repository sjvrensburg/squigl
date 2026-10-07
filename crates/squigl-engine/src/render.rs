//! Turning a decoded frame into the RGBA pixels of a view-space region: rotated as
//! shown, erased, rectified and enhanced, so a read, a save and every view of the
//! same selection agree.

use crate::enhance::{self, EnhanceConfig, EnhanceMode};
use crate::erase;
use crate::geometry::{Crop, Selection};
use crate::layout;
use squigl_core::convert::{i420_region_to_rgba, region_size, rotate_rgba, Rotation};
use squigl_core::decode::YuvFrame;

/// Fetches a view-space region of `frame` as rotated RGBA, every `step`-th pixel.
/// Returns the pixels and their size.
pub fn render_region(
    frame: &YuvFrame,
    rotation: Rotation,
    region: Crop,
    step: usize,
) -> (Vec<u8>, usize, usize) {
    let (vw, vh) = rotation.rotated_size(frame.width, frame.height);
    let src = region.to_source(rotation, vw, vh);
    let (sw, sh) = region_size(src.w, src.h, step);
    let mut buf = vec![0u8; sw * sh * 4];
    i420_region_to_rgba(frame, src.x, src.y, src.w, src.h, step, &mut buf);
    let (ow, oh) = rotation.rotated_size(sw, sh);
    (rotate_rgba(&buf, sw, sh, rotation), ow, oh)
}

/// The pixels of a selection (or the whole view when there is none) at every
/// `step`-th pixel, rotated as shown, with the `erased` regions (view space) painted
/// over and, for a quad, rectified. `enhance`, when given, is applied last -- this is
/// the crop the model reads and/or the crop panel shows; the preview passes none and
/// live block detection calls [`render_region`] directly, raw. Returns the pixels and
/// their size.
pub fn render_selection(
    frame: &YuvFrame,
    rotation: Rotation,
    selection: Option<Selection>,
    step: usize,
    erased: &[erase::Stroke],
    enhance: Option<&EnhanceConfig>,
) -> (Vec<u8>, usize, usize) {
    let (vw, vh) = rotation.rotated_size(frame.width, frame.height);
    let region = selection.map_or(Crop::whole(vw, vh), |sel| sel.rect);
    let (mut rgba, w, h) = render_region(frame, rotation, region, step);
    if !erased.is_empty() {
        let mut img =
            image::RgbaImage::from_raw(w as u32, h as u32, rgba).expect("buffer matches size");
        erase::erase(
            &mut img,
            &erase::to_region(erased, (region.x, region.y), step),
        );
        rgba = img.into_raw();
    }
    let (rgba, w, h) = match selection.and_then(|sel| sel.quad) {
        None => (rgba, w, h),
        Some(quad) => {
            // The quad in the rendered region's own pixels.
            let local = quad.map(|[x, y]| {
                [
                    (x - region.x as f32) / step as f32,
                    (y - region.y as f32) / step as f32,
                ]
            });
            let img =
                image::RgbaImage::from_raw(w as u32, h as u32, rgba).expect("buffer matches size");
            match layout::rectify(&img, &local) {
                Some(out) => {
                    let (ow, oh) = (out.width() as usize, out.height() as usize);
                    (out.into_raw(), ow, oh)
                }
                None => (img.into_raw(), w, h),
            }
        }
    };
    match enhance {
        Some(cfg) if cfg.mode != EnhanceMode::Off => {
            let img =
                image::RgbaImage::from_raw(w as u32, h as u32, rgba).expect("buffer matches size");
            let out = enhance::apply(&img, cfg);
            (out.into_raw(), w, h)
        }
        _ => (rgba, w, h),
    }
}

/// A view-space point in the source frame's continuous coordinates (the inverse of
/// turning by `rotation`, as [`Crop::to_source`] is for pixels).
fn point_to_source(rotation: Rotation, view_w: usize, view_h: usize, [x, y]: [f32; 2]) -> [f32; 2] {
    let (vw, vh) = (view_w as f32, view_h as f32);
    match rotation {
        Rotation::None => [x, y],
        Rotation::Cw90 => [y, vw - x],
        Rotation::Cw180 => [vw - x, vh - y],
        Rotation::Cw270 => [vh - y, x],
    }
}

/// `frame` with `strokes` (view space) painted over, for showing: the pixels an
/// erasure leaves alone are the frame's own, bit for bit, so only what was painted
/// changes. The fill is taken from the same ring as a read's whole-view rendering.
pub fn erased_frame(frame: &YuvFrame, rotation: Rotation, strokes: &[erase::Stroke]) -> YuvFrame {
    let mut out = frame.clone();
    let (vw, vh) = rotation.rotated_size(frame.width, frame.height);
    let source: Vec<erase::Stroke> = strokes
        .iter()
        .map(|s| erase::Stroke {
            points: s
                .points
                .iter()
                .map(|&p| point_to_source(rotation, vw, vh, p))
                .collect(),
            radius: s.radius,
        })
        .collect();
    // Everything the strokes and their rings touch, on even pixels (the chroma's).
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for s in &source {
        let reach = s.radius + erase::RING_PX + 1.0;
        for p in &s.points {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k] - reach);
                hi[k] = hi[k].max(p[k] + reach);
            }
        }
    }
    let even_down = |v: f32, max: usize| ((v.max(0.0) as usize) & !1).min(max);
    let even_up = |v: f32, max: usize| ((v.max(0.0).ceil() as usize + 1) & !1).min(max);
    let (x0, y0) = (
        even_down(lo[0], frame.width),
        even_down(lo[1], frame.height),
    );
    let (x1, y1) = (even_up(hi[0], frame.width), even_up(hi[1], frame.height));
    if x0 >= x1 || y0 >= y1 {
        return out;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut rgba = vec![0u8; w * h * 4];
    i420_region_to_rgba(frame, x0, y0, w, h, 1, &mut rgba);
    let mut img =
        image::RgbaImage::from_raw(w as u32, h as u32, rgba.clone()).expect("buffer matches size");
    erase::erase(&mut img, &erase::to_region(&source, (x0, y0), 1));
    let painted = img.into_raw();
    let patch = squigl_core::convert::rgba_to_i420(&painted, w, h);
    let changed = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        rgba[i..i + 3] != painted[i..i + 3]
    };
    let cw = frame.width / 2;
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let cells = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| (bx * 2 + dx, by * 2 + dy));
            let mut any = false;
            for (x, y) in cells {
                if changed(x, y) {
                    out.y[(y0 + y) * frame.width + x0 + x] = patch.y[y * w + x];
                    any = true;
                }
            }
            if any {
                let c = (y0 / 2 + by) * cw + x0 / 2 + bx;
                out.u[c] = patch.u[by * (w / 2) + bx];
                out.v[c] = patch.v[by * (w / 2) + bx];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 6x4 source frame with luma = 16 + x + 10 y, so every pixel is identifiable.
    fn frame() -> YuvFrame {
        let (w, h) = (6, 4);
        let y = (0..h)
            .flat_map(|row| (0..w).map(move |col| (16 + col + 10 * row) as u8))
            .collect();
        YuvFrame {
            width: w,
            height: h,
            y,
            u: vec![128; 6],
            v: vec![128; 6],
        }
    }

    /// The red channel of a rendered gray pixel: monotonic in the source luma, so it
    /// identifies the source pixel.
    fn at(rgba: &[u8], w: usize, x: usize, y: usize) -> u8 {
        rgba[(y * w + x) * 4]
    }

    #[test]
    fn crop_maps_back_to_source_under_every_rotation() {
        let f = frame();
        // A 2x1 view-space region; whichever way the frame is turned, rendering it
        // must equal cutting it out of the rotated whole view.
        for rotation in [
            Rotation::None,
            Rotation::Cw90,
            Rotation::Cw180,
            Rotation::Cw270,
        ] {
            let (vw, vh) = rotation.rotated_size(f.width, f.height);
            let (whole, ww, _) = render_region(&f, rotation, Crop::whole(vw, vh), 1);
            let region = Crop {
                x: 1,
                y: 1,
                w: 2,
                h: 1,
            };
            let (part, pw, ph) = render_region(&f, rotation, region, 1);
            assert_eq!((pw, ph), (2, 1), "{rotation:?}");
            for x in 0..2 {
                assert_eq!(
                    at(&part, pw, x, 0),
                    at(&whole, ww, region.x + x, region.y),
                    "{rotation:?} pixel {x}"
                );
            }
        }
    }

    #[test]
    fn rotated_whole_view_has_the_source_corner_where_expected() {
        let f = frame();
        let (v, w, _) = render_region(&f, Rotation::None, Crop::whole(6, 4), 1);
        let src_top_left = at(&v, w, 0, 0);
        // Clockwise: the source's top-left corner is the view's top-right.
        let (v, w, h) = render_region(&f, Rotation::Cw90, Crop::whole(4, 6), 1);
        assert_eq!((w, h), (4, 6));
        assert_eq!(at(&v, w, 3, 0), src_top_left);
        // Counter-clockwise: bottom-left.
        let (v, w, _) = render_region(&f, Rotation::Cw270, Crop::whole(4, 6), 1);
        assert_eq!(at(&v, w, 0, 5), src_top_left);
    }

    #[test]
    fn an_erased_region_is_flat_in_every_render_and_the_rest_is_untouched() {
        let f = frame();
        // Covers pixel centres (1..3, 1..3) and nothing else.
        let erased = erase::Stroke::line([2.0, 1.5], [2.0, 2.5], 0.75);
        let (raw, w, _) = render_selection(&f, Rotation::None, None, 1, &[], None);
        let (out, _, _) = render_selection(
            &f,
            Rotation::None,
            None,
            1,
            std::slice::from_ref(&erased),
            None,
        );
        let inside = |x: usize, y: usize| (1..3).contains(&x) && (1..3).contains(&y);
        let fill = at(&out, w, 1, 1);
        for y in 0..4 {
            for x in 0..6 {
                if inside(x, y) {
                    assert_eq!(at(&out, w, x, y), fill, "({x}, {y}) inside");
                } else {
                    assert_eq!(at(&out, w, x, y), at(&raw, w, x, y), "({x}, {y}) outside");
                }
            }
        }
        // A selection over part of it sees the erasure in its own pixels: flat, though
        // its fill is sampled from the ring as far as this render reaches.
        let sel = Selection {
            rect: Crop {
                x: 2,
                y: 0,
                w: 4,
                h: 4,
            },
            quad: None,
        };
        let (part, pw, _) = render_selection(&f, Rotation::None, Some(sel), 1, &[erased], None);
        assert_eq!(at(&part, pw, 0, 1), at(&part, pw, 0, 2));
        assert_ne!(at(&part, pw, 0, 2), at(&raw, w, 2, 2));
        assert_eq!(at(&part, pw, 1, 2), at(&raw, w, 3, 2));
    }

    #[test]
    fn an_erased_frame_shows_what_a_read_sees() {
        // Paper (luma 200) with a dark blot at source (10..14, 6..10).
        let (w, h) = (40, 24);
        let y = (0..h)
            .flat_map(|row| {
                (0..w).map(move |col| {
                    if (10..14).contains(&col) && (6..10).contains(&row) {
                        30
                    } else {
                        200
                    }
                })
            })
            .collect();
        let f = YuvFrame {
            width: w,
            height: h,
            y,
            u: vec![128; w * h / 4],
            v: vec![128; w * h / 4],
        };
        for rotation in [
            Rotation::None,
            Rotation::Cw90,
            Rotation::Cw180,
            Rotation::Cw270,
        ] {
            let (vw, vh) = rotation.rotated_size(w, h);
            // The blot's centre in view space: the view pixel whose source is (12, 8).
            let centre = (0..vw * vh)
                .map(|i| (i % vw, i / vw))
                .find(|&(x, y)| {
                    let s = Crop { x, y, w: 1, h: 1 }.to_source(rotation, vw, vh);
                    (s.x, s.y) == (12, 8)
                })
                .unwrap();
            let c = [centre.0 as f32, centre.1 as f32];
            let stroke = erase::Stroke::line(c, [c[0] + 0.5, c[1]], 4.0);
            let erased = erased_frame(&f, rotation, std::slice::from_ref(&stroke));
            let (shown, _, _) = render_region(&erased, rotation, Crop::whole(vw, vh), 1);
            let (read, _, _) = render_selection(&f, rotation, None, 1, &[stroke], None);
            let (raw, _, _) = render_region(&f, rotation, Crop::whole(vw, vh), 1);
            for i in 0..vw * vh {
                let px = |b: &[u8]| b[i * 4] as i32;
                assert!(
                    (px(&shown) - px(&read)).abs() <= 2,
                    "{rotation:?} pixel {i}"
                );
                if px(&read) == px(&raw) {
                    assert_eq!(
                        px(&shown),
                        px(&raw),
                        "{rotation:?}: an untouched pixel moved"
                    );
                }
            }
            assert!(
                at(&shown, vw, centre.0, centre.1) > 200,
                "{rotation:?}: the blot stayed"
            );
        }
    }
}
