//! Locating, spawning and supervising ffmpeg/ffprobe processes.
//!
//! Processes run synchronously on the calling (background) thread. Cancellation
//! is cooperative: the supervisor loop polls a [`CancelToken`] and kills the child.

mod quote;

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::error::{Error, Result};

pub use quote::shell_quote;

/// Paths of the ffmpeg and ffprobe executables.
#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    /// Bundled binaries next to the current executable first, then `PATH`.
    pub fn locate() -> Result<Tools> {
        Ok(Tools {
            ffmpeg: find_tool("ffmpeg")?,
            ffprobe: find_tool("ffprobe")?,
        })
    }

    /// ffmpeg and ffprobe from one folder (a user-chosen location).
    pub fn in_dir(dir: &Path) -> Result<Tools> {
        let pick = |name: &'static str| {
            let p = dir.join(exe_name(name));
            if p.is_file() {
                Ok(p)
            } else {
                Err(Error::ToolNotFound(name))
            }
        };
        Ok(Tools {
            ffmpeg: pick("ffmpeg")?,
            ffprobe: pick("ffprobe")?,
        })
    }

    /// The version line of ffmpeg (`ffmpeg version 9.0.2-…`), or the error.
    pub fn version(&self) -> std::result::Result<String, String> {
        let o =
            run(&self.ffmpeg().arg("-version"), RunOpts::default()).map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .next()
            .map(|l| l.split(" Copyright").next().unwrap_or(l).to_string())
            .ok_or_else(|| "no output".into())
    }

    pub fn ffmpeg(&self) -> Cmd {
        Cmd::new(&self.ffmpeg)
    }

    pub fn ffprobe(&self) -> Cmd {
        Cmd::new(&self.ffprobe)
    }
}

fn exe_name(name: &'static str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn find_tool(name: &'static str) -> Result<PathBuf> {
    let file = exe_name(name);
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let p = dir.join(&file);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(&file);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    Err(Error::ToolNotFound(name))
}

/// Cooperative cancellation flag shared between the UI and workers.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Receives every command line and its stderr output (for the command log).
pub type LogSink = Arc<dyn Fn(LogLine<'_>) + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub enum LogLine<'a> {
    Command(&'a str),
    Stderr(&'a str),
    Info(&'a str),
}

/// A command line under construction.
#[derive(Debug, Clone)]
pub struct Cmd {
    program: PathBuf,
    args: Vec<OsString>,
}

impl Cmd {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Cmd {
            program: program.into(),
            args: Vec::new(),
        }
    }
    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Self {
        self.args.push(a.as_ref().to_os_string());
        self
    }
    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(it.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }
    pub fn push(&mut self, a: impl AsRef<OsStr>) {
        self.args.push(a.as_ref().to_os_string());
    }
    pub fn get_args(&self) -> &[OsString] {
        &self.args
    }
    /// Copyable, shell-quoted command line.
    pub fn display(&self) -> String {
        let mut s = shell_quote(&self.program.to_string_lossy());
        for a in &self.args {
            s.push(' ');
            s.push_str(&shell_quote(&a.to_string_lossy()));
        }
        s
    }
}

/// Options for [`run`].
#[derive(Default)]
pub struct RunOpts<'a> {
    pub cancel: Option<&'a CancelToken>,
    pub log: Option<&'a LogSink>,
    /// Parse `-progress pipe:1` output on stdout; called with `out_time` in seconds.
    pub on_progress: Option<&'a mut dyn FnMut(f64)>,
    /// Data written to the child's stdin.
    pub stdin: Option<Vec<u8>>,
    /// Do not log stderr lines (e.g. huge outputs that are parsed instead).
    pub quiet_stderr: bool,
}

#[derive(Debug)]
pub struct RunOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl RunOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Starts `cmd` with stdout piped for streaming (stdin and stderr closed), for
/// outputs too large to collect, such as decoded audio.
pub fn spawn_stdout(cmd: &Cmd) -> Result<Child> {
    tracing::debug!("spawn: {}", cmd.display());
    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map_err(|source| Error::Spawn {
        program: cmd.program.to_string_lossy().into_owned(),
        source,
    })
}

/// Runs `cmd` to completion. Returns `Err(Cancelled)` if cancelled; a non-zero
/// exit code is *not* an error here (see [`run_checked`]).
pub fn run(cmd: &Cmd, mut opts: RunOpts<'_>) -> Result<RunOutput> {
    if let Some(c) = opts.cancel {
        c.check()?;
    }
    let line = cmd.display();
    tracing::debug!("run: {line}");
    if let Some(log) = opts.log {
        log(LogLine::Command(&line));
    }

    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(if opts.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|source| Error::Spawn {
        program: cmd.program.to_string_lossy().into_owned(),
        source,
    })?;

    if let Some(data) = opts.stdin.take() {
        let mut stdin = child.stdin.take().expect("piped stdin");
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }

    // stderr: collected on a thread (also forwarded to the log)
    let stderr_pipe = child.stderr.take().expect("piped stderr");
    let log_for_err = if opts.quiet_stderr {
        None
    } else {
        opts.log.cloned()
    };
    let stderr_thread = std::thread::spawn(move || {
        let mut all = String::new();
        let mut reader = BufReader::new(stderr_pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let l = String::from_utf8_lossy(&buf);
                    if let Some(log) = &log_for_err {
                        log(LogLine::Stderr(l.trim_end()));
                    }
                    all.push_str(&l);
                }
            }
        }
        all
    });

    // stdout: either progress lines (sent through a channel) or raw bytes
    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel::<f64>();
    let parse_progress = opts.on_progress.is_some();
    let stdout_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout_pipe);
        let mut raw = Vec::new();
        if parse_progress {
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if let Some(v) = line.trim().strip_prefix("out_time_us=")
                    && let Ok(us) = v.parse::<i64>()
                {
                    let _ = tx.send(us as f64 / 1e6);
                }
                line.clear();
            }
        } else {
            let _ = reader.read_to_end(&mut raw);
        }
        raw
    });

    let status = supervise(
        &mut child,
        opts.cancel,
        |t| {
            if let Some(cb) = opts.on_progress.as_mut() {
                cb(t)
            }
        },
        &rx,
    );
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    // drain the last progress updates
    while let Ok(t) = rx.try_recv() {
        if let Some(cb) = opts.on_progress.as_mut() {
            cb(t)
        }
    }
    let code = status?;
    Ok(RunOutput {
        code,
        stdout,
        stderr,
    })
}

fn supervise(
    child: &mut Child,
    cancel: Option<&CancelToken>,
    mut on_progress: impl FnMut(f64),
    rx: &mpsc::Receiver<f64>,
) -> Result<Option<i32>> {
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(t) => on_progress(t),
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        if cancel.is_some_and(CancelToken::is_cancelled) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Cancelled);
        }
        if let Some(st) = child.try_wait()? {
            return Ok(st.code());
        }
    }
}

/// Like [`run`], but a non-zero exit code becomes [`Error::CommandFailed`].
pub fn run_checked(cmd: &Cmd, opts: RunOpts<'_>) -> Result<RunOutput> {
    let out = run(cmd, opts)?;
    if !out.success() {
        return Err(Error::CommandFailed {
            cmd: cmd.display(),
            code: out.code,
            stderr_tail: tail_lines(&out.stderr, 15),
        });
    }
    Ok(out)
}

pub fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Standard leading ffmpeg arguments for jobs with progress reporting.
pub fn ffmpeg_base(tools: &Tools) -> Cmd {
    tools.ffmpeg().args([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-progress",
        "pipe:1",
        "-nostats",
    ])
}
