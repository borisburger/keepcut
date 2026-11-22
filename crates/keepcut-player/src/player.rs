//! mpv client wrapper: options, commands, and an event thread that forwards
//! property changes to the UI through a channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use anyhow::{Context as _, Result};
use libmpv2::events::{Event, PropertyData};
use libmpv2::{Format, Mpv};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderKind {
    Gl,
    Sw,
}

#[derive(Debug)]
pub enum PlayerEvent {
    Prop(&'static str, Value),
    PlaybackRestart,
    FileLoaded,
    EndFile,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Value {
    F64(f64),
    I64(i64),
    Bool(bool),
    Str(String),
    None,
}

/// Properties observed by the event thread (name, format).
const OBSERVED: &[(&str, Format)] = &[
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("pause", Format::Flag),
    ("eof-reached", Format::Flag),
    ("hwdec-current", Format::String),
    ("dwidth", Format::Int64),
    ("dheight", Format::Int64),
    ("estimated-vf-fps", Format::Double),
    ("frame-drop-count", Format::Int64),
    ("decoder-frame-drop-count", Format::Int64),
    ("video-codec", Format::String),
];

/// Latest values of the observed properties.
#[derive(Debug, Default, Clone)]
pub struct State {
    pub time_pos: f64,
    pub duration: f64,
    pub pause: bool,
    pub eof: bool,
    pub hwdec: String,
    pub dsize: [i64; 2],
    pub vf_fps: f64,
    pub vo_drops: i64,
    pub dec_drops: i64,
    pub codec: String,
}

impl State {
    pub fn apply(&mut self, name: &str, v: &Value) {
        match (name, v) {
            ("time-pos", Value::F64(x)) => self.time_pos = *x,
            ("duration", Value::F64(x)) => self.duration = *x,
            ("pause", Value::Bool(b)) => self.pause = *b,
            ("eof-reached", Value::Bool(b)) => self.eof = *b,
            ("hwdec-current", Value::Str(s)) => self.hwdec = s.clone(),
            ("hwdec-current", Value::None) => self.hwdec = "no".into(),
            ("dwidth", Value::I64(x)) => self.dsize[0] = *x,
            ("dheight", Value::I64(x)) => self.dsize[1] = *x,
            ("estimated-vf-fps", Value::F64(x)) => self.vf_fps = *x,
            ("frame-drop-count", Value::I64(x)) => self.vo_drops = *x,
            ("decoder-frame-drop-count", Value::I64(x)) => self.dec_drops = *x,
            ("video-codec", Value::Str(s)) => self.codec = s.clone(),
            _ => {}
        }
    }
}

pub struct Player {
    pub mpv: Mpv,
    pub events: Receiver<PlayerEvent>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub state: State,
}

impl Player {
    pub fn new(kind: RenderKind, hwdec: Option<&str>, egui_ctx: egui::Context) -> Result<Player> {
        let default_hwdec = if kind == RenderKind::Gl {
            "auto-safe"
        } else {
            "auto-copy-safe"
        };
        let hwdec = hwdec.unwrap_or(default_hwdec);
        let mpv = Mpv::with_initializer(|init| {
            let opts: &[(&str, &str)] = &[
                ("vo", "libmpv"),
                // GL path: zero-copy hwdec where interop exists, else mpv falls back.
                // SW path: frames must reach the CPU anyway.
                ("hwdec", hwdec),
                ("keep-open", "always"),
                ("idle", "yes"),
                ("pause", "yes"),
                ("hr-seek", "yes"),
                ("hr-seek-framedrop", "no"),
                ("osc", "no"),
                ("input-default-bindings", "no"),
                ("input-vo-keyboard", "no"),
                ("audio-display", "no"),
                ("force-window", "no"),
                ("terminal", "no"),
                ("demuxer-max-bytes", "500MiB"),
                ("demuxer-max-back-bytes", "300MiB"),
            ];
            // Options that a libmpv built without some features doesn't have: "osc" is the
            // Lua on-screen controller (absent with -Dlua=disabled, as in the Flatpak).
            const OPTIONAL: &[&str] = &["osc"];
            for (k, v) in opts {
                if let Err(e) = init.set_property(k, *v) {
                    if OPTIONAL.contains(k) {
                        tracing::debug!("mpv option {k}={v} not available: {e}");
                        continue;
                    }
                    tracing::error!("mpv option {k}={v}: {e}");
                    return Err(e);
                }
            }
            Ok(())
        })
        .context("creating mpv")?;

        let client = mpv
            .create_client(Some("keepcut_events"))
            .context("mpv client")?;
        for (i, (name, fmt)) in OBSERVED.iter().enumerate() {
            client.observe_property(name, *fmt, i as u64)?;
        }
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("mpv-events".into())
                .spawn(move || event_loop(client, tx, stop, egui_ctx))?
        };
        Ok(Player {
            mpv,
            events: rx,
            stop,
            thread: Some(thread),
            state: State::default(),
        })
    }

    pub fn command(&self, name: &str, args: &[&str]) {
        if let Err(e) = self.mpv.command(name, args) {
            tracing::warn!("mpv {name} {args:?}: {e}");
        }
    }

    pub fn load(&self, url: &str) {
        self.command("loadfile", &[url]);
    }

    pub fn seek_exact(&self, t: f64) {
        self.command("seek", &[&format!("{t:.6}"), "absolute+exact"]);
    }

    /// Drains pending events into `state`; returns them for further handling.
    pub fn poll(&mut self) -> Vec<PlayerEvent> {
        let evs: Vec<PlayerEvent> = self.events.try_iter().collect();
        for e in &evs {
            if let PlayerEvent::Prop(n, v) = e {
                self.state.apply(n, v);
            }
        }
        evs
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the event thread
        let _ = self.mpv.command("ignore", &[]);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn event_loop(
    client: Mpv,
    tx: Sender<PlayerEvent>,
    stop: Arc<AtomicBool>,
    egui_ctx: egui::Context,
) {
    while !stop.load(Ordering::SeqCst) {
        let Some(ev) = client.wait_event(0.25) else {
            continue;
        };
        let out = match ev {
            Ok(Event::PropertyChange {
                reply_userdata,
                change,
                ..
            }) => {
                let name = OBSERVED
                    .get(reply_userdata as usize)
                    .map_or("?", |(n, _)| *n);
                let v = match change {
                    PropertyData::Double(x) => Value::F64(x),
                    PropertyData::Int64(x) => Value::I64(x),
                    PropertyData::Flag(b) => Value::Bool(b),
                    PropertyData::Str(s) | PropertyData::OsdStr(s) => Value::Str(s.to_string()),
                    _ => Value::None,
                };
                Some(PlayerEvent::Prop(name, v))
            }
            Ok(Event::PlaybackRestart) => Some(PlayerEvent::PlaybackRestart),
            Ok(Event::FileLoaded) => Some(PlayerEvent::FileLoaded),
            Ok(Event::EndFile(_)) => Some(PlayerEvent::EndFile),
            Ok(Event::Shutdown) => {
                let _ = tx.send(PlayerEvent::Shutdown);
                break;
            }
            _ => None,
        };
        if let Some(e) = out {
            if tx.send(e).is_err() {
                break;
            }
            egui_ctx.request_repaint();
        }
    }
}

/// `edl://` URL playing `[start, end)` ranges of files back to back.
/// Paths use mpv's `%<byte length>%` quoting, so commas/semicolons are safe.
pub fn edl_url(items: &[(String, f64, f64)]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|(p, s, e)| format!("%{}%{},start={:.6},length={:.6}", p.len(), p, s, e - s))
        .collect();
    format!("edl://{}", parts.join(";"))
}
