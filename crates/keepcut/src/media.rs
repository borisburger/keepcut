//! Background probing (max. 2 at a time) and encoder detection; results come
//! back through channels and wake the UI.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use keepcut_core::encoders::{self, EncoderSet};
use keepcut_core::ffmpeg::Tools;
use keepcut_core::probe::{self, MediaInfo};

pub type ProbeResult = (PathBuf, Result<Arc<MediaInfo>, String>);

pub struct ProbeService {
    queue: Arc<(Mutex<VecDeque<PathBuf>>, Condvar)>,
    pub results: Receiver<ProbeResult>,
}

const WORKERS: usize = 2;

impl ProbeService {
    pub fn new(tools: Option<Tools>, ctx: egui::Context) -> ProbeService {
        let queue: Arc<(Mutex<VecDeque<PathBuf>>, Condvar)> = Arc::default();
        let (tx, rx) = channel();
        for i in 0..WORKERS {
            let (queue, tx, tools, ctx) = (queue.clone(), tx.clone(), tools.clone(), ctx.clone());
            std::thread::Builder::new()
                .name(format!("probe-{i}"))
                .spawn(move || worker(queue, tx, tools, ctx))
                .expect("spawning probe worker");
        }
        ProbeService { queue, results: rx }
    }

    pub fn enqueue(&self, path: PathBuf) {
        let (lock, cvar) = &*self.queue;
        lock.lock().unwrap().push_back(path);
        cvar.notify_one();
    }
}

fn worker(
    queue: Arc<(Mutex<VecDeque<PathBuf>>, Condvar)>,
    tx: Sender<ProbeResult>,
    tools: Option<Tools>,
    ctx: egui::Context,
) {
    loop {
        let path = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap();
            loop {
                if let Some(p) = q.pop_front() {
                    break p;
                }
                q = cvar.wait(q).unwrap();
            }
        };
        let r = match &tools {
            Some(t) => probe::probe(t, &path, None, None)
                .map(Arc::new)
                .map_err(|e| e.to_string()),
            None => Err("ffprobe not found (looked next to Keepcut and on PATH)".into()),
        };
        if tx.send((path, r)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// Runs encoder detection once in the background.
pub fn detect_encoders(tools: Option<Tools>, ctx: egui::Context) -> Receiver<EncoderSet> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let set = match tools {
            Some(t) => {
                encoders::detect(&t, None, None).unwrap_or_else(|_| EncoderSet::software_only())
            }
            None => EncoderSet::software_only(),
        };
        tracing::info!("working encoders: {}", set.working.join(", "));
        let _ = tx.send(set);
        ctx.request_repaint();
    });
    rx
}
