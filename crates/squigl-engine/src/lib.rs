//! Squigl's UI-independent logic, shared by every front end: the camera worker
//! ([`stream`]), view-space selections ([`geometry`]) and the pixels they cover
//! ([`render`]), hand erasures ([`erase`]), ink enhancement ([`enhance`]), block
//! detection's types ([`layout`]), transcription backends and the config file
//! ([`transcribe`]), the session's readings ([`history`]) and what a typesetter is
//! given ([`typeset`]).
//!
//! No UI toolkit, ONNX Runtime or Typst in here: the built-in models implement
//! [`transcribe::Transcriber`] and [`layout::BlockDetector`], and a typesetter
//! implements [`typeset::Typesetter`], from outside.

pub mod enhance;
pub mod erase;
pub mod geometry;
pub mod history;
pub mod layout;
pub mod render;
pub mod stream;
pub mod transcribe;
pub mod typeset;
