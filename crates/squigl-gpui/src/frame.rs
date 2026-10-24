//! Turning the engine's raw planes into the GPU image GPUI draws.
//!
//! The engine already does the expensive reduction in Rust: `render_planes`
//! crops to the viewport's region and decimates by an integer step, so at most
//! ~1 pixel per device pixel crosses to the front end (the same contract the
//! Tauri WebGL renderer used). What remains here per frame is YUV->RGB for
//! those screen-sized pixels plus the display mode's 256-entry LUT, then one
//! texture upload; GPUI (Vulkan on Linux) does scaling, placement and
//! compositing of every control on the GPU. Rotation is applied to the RGBA
//! before upload because GPUI images have no per-draw rotation; it is a
//! memcpy-shaped permutation of an already screen-sized buffer.

use gpui_kit::RenderImage;
use image::{Frame, ImageBuffer};
use smallvec::SmallVec;
use squigl_core::convert::{rotate_rgba, Planes};
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
}

impl FrameKey {
    pub fn of(planes: &ViewPlanes, lut: u64) -> Self {
        let h = &planes.header;
        Self {
            seq: h.seq,
            region: (h.region.x, h.region.y, h.region.w, h.region.h),
            step: h.step,
            rotation: h.rotation,
            lut,
        }
    }
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
pub fn planes_to_rgba(planes: &Planes, lut: &Lut) -> (Vec<u8>, usize, usize) {
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
                let t = table[usize::from(yv) * 4];
                o[..3].copy_from_slice(&[t, t, t]);
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

/// The sampled planes as a GPUI image in view orientation (BGRA, as
/// [`RenderImage`] stores it).
pub fn to_render_image(view: &ViewPlanes, lut: &Lut) -> Arc<RenderImage> {
    let (rgba, w, h) = planes_to_rgba(&view.planes, lut);
    let (rotated, ow, oh) = if view.header.rotation == squigl_core::convert::Rotation::None {
        (rgba, w, h)
    } else {
        let (ow, oh) = view.header.rotation.rotated_size(w, h);
        (rotate_rgba(&rgba, w, h, view.header.rotation), ow, oh)
    };
    let mut bgra = rotated;
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
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
        let (rgba, w, h) = planes_to_rgba(&planes(vec![16, 235], vec![], vec![], 2, 1), &lut);
        assert_eq!((w, h), (2, 1));
        // Yellow on black: dark strokes are the yellow ink, bright ground black.
        assert_eq!(&rgba[0..3], &[255, 255, 0]);
        assert_eq!(&rgba[4..7], &[0, 0, 0]);
    }

    #[test]
    fn neutral_chroma_is_grey_and_matches_convert() {
        let lut = squigl_engine::display::lut(&DisplayConfig::default());
        let p = planes(vec![128; 4], vec![128], vec![128], 2, 2);
        let (rgba, _, _) = planes_to_rgba(&p, &lut);
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
