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
}
