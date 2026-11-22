use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} not found (looked next to the executable and on PATH)")]
    ToolNotFound(&'static str),
    #[error("failed to start {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("cancelled")]
    Cancelled,
    #[error("command failed (exit code {code:?}): {cmd}\n{stderr_tail}")]
    CommandFailed {
        cmd: String,
        code: Option<i32>,
        stderr_tail: String,
    },
    #[error("cannot probe {path}: {msg}")]
    Probe { path: PathBuf, msg: String },
    #[error("cannot plan export: {0}")]
    Plan(String),
    /// A project file that can't be read.
    #[error("{0}")]
    Project(String),
    /// Not enough disk space (message says where and how much).
    #[error("{0}")]
    NoSpace(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
