//! A synthetic source: cycling colour bars, so a sink or a front end can be
//! exercised with no phone and no recording.

use crate::decode::YuvFrame;
use crate::error::Result;
use crate::sink::FrameSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The eight 75% bars (white, yellow, cyan, green, magenta, red, blue, black) as
/// BT.601 limited-range Y, U, V.
const BARS: [(u8, u8, u8); 8] = [
    (235, 128, 128),
    (210, 16, 146),
    (170, 166, 16),
    (145, 54, 34),
    (106, 202, 222),
    (81, 90, 240),
    (41, 240, 110),
    (16, 128, 128),
];

/// Colour bars that shift one bar to the left every eight frames.
pub struct TestPattern {
    width: usize,
    height: usize,
}

impl TestPattern {
    /// The rate [`run`](Self::run) produces frames at.
    pub const FPS: u32 = 30;

    /// A `width`x`height` pattern; both must be even.
    pub fn new(width: usize, height: usize) -> Self {
        assert!(
            width.is_multiple_of(2) && height.is_multiple_of(2),
            "{width}x{height} is not even"
        );
        Self { width, height }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Frame number `index`.
    pub fn frame(&self, index: usize) -> YuvFrame {
        let (w, h) = (self.width, self.height);
        let shift = index / 8;
        // The bar under pixel column `col`.
        let bar = |col: usize| BARS[(col * BARS.len() / w + shift) % BARS.len()];
        let y_row: Vec<u8> = (0..w).map(|col| bar(col).0).collect();
        let u_row: Vec<u8> = (0..w / 2).map(|cx| bar(cx * 2).1).collect();
        let v_row: Vec<u8> = (0..w / 2).map(|cx| bar(cx * 2).2).collect();
        YuvFrame {
            width: w,
            height: h,
            y: y_row.repeat(h),
            u: u_row.repeat(h / 2),
            v: v_row.repeat(h / 2),
        }
    }

    /// Blocks, handing `sink` a frame every 1/[`FPS`](Self::FPS) s until `stop`.
    pub fn run<S: FrameSink + ?Sized>(&mut self, sink: &mut S, stop: &AtomicBool) -> Result<()> {
        let mut index = 0usize;
        while !stop.load(Ordering::Relaxed) {
            sink.frame(&self.frame(index))?;
            index = index.wrapping_add(1);
            std::thread::sleep(Duration::from_secs(1) / Self::FPS);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_bars_that_shift_every_eight_frames() {
        let p = TestPattern::new(16, 4);
        let f = p.frame(0);
        assert_eq!((f.y.len(), f.u.len(), f.v.len()), (64, 16, 16));
        // Two pixels per bar: white first, black last.
        assert_eq!(&f.y[..4], &[235, 235, 210, 210]);
        assert_eq!(f.y[15], 16);
        // Every row is the same.
        assert_eq!(&f.y[..16], &f.y[48..]);
        assert_eq!(p.frame(7).y, f.y);
        assert_eq!(p.frame(8).y[0], 210);
    }
}
