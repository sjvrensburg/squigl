//! Pixel-format conversions from the decoder's I420 (planar YUV 4:2:0).
//!
//! - [`i420_to_yuyv`]: packed YUYV422 for the V4L2 sink, because it's the most broadly
//!   compatible "raw webcam" format for consumers (browsers, most webcam apps) -- the
//!   real scrcpy V4L2 sink does the same conversion for the same reason.
//! - [`i420_to_rgba`], [`i420_crop_to_rgba`], [`i420_to_rgba_decimated`] (and the general
//!   [`i420_region_to_rgba`]): packed RGBA8 for on-screen display (a GUI texture), whole,
//!   a region at native pixels, or every n-th pixel for a cheap preview of a large frame.
//! - [`rgba_to_i420`]: the other way, for a still image shown as a camera frame.
//! - [`i420_region_planes`]: a region at a step as raw planes, unconverted, for a
//!   front end that converts on the GPU.

use crate::decode::YuvFrame;

/// Converts `frame` into `out` as packed YUYV422 (byte order Y0 U0 Y1 V0 per pixel
/// pair). `out` must be exactly `width * height * 2` bytes.
pub fn i420_to_yuyv(frame: &YuvFrame, out: &mut [u8]) {
    let (w, h) = (frame.width, frame.height);
    debug_assert_eq!(out.len(), w * h * 2);

    for row in 0..h {
        let y_row = &frame.y[row * w..row * w + w];
        let uv_row = row / 2;
        let uv_w = w / 2;
        let u_row = &frame.u[uv_row * uv_w..uv_row * uv_w + uv_w];
        let v_row = &frame.v[uv_row * uv_w..uv_row * uv_w + uv_w];

        let out_row = &mut out[row * w * 2..row * w * 2 + w * 2];
        for pair in 0..uv_w {
            let y0 = y_row[pair * 2];
            let y1 = y_row[pair * 2 + 1];
            let u = u_row[pair];
            let v = v_row[pair];
            let o = &mut out_row[pair * 4..pair * 4 + 4];
            o[0] = y0;
            o[1] = u;
            o[2] = y1;
            o[3] = v;
        }
    }
}

/// Converts the whole frame into `out` as packed RGBA8 (alpha 255). `out` must be
/// exactly `width * height * 4` bytes.
pub fn i420_to_rgba(frame: &YuvFrame, out: &mut [u8]) {
    convert_rgba(frame, 0, 0, frame.width, frame.height, 1, out);
}

/// Converts the `w`x`h` region whose top-left corner is (`x`, `y`) into `out` as
/// packed RGBA8 at native resolution -- the "zoom to region" view. The region must
/// lie within the frame; `out` must be exactly `w * h * 4` bytes.
pub fn i420_crop_to_rgba(frame: &YuvFrame, x: usize, y: usize, w: usize, h: usize, out: &mut [u8]) {
    i420_region_to_rgba(frame, x, y, w, h, 1, out);
}

/// The general form of the RGBA conversions: every `step`-th pixel of the `w`x`h`
/// region at (`x`, `y`), producing [`region_size`]`(w, h, step)` pixels. `out` must
/// be exactly that many pixels times 4 bytes.
pub fn i420_region_to_rgba(
    frame: &YuvFrame,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    step: usize,
    out: &mut [u8],
) {
    assert!(
        x + w <= frame.width && y + h <= frame.height,
        "region outside frame"
    );
    let (ow, oh) = region_size(w, h, step);
    convert_rgba(frame, x, y, ow, oh, step, out);
}

/// Output size of [`i420_region_to_rgba`] for a `w`x`h` region at `step`.
pub fn region_size(w: usize, h: usize, step: usize) -> (usize, usize) {
    assert!(step >= 1);
    (w.div_ceil(step), h.div_ceil(step))
}

/// Size of the image [`i420_to_rgba_decimated`] produces for `frame` at `step`.
pub fn decimated_size(frame: &YuvFrame, step: usize) -> (usize, usize) {
    region_size(frame.width, frame.height, step)
}

/// Converts every `step`-th pixel in each direction (nearest-neighbour downscale) into
/// `out` as packed RGBA8: a 4K frame at `step = 2` becomes a 1080p preview for a
/// quarter of the work. `out` must be exactly `w * h * 4` bytes for
/// [`decimated_size`]'s `(w, h)`.
pub fn i420_to_rgba_decimated(frame: &YuvFrame, step: usize, out: &mut [u8]) {
    let (w, h) = decimated_size(frame, step);
    convert_rgba(frame, 0, 0, w, h, step, out);
}

/// Writes `out_w` x `out_h` RGBA pixels, sampling the frame at
/// `(x0 + col * step, y0 + row * step)`. BT.601 limited range, the convention for
/// camera H.264 as produced by Android's encoder (and what YUYV consumers assume).
fn convert_rgba(
    frame: &YuvFrame,
    x0: usize,
    y0: usize,
    out_w: usize,
    out_h: usize,
    step: usize,
    out: &mut [u8],
) {
    debug_assert_eq!(out.len(), out_w * out_h * 4);
    let uv_w = frame.width / 2;
    for row in 0..out_h {
        let sy = y0 + row * step;
        let y_row = &frame.y[sy * frame.width..];
        let u_row = &frame.u[(sy / 2) * uv_w..];
        let v_row = &frame.v[(sy / 2) * uv_w..];
        let out_row = &mut out[row * out_w * 4..(row + 1) * out_w * 4];
        let (pixels, _) = out_row.as_chunks_mut::<4>();
        for (col, px) in pixels.iter_mut().enumerate() {
            let sx = x0 + col * step;
            let [r, g, b] = yuv_to_rgb(y_row[sx], u_row[sx / 2], v_row[sx / 2]);
            *px = [r, g, b, 255];
        }
    }
}

/// Which planes [`i420_region_planes`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaneFormat {
    /// Luma only: enough for every two-colour display mode, a third of the bytes.
    Luma,
    /// Luma plus 4:2:0 chroma at the output's own resolution.
    #[default]
    Yuv420,
}

/// Raw planes of a region (see [`i420_region_planes`]), in the frame's own
/// orientation. Tightly packed: `y` is `width * height`, `u` and `v` are
/// `width.div_ceil(2) * height.div_ceil(2)` (empty for [`PlaneFormat::Luma`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planes {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

/// Every `step`-th pixel of the `w`x`h` region at (`x`, `y`) as raw planes, the way
/// [`i420_region_to_rgba`] samples it but without converting: output luma `(c, r)`
/// is the frame's at `(x + c * step, y + r * step)`, and output chroma `(c, r)` is
/// what [`i420_region_to_rgba`] uses for output pixel `(2c, 2r)` -- so a GPU that
/// samples the chroma planes at half the luma coordinates reproduces it exactly
/// wherever chroma is constant over each 2x2 block of output pixels.
pub fn i420_region_planes(
    frame: &YuvFrame,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    step: usize,
    format: PlaneFormat,
) -> Planes {
    assert!(
        x + w <= frame.width && y + h <= frame.height,
        "region outside frame"
    );
    let (ow, oh) = region_size(w, h, step);
    // Every `step`-th of `n` bytes from `start`: a row of the output. A whole row
    // at step 1 is one copy (a magnified 4K frame is ~3 MB a request).
    let pick = |out: &mut Vec<u8>, plane: &[u8], start: usize, n: usize| {
        if step == 1 {
            out.extend_from_slice(&plane[start..start + n]);
        } else {
            out.extend(plane[start..].iter().step_by(step).take(n));
        }
    };
    let mut luma = Vec::with_capacity(ow * oh);
    for row in 0..oh {
        pick(&mut luma, &frame.y, (y + row * step) * frame.width + x, ow);
    }
    let (mut u, mut v) = (Vec::new(), Vec::new());
    if format == PlaneFormat::Yuv420 {
        let uv_w = frame.width / 2;
        let (cw, ch) = (ow.div_ceil(2), oh.div_ceil(2));
        u.reserve(cw * ch);
        v.reserve(cw * ch);
        // Output chroma (c, r) is at ((x + 2c step) / 2, (y + 2r step) / 2), which
        // is (x / 2 + c step, y / 2 + r step).
        for row in 0..ch {
            let start = (y / 2 + row * step) * uv_w + x / 2;
            pick(&mut u, &frame.u, start, cw);
            pick(&mut v, &frame.v, start, cw);
        }
    }
    Planes {
        width: ow,
        height: oh,
        y: luma,
        u,
        v,
    }
}

/// A quarter-turn rotation of an image for display, clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    pub fn turned_cw(self) -> Self {
        match self {
            Rotation::None => Rotation::Cw90,
            Rotation::Cw90 => Rotation::Cw180,
            Rotation::Cw180 => Rotation::Cw270,
            Rotation::Cw270 => Rotation::None,
        }
    }

    pub fn turned_ccw(self) -> Self {
        self.turned_cw().turned_cw().turned_cw()
    }

    /// Whether width and height swap under this rotation.
    pub fn transposes(self) -> bool {
        matches!(self, Rotation::Cw90 | Rotation::Cw270)
    }

    /// Size of a `w`x`h` image after rotation.
    pub fn rotated_size(self, w: usize, h: usize) -> (usize, usize) {
        if self.transposes() {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// Rotates a packed RGBA8 image of `w`x`h` pixels, returning the rotated pixels
/// (size per [`Rotation::rotated_size`]).
pub fn rotate_rgba(rgba: &[u8], w: usize, h: usize, rotation: Rotation) -> Vec<u8> {
    debug_assert_eq!(rgba.len(), w * h * 4);
    if rotation == Rotation::None {
        return rgba.to_vec();
    }
    let (ow, oh) = rotation.rotated_size(w, h);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            // Where the output pixel (x, y) comes from in the source.
            let (sx, sy) = match rotation {
                Rotation::None => (x, y),
                Rotation::Cw90 => (y, h - 1 - x),
                Rotation::Cw180 => (w - 1 - x, h - 1 - y),
                Rotation::Cw270 => (w - 1 - y, x),
            };
            out[(y * ow + x) * 4..][..4].copy_from_slice(&rgba[(sy * w + sx) * 4..][..4]);
        }
    }
    out
}

/// Converts packed RGBA8 (alpha ignored) into an I420 frame, BT.601 limited range
/// (the inverse of what [`i420_to_rgba`] does); each chroma sample is the mean of
/// its 2x2 block. `width` and `height` must be even.
pub fn rgba_to_i420(rgba: &[u8], width: usize, height: usize) -> YuvFrame {
    assert!(
        width.is_multiple_of(2) && height.is_multiple_of(2),
        "{width}x{height} is not even"
    );
    assert_eq!(rgba.len(), width * height * 4);
    let px = |x: usize, y: usize| -> [i32; 3] {
        let p = &rgba[(y * width + x) * 4..][..3];
        [p[0] as i32, p[1] as i32, p[2] as i32]
    };
    let mut y_plane = Vec::with_capacity(width * height);
    for row in 0..height {
        for col in 0..width {
            let [r, g, b] = px(col, row);
            y_plane.push((16 + ((66 * r + 129 * g + 25 * b + 128) >> 8)) as u8);
        }
    }
    let (cw, ch) = (width / 2, height / 2);
    let mut u_plane = Vec::with_capacity(cw * ch);
    let mut v_plane = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        for col in 0..cw {
            let mut sum = [0i32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = px(col * 2 + dx, row * 2 + dy);
                for c in 0..3 {
                    sum[c] += p[c];
                }
            }
            let [r, g, b] = sum.map(|c| (c + 2) / 4);
            u_plane.push((128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8)) as u8);
            v_plane.push((128 + ((112 * r - 94 * g - 18 * b + 128) >> 8)) as u8);
        }
    }
    YuvFrame {
        width,
        height,
        y: y_plane,
        u: u_plane,
        v: v_plane,
    }
}

/// BT.601 limited-range YCbCr -> RGB, fixed-point (x256):
/// R = 1.164 (Y-16) + 1.596 (V-128); G = 1.164 (Y-16) - 0.813 (V-128) - 0.391 (U-128);
/// B = 1.164 (Y-16) + 2.018 (U-128).
#[inline]
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = 298 * (y as i32 - 16) + 128;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clamp = |x: i32| (x >> 8).clamp(0, 255) as u8;
    [
        clamp(c + 409 * e),
        clamp(c - 100 * d - 208 * e),
        clamp(c + 516 * d),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame whose luma is `x + 10 * y` (so positions are recognisable), flat chroma.
    fn gradient_frame(w: usize, h: usize) -> YuvFrame {
        let mut y = Vec::with_capacity(w * h);
        for row in 0..h {
            for col in 0..w {
                y.push((16 + col + 10 * row) as u8);
            }
        }
        YuvFrame {
            width: w,
            height: h,
            y,
            u: vec![128; (w / 2) * (h / 2)],
            v: vec![128; (w / 2) * (h / 2)],
        }
    }

    #[test]
    fn bt601_reference_colours() {
        assert_eq!(yuv_to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb(235, 128, 128), [255, 255, 255]);
        // Limited-range gray 128 maps to 130 in full range.
        assert_eq!(yuv_to_rgb(128, 128, 128), [130, 130, 130]);
        // BT.601 pure red is approximately (Y 81, U 90, V 240).
        let [r, g, b] = yuv_to_rgb(81, 90, 240);
        assert!(r >= 250 && g <= 5 && b <= 5, "got {r},{g},{b}");
        // Super-white / super-black input saturates instead of wrapping.
        assert_eq!(yuv_to_rgb(255, 128, 128), [255, 255, 255]);
        assert_eq!(yuv_to_rgb(0, 128, 128), [0, 0, 0]);
    }

    #[test]
    fn region_planes_sample_where_the_rgba_conversion_does() {
        // Luma 16 + col + 10 row; chroma 100 + c + 7 r, so every sample is unique.
        let (w, h) = (12, 10);
        let mut frame = gradient_frame(w, h);
        let uv_w = w / 2;
        for r in 0..h / 2 {
            for c in 0..uv_w {
                frame.u[r * uv_w + c] = (100 + c + 7 * r) as u8;
                frame.v[r * uv_w + c] = (200 - c - 7 * r) as u8;
            }
        }
        for (x, y, rw, rh, step) in [
            (0, 0, 12, 10, 1),
            (3, 1, 7, 8, 1),
            (1, 3, 11, 7, 2),
            (2, 0, 9, 9, 3),
        ] {
            let p = i420_region_planes(&frame, x, y, rw, rh, step, PlaneFormat::Yuv420);
            assert_eq!((p.width, p.height), region_size(rw, rh, step));
            for r in 0..p.height {
                for c in 0..p.width {
                    assert_eq!(
                        p.y[r * p.width + c],
                        frame.y[(y + r * step) * w + x + c * step]
                    );
                }
            }
            let cw = p.width.div_ceil(2);
            assert_eq!(p.u.len(), cw * p.height.div_ceil(2));
            for r in 0..p.height.div_ceil(2) {
                for c in 0..cw {
                    let (sx, sy) = ((x + 2 * c * step) / 2, (y + 2 * r * step) / 2);
                    assert_eq!(
                        p.u[r * cw + c],
                        frame.u[sy * uv_w + sx],
                        "({x},{y}) step {step}"
                    );
                    assert_eq!(p.v[r * cw + c], frame.v[sy * uv_w + sx]);
                }
            }
            let luma = i420_region_planes(&frame, x, y, rw, rh, step, PlaneFormat::Luma);
            assert_eq!(luma.y, p.y);
            assert!(luma.u.is_empty() && luma.v.is_empty());
        }
    }

    #[test]
    fn rgba_to_i420_round_trips_within_rounding() {
        let colours: [[u8; 3]; 6] = [
            [0, 0, 0],
            [255, 255, 255],
            [255, 0, 0],
            [0, 160, 40],
            [30, 60, 220],
            [200, 190, 120],
        ];
        for c in colours {
            let rgba: Vec<u8> = (0..4).flat_map(|_| [c[0], c[1], c[2], 255]).collect();
            let frame = rgba_to_i420(&rgba, 2, 2);
            let mut back = vec![0u8; 16];
            i420_to_rgba(&frame, &mut back);
            for (got, want) in back[..3].iter().zip(c) {
                assert!(
                    got.abs_diff(want) <= 3,
                    "{c:?} came back as {:?}",
                    &back[..3]
                );
            }
        }
    }

    #[test]
    fn rgba_full_frame_alpha_and_gray() {
        let frame = gradient_frame(4, 2);
        let mut out = vec![0u8; 4 * 2 * 4];
        i420_to_rgba(&frame, &mut out);
        for px in out.as_chunks::<4>().0 {
            assert_eq!(px[3], 255);
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
        }
        // Y=16 at (0,0) is black; luma rises along the row.
        assert_eq!(out[0], 0);
        assert!(out[4] > out[0] && out[8] > out[4]);
    }

    #[test]
    fn crop_samples_the_requested_region() {
        let frame = gradient_frame(8, 6);
        let mut full = vec![0u8; 8 * 6 * 4];
        i420_to_rgba(&frame, &mut full);
        let mut crop = vec![0u8; 3 * 2 * 4];
        i420_crop_to_rgba(&frame, 5, 3, 3, 2, &mut crop);
        for row in 0..2 {
            for col in 0..3 {
                let c = &crop[(row * 3 + col) * 4..][..4];
                let f = &full[((row + 3) * 8 + col + 5) * 4..][..4];
                assert_eq!(c, f, "crop pixel ({col},{row})");
            }
        }
    }

    #[test]
    fn region_with_step_matches_crop_of_decimated() {
        let frame = gradient_frame(8, 6);
        assert_eq!(region_size(5, 4, 2), (3, 2));
        let mut out = vec![0u8; 3 * 2 * 4];
        i420_region_to_rgba(&frame, 2, 1, 5, 4, 2, &mut out);
        let mut full = vec![0u8; 8 * 6 * 4];
        i420_to_rgba(&frame, &mut full);
        for row in 0..2 {
            for col in 0..3 {
                let o = &out[(row * 3 + col) * 4..][..4];
                let f = &full[((1 + row * 2) * 8 + 2 + col * 2) * 4..][..4];
                assert_eq!(o, f, "region pixel ({col},{row})");
            }
        }
    }

    #[test]
    fn decimation_picks_every_nth_pixel() {
        let frame = gradient_frame(8, 6);
        assert_eq!(decimated_size(&frame, 2), (4, 3));
        assert_eq!(decimated_size(&frame, 3), (3, 2));
        let mut full = vec![0u8; 8 * 6 * 4];
        i420_to_rgba(&frame, &mut full);
        let mut small = vec![0u8; 4 * 3 * 4];
        i420_to_rgba_decimated(&frame, 2, &mut small);
        for row in 0..3 {
            for col in 0..4 {
                let s = &small[(row * 4 + col) * 4..][..4];
                let f = &full[((row * 2) * 8 + col * 2) * 4..][..4];
                assert_eq!(s, f, "decimated pixel ({col},{row})");
            }
        }
    }

    #[test]
    fn rotation_moves_the_corners_the_right_way() {
        // 3x2 image, pixel value = its index, one byte per channel for legibility.
        let px = |i: u8| [i, i, i, 255];
        let src: Vec<u8> = (0..6).flat_map(px).collect();
        let at = |buf: &[u8], w: usize, x: usize, y: usize| buf[(y * w + x) * 4];

        let cw = rotate_rgba(&src, 3, 2, Rotation::Cw90);
        assert_eq!(Rotation::Cw90.rotated_size(3, 2), (2, 3));
        // Source top-left (0) ends up top-right; source bottom-left (3) top-left.
        assert_eq!(at(&cw, 2, 1, 0), 0);
        assert_eq!(at(&cw, 2, 0, 0), 3);
        assert_eq!(at(&cw, 2, 1, 2), 2);

        let ccw = rotate_rgba(&src, 3, 2, Rotation::Cw270);
        // Source top-left ends up bottom-left; source top-right (2) top-left.
        assert_eq!(at(&ccw, 2, 0, 2), 0);
        assert_eq!(at(&ccw, 2, 0, 0), 2);

        let half = rotate_rgba(&src, 3, 2, Rotation::Cw180);
        assert_eq!(at(&half, 3, 0, 0), 5);
        assert_eq!(at(&half, 3, 2, 1), 0);

        assert_eq!(rotate_rgba(&src, 3, 2, Rotation::None), src);
        // Four quarter turns are the identity.
        let four = [Rotation::Cw90; 4]
            .iter()
            .fold((src.clone(), 3, 2), |(b, w, h), r| {
                (rotate_rgba(&b, w, h, *r), h, w)
            });
        assert_eq!(four.0, src);
        assert_eq!(Rotation::None.turned_ccw(), Rotation::Cw270);
    }

    #[test]
    fn converts_flat_gray_frame() {
        // 4x2 flat gray frame: Y=128, U=V=128 everywhere.
        let frame = YuvFrame {
            width: 4,
            height: 2,
            y: vec![128; 4 * 2],
            u: vec![128; 2],
            v: vec![128; 2],
        };
        let mut out = vec![0u8; 4 * 2 * 2];
        i420_to_yuyv(&frame, &mut out);
        assert!(out.iter().all(|&b| b == 128));
    }
}
