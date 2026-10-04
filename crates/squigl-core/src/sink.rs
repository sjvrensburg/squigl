//! Where decoded frames go: the [`FrameSink`] trait that [`crate::CameraSession::run`]
//! and [`crate::WebrtcSource::run`] feed. The V4L2 (`/dev/videoN`) sink lives in the
//! Linux-only `squigl-v4l2` crate.

use crate::decode::YuvFrame;
use crate::error::Result;

/// Consumer of decoded frames. [`crate::CameraSession::run`] calls [`frame`](Self::frame)
/// once per decoded frame, on the decoding thread, at the size in
/// [`crate::CameraSession::meta`]; returning an error ends the session.
///
/// Implemented by `squigl_v4l2::V4l2Sink` and by any `FnMut(&YuvFrame) -> Result<()>`, so a
/// preview or recorder can be a closure (annotate the parameter type; inference
/// can't pick the lifetime on its own):
///
/// ```no_run
/// # use squigl_core::{decode::YuvFrame, CameraSession, ConnectOptions};
/// # use std::sync::atomic::AtomicBool;
/// # fn main() -> squigl_core::Result<()> {
/// let mut session = CameraSession::connect(ConnectOptions::default())?;
/// let mut show = |frame: &YuvFrame| {
///     println!("{}x{}", frame.width, frame.height);
///     Ok(())
/// };
/// session.run(&mut show, &AtomicBool::new(false))
/// # }
/// ```
pub trait FrameSink {
    fn frame(&mut self, frame: &YuvFrame) -> Result<()>;
}

impl<F: FnMut(&YuvFrame) -> Result<()>> FrameSink for F {
    fn frame(&mut self, frame: &YuvFrame) -> Result<()> {
        self(frame)
    }
}
