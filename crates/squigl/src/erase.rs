//! Hand erasure: brush strokes over a capture, painted over with the paper around
//! them before anything reads it, for a light scratch-out the model would otherwise
//! read through. The fill only has to look like empty paper to the model, not to a
//! person, so it is one flat colour per stroke taken from just outside it -- no
//! inpainting (a ruled line under a stroke is left with a gap, which the models
//! ignore).

use image::{Rgba, RgbaImage};

/// How far around a stroke (in the image's own pixels) its paper colour is sampled.
const RING_PX: f32 = 6.0;

/// The ring's brightness percentile taken as the paper: paper is the brighter
/// majority around handwriting, so this holds while up to three quarters of the
/// ring is ink.
const PAPER_PERCENTILE: f32 = 0.75;

/// One brush stroke: the pointer's path and the brush's radius, in the pixels of
/// whatever it is drawn on. Everything within `radius` of the path is erased; one
/// point is a dot.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub points: Vec<[f32; 2]>,
    pub radius: f32,
}

impl Stroke {
    /// A straight stroke from `a` to `b`.
    pub fn line(a: [f32; 2], b: [f32; 2], radius: f32) -> Self {
        Self {
            points: vec![a, b],
            radius,
        }
    }

    /// Each point through `f`, the radius scaled by `scale`.
    fn mapped(&self, f: impl Fn([f32; 2]) -> [f32; 2], scale: f32) -> Self {
        Self {
            points: self.points.iter().map(|&p| f(p)).collect(),
            radius: self.radius * scale,
        }
    }
}

/// The strokes of a view, for a region of it at `origin` rendered every `step`-th
/// pixel, in that rendering's own pixels.
pub fn to_region(strokes: &[Stroke], origin: (usize, usize), step: usize) -> Vec<Stroke> {
    let (ox, oy, s) = (origin.0 as f32, origin.1 as f32, step as f32);
    strokes
        .iter()
        .map(|st| st.mapped(|[x, y]| [(x - ox) / s, (y - oy) / s], 1.0 / s))
        .collect()
}

/// Paints each of `strokes` (in `img`'s pixels) with the paper colour around it, in
/// order, each sampling the image as the ones before it left it. The colour comes
/// from as much of the ring as `img` holds, so a crop that cuts through it can fill
/// a shade differently from the whole view -- paper either way.
pub fn erase(img: &mut RgbaImage, strokes: &[Stroke]) {
    for stroke in strokes {
        if stroke.points.is_empty() || stroke.radius <= 0.0 {
            continue;
        }
        let reach = stroke.radius + RING_PX;
        let Some((x0, y0, x1, y1)) = bounds(img, &stroke.points, reach) else {
            continue;
        };
        let bw = (x1 - x0) as usize;
        // Distance from each pixel centre in the box to the path, near it only.
        let mut dist = vec![f32::INFINITY; bw * (y1 - y0) as usize];
        let segments = stroke
            .points
            .windows(2)
            .map(|w| (w[0], w[1]))
            .chain((stroke.points.len() == 1).then(|| (stroke.points[0], stroke.points[0])));
        for (a, b) in segments {
            let Some((sx0, sy0, sx1, sy1)) = bounds(img, &[a, b], reach) else {
                continue;
            };
            for y in sy0.max(y0)..sy1.min(y1) {
                for x in sx0.max(x0)..sx1.min(x1) {
                    let d = segment_distance([x as f32 + 0.5, y as f32 + 0.5], a, b);
                    let i = (y - y0) as usize * bw + (x - x0) as usize;
                    dist[i] = dist[i].min(d);
                }
            }
        }
        let mut ring = Vec::new();
        let mut inside = Vec::new();
        for (i, &d) in dist.iter().enumerate() {
            let (x, y) = (x0 + (i % bw) as u32, y0 + (i / bw) as u32);
            if d <= stroke.radius {
                inside.push((x, y));
            } else if d <= reach {
                ring.push(*img.get_pixel(x, y));
            }
        }
        if inside.is_empty() {
            continue;
        }
        let paper = paper_colour(&mut ring).unwrap_or(Rgba([255, 255, 255, 255]));
        for (x, y) in inside {
            img.put_pixel(x, y, paper);
        }
    }
}

/// The bounding box of `points` grown by `margin`, clamped to `img`; `None` when
/// that misses the image altogether.
fn bounds(img: &RgbaImage, points: &[[f32; 2]], margin: f32) -> Option<(u32, u32, u32, u32)> {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in points {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x0 = (lo[0] - margin).floor().max(0.0);
    let y0 = (lo[1] - margin).floor().max(0.0);
    let x1 = (hi[0] + margin).ceil().min(w);
    let y1 = (hi[1] + margin).ceil().min(h);
    (x0 < x1 && y0 < y1).then_some((x0 as u32, y0 as u32, x1 as u32, y1 as u32))
}

/// Distance from `p` to the segment `a`-`b` (a point when they coincide).
fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
    ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()
}

/// The [`PAPER_PERCENTILE`] pixel of `ring` by brightness; `None` for no ring (a
/// stroke covering the whole image).
fn paper_colour(ring: &mut [Rgba<u8>]) -> Option<Rgba<u8>> {
    if ring.is_empty() {
        return None;
    }
    let luma = |p: &Rgba<u8>| 299 * p.0[0] as u32 + 587 * p.0[1] as u32 + 114 * p.0[2] as u32;
    let k = ((ring.len() - 1) as f32 * PAPER_PERCENTILE).round() as usize;
    let (_, paper, _) = ring.select_nth_unstable_by_key(k, luma);
    Some(Rgba([paper.0[0], paper.0[1], paper.0[2], 255]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAPER: Rgba<u8> = Rgba([235, 225, 200, 255]);
    const INK: Rgba<u8> = Rgba([30, 30, 60, 255]);

    /// Paper with a horizontal ink stroke through rows 9..12.
    fn struck_page() -> RgbaImage {
        RgbaImage::from_fn(
            40,
            20,
            |_, y| if (9..12).contains(&y) { INK } else { PAPER },
        )
    }

    #[test]
    fn a_stroke_becomes_the_paper_around_it_and_nothing_else_changes() {
        let before = struck_page();
        let mut img = before.clone();
        let stroke = Stroke::line([10.0, 10.0], [30.0, 10.0], 3.0);
        erase(&mut img, std::slice::from_ref(&stroke));
        for (x, y, p) in img.enumerate_pixels() {
            let d = segment_distance([x as f32 + 0.5, y as f32 + 0.5], [10.0, 10.0], [30.0, 10.0]);
            if d <= 3.0 {
                assert_eq!(*p, PAPER, "({x}, {y}) under the stroke");
            } else {
                assert_eq!(p, before.get_pixel(x, y), "({x}, {y}) off it");
            }
        }
        // The ink line survives beyond the stroke's rounded ends.
        assert_eq!(*img.get_pixel(5, 10), INK);
        assert_eq!(*img.get_pixel(35, 10), INK);
    }

    #[test]
    fn a_click_is_a_dot() {
        // A thin ink line through row 10: a dot on it clears the line under the
        // brush only.
        let mut img = RgbaImage::from_fn(20, 20, |_, y| if y == 10 { INK } else { PAPER });
        let dot = Stroke {
            points: vec![[10.0, 10.5]],
            radius: 2.5,
        };
        erase(&mut img, &[dot]);
        assert_eq!(*img.get_pixel(10, 10), PAPER, "under the dot");
        assert_eq!(*img.get_pixel(14, 10), INK, "beyond the radius");
    }

    #[test]
    fn mostly_ink_around_still_fills_with_paper() {
        // Two thirds of every row is ink: the ring is mostly ink, the fill is not.
        let mut img = RgbaImage::from_fn(30, 30, |x, _| if x < 20 { INK } else { PAPER });
        erase(&mut img, &[Stroke::line([12.0, 15.0], [18.0, 15.0], 5.0)]);
        assert_eq!(*img.get_pixel(15, 15), PAPER);
    }

    #[test]
    fn strokes_off_the_image_are_clamped_or_skipped() {
        let mut img = struck_page();
        erase(
            &mut img,
            &[
                Stroke::line([-10.0, 10.0], [3.0, 10.0], 2.0),
                Stroke::line([100.0, 100.0], [120.0, 100.0], 2.0),
            ],
        );
        assert_eq!(*img.get_pixel(2, 10), PAPER);
        assert_eq!(*img.get_pixel(10, 10), INK);
    }

    #[test]
    fn view_strokes_move_into_a_region_and_its_step() {
        let s = Stroke::line([110.0, 60.0], [130.0, 80.0], 8.0);
        assert_eq!(
            to_region(&[s], (100, 50), 2),
            vec![Stroke::line([5.0, 5.0], [15.0, 15.0], 4.0)]
        );
    }
}
