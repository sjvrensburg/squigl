//! Linux V4L2 output for `squigl-core`: [`V4l2Sink`] writes decoded frames to a
//! `v4l2loopback` device (`/dev/videoN`) so any webcam app can use the phone, and
//! [`loopback`] creates that device node when it is missing.
//!
//! Linux-only: on any other target this crate is empty, so a crate that depends on
//! it only for an optional feature (the engine's `--device` tee) still builds there.
//! `squigl-cli` uses it only on Linux, the one place it does anything.

#![cfg(target_os = "linux")]

pub mod loopback;
mod sink;

pub use sink::{LazyV4l2Sink, V4l2Sink};
