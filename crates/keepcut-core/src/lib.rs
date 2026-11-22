//! Keepcut core: probing, smart-cut planning and export through the ffmpeg CLI.
//! No UI dependencies.

pub mod chapters;
pub mod compat;
pub mod edit;
pub mod encoders;
pub mod error;
pub mod exec;
pub mod ffmpeg;
pub mod fsutil;
pub mod history;
pub mod model;
pub mod plan;
pub mod probe;
pub mod timefmt;
pub mod verify;

pub use error::{Error, Result};
