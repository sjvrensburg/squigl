//! What a front end that draws on the GPU asks for, and what it gets: the part of a
//! frame its viewport shows, decimated to no more pixels than the screen has there,
//! as raw planes. The front end does the YUV->RGB conversion, the rotation, the
//! pan and zoom within what it was sent, and the display mode's table
//! ([`crate::display`]) in one shader pass.
//!
//! A [`Viewport`] (the drawing area, the point at its centre, the magnification)
//! becomes a [`ViewRequest`] (a view-space region and a step) by
//! [`Viewport::request`]; [`render_planes`] answers one with [`ViewPlanes`], whose
//! [`PlaneHeader`] says exactly what was sampled.

use crate::geometry::Crop;
use serde::{Deserialize, Serialize};
use squigl_core::convert::{i420_region_planes, PlaneFormat, Planes, Rotation};
use squigl_core::decode::YuvFrame;

/// Which frame a request is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrameRef {
    /// The newest frame from the source.
    Live,
    /// The frozen capture.
    Captured,
    /// What is on screen: the capture while there is one, else the live frame.
    #[default]
    Shown,
}

/// A front end's drawing area and what it shows.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// The drawing area in CSS pixels.
    pub width: f32,
    pub height: f32,
    /// Device pixels per CSS pixel.
    pub dpr: f32,
    /// The view-space point at the centre of the drawing area, as a fraction of the
    /// view's width and height (0.5, 0.5 is the middle).
    pub centre: [f32; 2],
    /// 1 fits the whole view in the drawing area; 2 shows half as much, twice as
    /// big.
    pub magnification: f32,
}

/// A region of the (rotated) view to send, every `step`-th pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRequest {
    pub frame: FrameRef,
    /// In view space, i.e. after the rotation.
    pub region: Crop,
    pub step: usize,
    pub format: PlaneFormat,
}

/// Where a [`Viewport`] puts the view: the view-space point at the drawing area's
/// top-left corner and the device pixels per view pixel. A view smaller than the
/// area (low magnification) is centred, so the origin may be negative.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub origin: [f32; 2],
    pub scale: f32,
}

impl Viewport {
    /// Where this viewport puts a `view`-sized (rotated) frame. A centre too near
    /// an edge is moved in, so the area never shows past the view's edge on a side
    /// it could fill.
    pub fn placement(&self, view: (usize, usize)) -> Placement {
        let (vw, vh) = (view.0.max(1) as f32, view.1.max(1) as f32);
        let (dw, dh) = (
            (self.width * self.dpr).max(1.0),
            (self.height * self.dpr).max(1.0),
        );
        let scale = (dw / vw).min(dh / vh) * self.magnification.max(f32::EPSILON);
        let origin = |c: f32, area: f32, size: f32| {
            let shown = area / scale;
            if shown >= size {
                (size - shown) / 2.0
            } else {
                (c * size).clamp(shown / 2.0, size - shown / 2.0) - shown / 2.0
            }
        };
        Placement {
            origin: [
                origin(self.centre[0], dw, vw),
                origin(self.centre[1], dh, vh),
            ],
            scale,
        }
    }

    /// The part of a `view`-sized (rotated) frame this viewport shows (see
    /// [`placement`](Self::placement)), and the largest step that still leaves at
    /// least one sent pixel per device pixel.
    pub fn request(
        &self,
        view: (usize, usize),
        frame: FrameRef,
        format: PlaneFormat,
    ) -> ViewRequest {
        let p = self.placement(view);
        let (dw, dh) = (
            (self.width * self.dpr).max(1.0),
            (self.height * self.dpr).max(1.0),
        );
        let span = |origin: f32, area: f32, size: usize| {
            let start = (origin.floor().max(0.0) as usize).min(size.max(1) - 1);
            let end = ((origin + area / p.scale).ceil().max(0.0) as usize).min(size);
            (start, end.saturating_sub(start).max(1))
        };
        let (x, w) = span(p.origin[0], dw, view.0);
        let (y, h) = span(p.origin[1], dh, view.1);
        let step = ((1.0 / p.scale).floor() as usize).max(1);
        ViewRequest {
            frame,
            region: Crop { x, y, w, h },
            step,
            format,
        }
    }
}

/// What [`render_planes`] sampled, for the front end to place and orient it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaneHeader {
    /// The frame's number: a live frame's count from the source, or the capture's.
    pub seq: u64,
    /// The whole frame, in its own (unrotated) orientation.
    pub source_width: usize,
    pub source_height: usize,
    /// The sampled region of the frame, unrotated.
    pub region: Crop,
    pub step: usize,
    /// To apply when drawing: the planes are in the frame's own orientation.
    pub rotation: Rotation,
    pub format: PlaneFormat,
    /// The luma plane's size; chroma planes are half of each, rounded up.
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewPlanes {
    pub header: PlaneHeader,
    pub planes: Planes,
}

/// Answers `request` from `frame`, shown at `rotation`. A region reaching outside the
/// view is clipped to it; one entirely outside gets the whole view.
pub fn render_planes(
    frame: &YuvFrame,
    seq: u64,
    rotation: Rotation,
    request: &ViewRequest,
) -> ViewPlanes {
    let (vw, vh) = rotation.rotated_size(frame.width, frame.height);
    // Clipped here rather than by `Crop::clamped`, which also refuses a region
    // smaller than a hand-drawn crop may be.
    let r = request.region;
    let (x, y) = (r.x.min(vw), r.y.min(vh));
    let region = match (r.w.min(vw - x), r.h.min(vh - y)) {
        (0, _) | (_, 0) => Crop::whole(vw, vh),
        (w, h) => Crop { x, y, w, h },
    };
    let src = region.to_source(rotation, vw, vh);
    let step = request.step.max(1);
    let planes = i420_region_planes(frame, src.x, src.y, src.w, src.h, step, request.format);
    ViewPlanes {
        header: PlaneHeader {
            seq,
            source_width: frame.width,
            source_height: frame.height,
            region: src,
            step,
            rotation,
            format: request.format,
            width: planes.width,
            height: planes.height,
        },
        planes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::render_region;
    use squigl_core::convert::{i420_to_rgba, rotate_rgba};

    /// A 10x6 frame, luma `16 + 2x + 20y` (all distinct), flat chroma.
    fn frame() -> YuvFrame {
        let (w, h) = (10, 6);
        YuvFrame {
            width: w,
            height: h,
            y: (0..h)
                .flat_map(|r| (0..w).map(move |c| (16 + 2 * c + 20 * r) as u8))
                .collect(),
            u: vec![128; (w / 2) * (h / 2)],
            v: vec![128; (w / 2) * (h / 2)],
        }
    }

    /// The planes drawn the way a front end would: converted, then rotated.
    fn draw(p: &ViewPlanes) -> Vec<u8> {
        let (w, h) = (p.planes.width, p.planes.height);
        let as_frame = YuvFrame {
            width: w,
            height: h,
            y: p.planes.y.clone(),
            // Flat chroma, so any (generous) size will do.
            u: vec![128; w * h],
            v: vec![128; w * h],
        };
        let mut rgba = vec![0; w * h * 4];
        i420_to_rgba(&as_frame, &mut rgba);
        rotate_rgba(&rgba, w, h, p.header.rotation)
    }

    #[test]
    fn planes_drawn_match_the_rgba_render_for_every_rotation_region_and_step() {
        let f = frame();
        for rotation in [
            Rotation::None,
            Rotation::Cw90,
            Rotation::Cw180,
            Rotation::Cw270,
        ] {
            let (vw, vh) = rotation.rotated_size(f.width, f.height);
            for step in 1..=3 {
                for x in 0..vw {
                    for y in 0..vh {
                        for (w, h) in [(1, 1), (vw - x, vh - y), ((vw - x).min(3), (vh - y).min(4))]
                        {
                            let region = Crop { x, y, w, h };
                            let request = ViewRequest {
                                frame: FrameRef::Shown,
                                region,
                                step,
                                format: PlaneFormat::Yuv420,
                            };
                            let planes = render_planes(&f, 7, rotation, &request);
                            let (want, ww, wh) = render_region(&f, rotation, region, step);
                            let (pw, ph) =
                                rotation.rotated_size(planes.planes.width, planes.planes.height);
                            assert_eq!((pw, ph), (ww, wh), "{rotation:?} {region:?} step {step}");
                            assert_eq!(draw(&planes), want, "{rotation:?} {region:?} step {step}");
                            assert_eq!(planes.header.seq, 7);
                            assert_eq!(planes.header.rotation, rotation);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_region_outside_the_view_is_clipped() {
        let f = frame();
        let request = ViewRequest {
            frame: FrameRef::Live,
            region: Crop {
                x: 8,
                y: 4,
                w: 50,
                h: 50,
            },
            step: 1,
            format: PlaneFormat::Luma,
        };
        let p = render_planes(&f, 1, Rotation::None, &request);
        assert_eq!((p.header.width, p.header.height), (2, 2));
        assert!(p.planes.u.is_empty());
    }

    fn viewport(magnification: f32, centre: [f32; 2]) -> Viewport {
        Viewport {
            width: 800.0,
            height: 600.0,
            dpr: 1.0,
            centre,
            magnification,
        }
    }

    #[test]
    fn at_1x_a_large_frame_is_sent_whole_and_decimated_to_the_screen() {
        // 4000x3000 into 800x600: a fifth of the pixels each way.
        let r = viewport(1.0, [0.5, 0.5]).request((4000, 3000), FrameRef::Shown, PlaneFormat::Luma);
        assert_eq!(
            r.region,
            Crop {
                x: 0,
                y: 0,
                w: 4000,
                h: 3000
            }
        );
        assert_eq!(r.step, 5);
    }

    #[test]
    fn magnified_the_region_shrinks_around_the_centre_and_the_step_falls() {
        let r =
            viewport(10.0, [0.5, 0.5]).request((4000, 3000), FrameRef::Shown, PlaneFormat::Luma);
        assert_eq!(
            r.region,
            Crop {
                x: 1800,
                y: 1350,
                w: 400,
                h: 300
            }
        );
        // 2 device pixels per view pixel: every pixel is sent.
        assert_eq!(r.step, 1);
        // A high-DPI screen at 1x needs twice the pixels: step 2, not 5.
        let hidpi = Viewport {
            dpr: 2.0,
            ..viewport(1.0, [0.5, 0.5])
        };
        assert_eq!(
            hidpi
                .request((4000, 3000), FrameRef::Shown, PlaneFormat::Luma)
                .step,
            2
        );
    }

    #[test]
    fn a_small_view_is_centred_and_a_magnified_one_follows_the_centre() {
        // 400x300 in 800x600 at 1x: fits exactly (scale 2), origin 0.
        let p = viewport(1.0, [0.5, 0.5]).placement((400, 300));
        assert_eq!((p.origin, p.scale), ([0.0, 0.0], 2.0));
        // A square 300x300 view in 800x600: scale 2, 400 view px across, centred.
        let p = viewport(1.0, [0.5, 0.5]).placement((300, 300));
        assert_eq!((p.origin, p.scale), ([-50.0, 0.0], 2.0));
        // At 4x on 4000x3000, centred on (0.25, 0.5): 1000 view px wide from 500.
        let p = viewport(4.0, [0.25, 0.5]).placement((4000, 3000));
        assert_eq!(p.scale, 0.8);
        assert_eq!(p.origin, [500.0, 1125.0]);
    }

    #[test]
    fn a_centre_near_the_edge_is_moved_in() {
        let r =
            viewport(10.0, [0.0, 1.0]).request((4000, 3000), FrameRef::Shown, PlaneFormat::Luma);
        assert_eq!(
            r.region,
            Crop {
                x: 0,
                y: 2700,
                w: 400,
                h: 300
            }
        );
    }
}
