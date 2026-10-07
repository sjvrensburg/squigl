//! Squigl's UI-independent logic, shared by every front end. [`engine`] is the API a
//! front end drives (commands in, versioned state and events out); beneath it are
//! the camera worker and its sources ([`stream`]), view-space selections
//! ([`geometry`]) and the pixels they cover ([`render`]), what a GPU front end is
//! sent ([`view`]) and the display modes it applies ([`display`]), hand erasures
//! ([`erase`]), ink enhancement ([`enhance`]), block detection's types ([`layout`]),
//! transcription backends ([`transcribe`]), built-in models getting ready
//! ([`model`]), the settings file ([`config`]) and where files live ([`paths`]), the
//! session's readings ([`history`]) and what a typesetter is given ([`typeset`]).

pub mod config;
pub mod display;
pub mod engine;
pub mod enhance;
pub mod erase;
pub mod geometry;
pub mod history;
pub mod layout;
pub mod math;
pub mod model;
pub mod paths;
pub mod render;
pub mod stream;
pub mod transcribe;
pub mod typeset;
pub mod view;
