//! Turning the engine's raw planes into the GPU image GPUI draws.
//!
//! The engine already does the expensive reduction in Rust: `render_planes`
//! crops to the viewport's region and decimates by an integer step, so at most
//! ~1 pixel per device pixel crosses to the front end (the same contract the
//! Tauri WebGL renderer used). What remains here per frame is two CPU passes
//! over those screen-sized pixels (YUV->RGB with the display LUT, then a fused
//! rotate-and-BGRA-swap) and one texture upload; GPUI (Vulkan on Linux) does
//! scaling, placement and compositing of every control on the GPU. Rotation
//! happens before upload because GPUI images have no per-draw rotation.
//! Callers must `Window::drop_image` the replaced image (see `app.rs`):
//! a `RenderImage` otherwise keeps its sprite-atlas tiles for the app's life.

use gpui_kit::RenderImage;
use image::{Frame, ImageBuffer};
use smallvec::SmallVec;
use squigl_core::convert::Planes;
use squigl_engine::display::Lut;
use squigl_engine::view::ViewPlanes;
use std::sync::Arc;

/// What identifies a converted frame, so an unchanged one is not reconverted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameKey {
    pub seq: u64,
    pub region: (usize, usize, usize, usize),
    pub step: usize,
    pub rotation: squigl_core::convert::Rotation,
    pub lut: u64,
    /// A hash of the capture's erasure strokes (they are painted into the
    /// planes, so an erasure-only change must not reuse the old picture).
    pub erasures: u64,
    pub smooth: bool,
}

impl FrameKey {
    pub fn of(planes: &ViewPlanes, lut: u64, erasures: u64, smooth: bool) -> Self {
        let h = &planes.header;
        Self {
            seq: h.seq,
            region: (h.region.x, h.region.y, h.region.w, h.region.h),
            step: h.step,
            rotation: h.rotation,
            lut,
            erasures,
            smooth,
        }
    }
}

/// A cheap hash of erasure strokes, for [`FrameKey`].
pub fn erasures_key(strokes: &[squigl_engine::erase::Stroke]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    let mut mix = |v: u64| hash = (hash ^ v).wrapping_mul(0x100000001b3);
    mix(strokes.len() as u64);
    for s in strokes {
        mix(s.points.len() as u64);
        mix(s.radius.to_bits() as u64);
        for pt in &s.points {
            mix(pt[0].to_bits() as u64);
            mix(pt[1].to_bits() as u64);
        }
    }
    hash
}

/// A cheap hash of a LUT, for [`FrameKey`]: the table's own bytes.
pub fn lut_key(lut: &Lut) -> u64 {
    let bytes = lut.to_rgba();
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}

/// Planes (in the frame's own orientation) to RGBA in view orientation, with
/// the display LUT applied. BT.601 limited range, the coefficients of
/// `squigl_core::convert` (and of the Tauri WebGL shader this replaces).
pub fn planes_to_rgba(planes: &Planes, lut: &Lut, smooth: bool) -> (Vec<u8>, usize, usize) {
    let (w, h) = (planes.width, planes.height);
    let table = lut.to_rgba();
    let luma_mode = matches!(lut, Lut::Luma(_));
    let mut out = vec![0u8; w * h * 4];
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let yv = planes.y[i];
            let o = &mut out[i * 4..i * 4 + 4];
            o[3] = 255;
            if luma_mode {
                o[..3].copy_from_slice(&table[usize::from(yv) * 4..][..3]);
            } else if planes.u.is_empty() {
                // Luma-only in a tone mode: the grey the BT.601 expansion
                // gives, then the table (raw Y would keep blacks at 16).
                let grey = ((298 * (i32::from(yv) - 16) + 128) >> 8).clamp(0, 255) as u8;
                let t = table[usize::from(grey) * 4];
                o[..3].copy_from_slice(&[t, t, t]);
            } else if smooth {
                // Bilinear chroma, as the Tauri renderers' texture sampling.
                let (u, v) = chroma_bilinear(planes, x, y, cw, ch);
                let c = 298 * (i32::from(yv) - 16) + 128;
                let d = i32::from(u) - 128;
                let e = i32::from(v) - 128;
                let clamp = |z: i32| (z >> 8).clamp(0, 255) as u8;
                let (r, g, b) = (
                    clamp(c + 409 * e),
                    clamp(c - 100 * d - 208 * e),
                    clamp(c + 516 * d),
                );
                o[0] = table[usize::from(r) * 4];
                o[1] = table[usize::from(g) * 4 + 1];
                o[2] = table[usize::from(b) * 4 + 2];
            } else {
                let ci = (y / 2).min(ch - 1) * cw + (x / 2).min(cw - 1);
                // squigl_core::convert::yuv_to_rgb, verbatim.
                let c = 298 * (i32::from(yv) - 16) + 128;
                let d = i32::from(planes.u[ci]) - 128;
                let e = i32::from(planes.v[ci]) - 128;
                let clamp = |v: i32| (v >> 8).clamp(0, 255) as u8;
                let (r, g, b) = (
                    clamp(c + 409 * e),
                    clamp(c - 100 * d - 208 * e),
                    clamp(c + 516 * d),
                );
                o[0] = table[usize::from(r) * 4];
                o[1] = table[usize::from(g) * 4 + 1];
                o[2] = table[usize::from(b) * 4 + 2];
            }
        }
    }
    (out, w, h)
}

/// Bilinear chroma at luma pixel (`x`, `y`): chroma covers 2x2 luma blocks,
/// sampled at their centres, as a GPU sampling the half-size planes would.
fn chroma_bilinear(planes: &Planes, x: usize, y: usize, cw: usize, ch: usize) -> (u8, u8) {
    let sample = |plane: &[u8]| -> u8 {
        let fx = (x as f32 + 0.5) / 2.0 - 0.5;
        let fy = (y as f32 + 0.5) / 2.0 - 0.5;
        let x0 = fx.floor().max(0.0) as usize;
        let y0 = fy.floor().max(0.0) as usize;
        let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
        let (tx, ty) = (
            (fx - x0 as f32).clamp(0.0, 1.0),
            (fy - y0 as f32).clamp(0.0, 1.0),
        );
        let at = |cx: usize, cy: usize| f32::from(plane[cy * cw + cx]);
        let top = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
        let bot = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
        (top * (1.0 - ty) + bot * ty).round() as u8
    };
    (sample(&planes.u), sample(&planes.v))
}

/// The sampled planes as a GPUI image in view orientation (BGRA, as
/// [`RenderImage`] stores it). Rotation and the BGRA swap are one pass.
pub fn to_render_image(view: &ViewPlanes, lut: &Lut, smooth: bool) -> Arc<RenderImage> {
    let (rgba, w, h) = planes_to_rgba(&view.planes, lut, smooth);
    let rotation = view.header.rotation;
    let (ow, oh) = rotation.rotated_size(w, h);
    let mut bgra = vec![0u8; ow * oh * 4];
    for oy in 0..oh {
        for ox in 0..ow {
            let (sx, sy) = match rotation {
                squigl_core::convert::Rotation::None => (ox, oy),
                squigl_core::convert::Rotation::Cw90 => (oy, h - 1 - ox),
                squigl_core::convert::Rotation::Cw180 => (w - 1 - ox, h - 1 - oy),
                squigl_core::convert::Rotation::Cw270 => (w - 1 - oy, ox),
            };
            let src = &rgba[(sy * w + sx) * 4..][..4];
            let dst = &mut bgra[(oy * ow + ox) * 4..][..4];
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = 255;
        }
    }
    let buffer = ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(ow as u32, oh as u32, bgra)
        .expect("RGBA buffer matches the planes' size");
    let mut frames = SmallVec::<[Frame; 1]>::new();
    frames.push(Frame::new(buffer));
    Arc::new(RenderImage::new(frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    use squigl_engine::display::{DisplayConfig, DisplayMode};

    fn planes(y: Vec<u8>, u: Vec<u8>, v: Vec<u8>, w: usize, h: usize) -> Planes {
        Planes {
            width: w,
            height: h,
            y,
            u,
            v,
        }
    }

    #[test]
    fn luma_mode_maps_the_y_byte_through_the_table() {
        let lut = squigl_engine::display::lut(&DisplayConfig {
            mode: DisplayMode::YellowOnBlack,
            ..Default::default()
        });
        let (rgba, w, h) = planes_to_rgba(&planes(vec![16, 235], vec![], vec![], 2, 1), &lut, true);
        assert_eq!((w, h), (2, 1));
        // Yellow on black: dark strokes are the yellow ink, bright ground black.
        assert_eq!(&rgba[0..3], &[255, 255, 0]);
        assert_eq!(&rgba[4..7], &[0, 0, 0]);
    }

    #[test]
    fn luma_only_tone_expands_limited_range_like_convert() {
        let lut = squigl_engine::display::lut(&DisplayConfig::default());
        let (rgba, _, _) = planes_to_rgba(&planes(vec![16, 235], vec![], vec![], 2, 1), &lut, true);
        assert_eq!(&rgba[0..3], &[0, 0, 0], "limited-range black expands to 0");
        assert_eq!(
            &rgba[4..7],
            &[255, 255, 255],
            "limited-range white expands to 255"
        );
    }

    #[test]
    fn smooth_chroma_blends_neighbours_and_nearest_does_not() {
        // Left half red-ish chroma, right half blue-ish, neutral luma.
        let p = planes(
            vec![128; 16],
            vec![90, 200, 90, 200],
            vec![240, 90, 240, 90],
            4,
            4,
        );
        let lut = squigl_engine::display::lut(&DisplayConfig::default());
        let (near, _, _) = planes_to_rgba(&p, &lut, false);
        let (smooth, _, _) = planes_to_rgba(&p, &lut, true);
        let px = |b: &[u8], x: usize| i32::from(b[x * 4]);
        // Red follows V (240 left, 90 right): at the boundary nearest jumps
        // the full step, smooth lands in between on both sides.
        assert!(
            px(&smooth, 1) < px(&near, 1),
            "left of the edge blends down"
        );
        assert!(px(&smooth, 2) > px(&near, 2), "right of the edge blends up");
        assert!(px(&smooth, 1) > px(&smooth, 2));
    }

    #[test]
    fn neutral_chroma_is_grey_and_matches_convert() {
        let lut = squigl_engine::display::lut(&DisplayConfig::default());
        let p = planes(vec![128; 4], vec![128], vec![128], 2, 2);
        let (rgba, _, _) = planes_to_rgba(&p, &lut, false);
        let mut want = vec![0u8; 16];
        let frame = squigl_core::decode::YuvFrame {
            width: 2,
            height: 2,
            y: vec![128; 4],
            u: vec![128],
            v: vec![128],
        };
        squigl_core::convert::i420_to_rgba(&frame, &mut want);
        for i in 0..4 {
            assert!(
                (i32::from(rgba[i * 4]) - i32::from(want[i * 4])).abs() <= 1,
                "pixel {i}: {} vs {}",
                rgba[i * 4],
                want[i * 4]
            );
        }
    }
}
