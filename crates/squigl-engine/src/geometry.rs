//! Rectangles and selections in *view* space -- the source frame turned by the chosen
//! [`Rotation`] -- and the mappings back to the source frame and in from the Zoom
//! pane's image.

use crate::layout::{self, Quad};
use squigl_core::convert::Rotation;

/// Drags smaller than this are a click, which clears the crop.
pub const MIN_CROP_PX: usize = 8;

/// A rectangle in pixels, in whichever space the context says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Crop {
    pub fn whole(w: usize, h: usize) -> Self {
        Self { x: 0, y: 0, w, h }
    }

    pub fn from_corners(a: (usize, usize), b: (usize, usize)) -> Self {
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    /// Clamps to a `width`x`height` space; `None` if nothing usable is left.
    pub fn clamped(self, width: usize, height: usize) -> Option<Self> {
        let x = self.x.min(width);
        let y = self.y.min(height);
        let w = self.w.min(width - x);
        let h = self.h.min(height - y);
        (w >= MIN_CROP_PX && h >= MIN_CROP_PX).then_some(Self { x, y, w, h })
    }

    /// Shifted by (`dx`, `dy`) and kept inside a `width`x`height` space.
    pub fn moved(self, dx: i64, dy: i64, width: usize, height: usize) -> Self {
        let x = (self.x as i64 + dx).clamp(0, (width - self.w) as i64) as usize;
        let y = (self.y as i64 + dy).clamp(0, (height - self.h) as i64) as usize;
        Self { x, y, ..self }
    }

    /// Scaled by `factor` about its centre, clamped to the space and to
    /// [`MIN_CROP_PX`]: the digital zoom in and out.
    pub fn scaled(self, factor: f32, width: usize, height: usize) -> Self {
        let (cx, cy) = (
            self.x as f32 + self.w as f32 / 2.0,
            self.y as f32 + self.h as f32 / 2.0,
        );
        let w = ((self.w as f32 * factor).round() as usize).clamp(MIN_CROP_PX, width);
        let h = ((self.h as f32 * factor).round() as usize).clamp(MIN_CROP_PX, height);
        let x = ((cx - w as f32 / 2.0).round().max(0.0) as usize).min(width - w);
        let y = ((cy - h as f32 / 2.0).round().max(0.0) as usize).min(height - h);
        Self { x, y, w, h }
    }

    pub fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn area(self) -> usize {
        self.w * self.h
    }

    /// Intersection over union with `other`: how much the same box they are.
    pub fn iou(self, other: Self) -> f32 {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        if x1 <= x0 || y1 <= y0 {
            return 0.0;
        }
        let inter = ((x1 - x0) * (y1 - y0)) as f32;
        inter / ((self.area() + other.area()) as f32 - inter)
    }

    /// Maps a rectangle in view space (the source frame turned by `rotation`, so
    /// `view_w`x`view_h` pixels) back to the source frame.
    pub fn to_source(self, rotation: Rotation, view_w: usize, view_h: usize) -> Self {
        let Self { x, y, w, h } = self;
        match rotation {
            Rotation::None => self,
            Rotation::Cw90 => Self {
                x: y,
                y: view_w - x - w,
                w: h,
                h: w,
            },
            Rotation::Cw180 => Self {
                x: view_w - x - w,
                y: view_h - y - h,
                w,
                h,
            },
            Rotation::Cw270 => Self {
                x: view_h - y - h,
                y: x,
                w: h,
                h: w,
            },
        }
    }
}

/// What is selected on the view: a rectangle, and -- when it came from a detected
/// block that is not rectangular -- the quad inside it that is the actual block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub rect: Crop,
    pub quad: Option<Quad>,
}

/// A point given as a fraction of the Zoom pane's image of `selection` (rectified,
/// for a quad), in view space. `None` for a quad too degenerate to invert.
pub fn zoom_to_view(selection: Selection, [fx, fy]: [f32; 2]) -> Option<[f32; 2]> {
    let rect = selection.rect;
    let (ox, oy) = (rect.x as f32, rect.y as f32);
    match selection.quad {
        None => Some([ox + fx * rect.w as f32, oy + fy * rect.h as f32]),
        Some(quad) => {
            let local = quad.map(|[x, y]| [x - ox, y - oy]);
            let (w, h) = layout::rectified_size(&local);
            let [x, y] = layout::unrectify(&local, [fx * w as f32, fy * h as f32])?;
            Some([ox + x, oy + y])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_is_overlap_over_union() {
        let a = Crop {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let b = Crop {
            x: 5,
            y: 0,
            w: 10,
            h: 10,
        };
        assert!((a.iou(a) - 1.0).abs() < 1e-6);
        assert!((a.iou(b) - 50.0 / 150.0).abs() < 1e-6);
        assert_eq!(a.iou(Crop { x: 20, ..a }), 0.0);
    }

    #[test]
    fn small_drags_are_clicks() {
        assert!(Crop::from_corners((10, 10), (12, 40))
            .clamped(100, 100)
            .is_none());
        assert_eq!(
            Crop::from_corners((90, 5), (200, 20)).clamped(100, 100),
            Some(Crop {
                x: 90,
                y: 5,
                w: 10,
                h: 15
            })
        );
    }

    #[test]
    fn a_zoom_pane_point_maps_to_view_space() {
        let rect = Crop {
            x: 100,
            y: 50,
            w: 200,
            h: 80,
        };
        let plain = Selection { rect, quad: None };
        assert_eq!(zoom_to_view(plain, [0.25, 0.5]), Some([150.0, 90.0]));
        assert_eq!(zoom_to_view(plain, [1.0, 1.0]), Some([300.0, 130.0]));
        // For a quad the rectified image's corners are the quad's.
        let quad: Quad = [[110.0, 55.0], [290.0, 70.0], [280.0, 128.0], [105.0, 110.0]];
        let tilted = Selection {
            rect,
            quad: Some(quad),
        };
        for (corner, want) in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
            .iter()
            .zip(quad)
        {
            let got = zoom_to_view(tilted, *corner).unwrap();
            assert!(
                (got[0] - want[0]).abs() < 1e-2 && (got[1] - want[1]).abs() < 1e-2,
                "{got:?} != {want:?}"
            );
        }
    }
}
