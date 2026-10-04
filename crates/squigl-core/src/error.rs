use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "adb not found: install Android platform-tools (android-tools on Linux), or set \
         SQUIGL_ADB to the adb executable"
    )]
    AdbNotFound,

    #[error("adb command failed: {0}")]
    AdbCommand(String),

    #[error("no Android device found (is USB debugging enabled and authorized, or the phone reachable over Wi-Fi?)")]
    NoDevice,

    #[error(
        "the phone has not allowed USB debugging from this computer yet: unlock it and tap Allow"
    )]
    DeviceUnauthorized,

    #[error(
        "the phone is connected but not responding (adb lists it as offline); reconnect the cable"
    )]
    DeviceOffline,

    #[error("unexpected scrcpy protocol data: {0}")]
    Protocol(String),

    #[error("video stream stalled: no data from the phone for {0:?}")]
    StreamStalled(std::time::Duration),

    #[error("the phone restarted the video stream at {width}x{height}; reconnect to pick up the new size")]
    StreamResized { width: u32, height: u32 },

    #[error("H.264 decode error: {0}")]
    Decode(String),

    /// A [`crate::sink::FrameSink`] failed; the message names the sink.
    #[error("{0}")]
    Sink(String),

    /// A [`crate::replay`] recording could not be read or written; the message says
    /// what.
    #[error("{0}")]
    Recording(String),

    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
