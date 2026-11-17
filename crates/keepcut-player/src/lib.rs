//! libmpv playback for Keepcut: an mpv client wrapper with an event thread
//! ([`player`]) and render contexts drawing into egui ([`render`]): OpenGL into an
//! app-owned texture, or a software fallback on its own thread.
//!
//! The `keepcut-player` binary in this crate is the Phase 0B spike / benchmark tool.

pub mod player;
pub mod render;
