//! An Android phone's camera as decoded frames, from either of two sources:
//! [`CameraSession`] (scrcpy-server over ADB) or [`WebrtcSource`] (a browser on the
//! phone) -- or from a recording of one ([`Replay`]). Frames go to any
//! [`sink::FrameSink`]; the Linux V4L2 (`/dev/videoN`) sink is the separate
//! `squigl-v4l2` crate, so nothing here is Linux-specific.
//!
//! The Android-side capture/encode is delegated to the real, upstream `scrcpy-server`
//! (embedded at build time, see `build.rs`) over ADB -- only the client-side protocol
//! and H.264 decode are implemented here. See the repository README for the full
//! architecture and scope.

pub mod adb;
pub mod cameras;
pub mod convert;
pub mod decode;
pub mod error;
pub mod protocol;
pub mod replay;
pub mod session;
pub mod sink;
pub mod webrtc_source;

pub use cameras::{list_cameras, CameraInfo};
pub use error::{Error, Result};
pub use replay::{Recorder, Replay};
pub use session::{CameraControl, CameraSession, ConnectOptions, Facing, ZOOM_STEP};
pub use webrtc_source::WebrtcSource;
