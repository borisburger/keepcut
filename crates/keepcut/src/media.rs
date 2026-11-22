//! Background probing (max. 2 at a time) and encoder detection; results come
//! back through channels and wake the UI.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, RwLock};

use keepcut_core::encoders::{self, EncoderSet};
use keepcut_core::ffmpeg::Tools;
use keepcut_core::probe::{self, MediaInfo};

pub type ProbeResult = (PathBuf, Result<Arc<MediaInfo>, String>);

pub struct ProbeService {
    queue: Arc<(Mutex<VecDeque<PathBuf>>, Condvar)>,
    tools: Arc<RwLock<Option<Tools>>>,
    pub results: Receiver<ProbeResult>,
}

const WORKERS: usize = 2;

impl ProbeService {
    pub fn new(tools: Option<Tools>, ctx: egui::Context) -> ProbeService {
        let queue: Arc<(Mutex<VecDeque<PathBuf>>, Condvar)> = Arc::default();
        let tools = Arc::new(RwLock::new(tools));
        let (tx, rx) = channel();
        for i in 0..WORKERS {
            let (queue, tx, tools, ctx) = (queue.clone(), tx.clone(), tools.clone(), ctx.clone());
            std::thread::Builder::new()
                .name(format!("probe-{i}"))
                .spawn(move || worker(queue, tx, tools, ctx))
                .expect("spawning probe worker");
        }
        ProbeService {
            queue,
            tools,
            results: rx,
        }
    }

    /// ffmpeg was found somewhere else (Settings).
    pub fn set_tools(&self, tools: Option<Tools>) {
        *self.tools.write().unwrap() = tools;
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
    tools: Arc<RwLock<Option<Tools>>>,
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
        let tools = tools.read().unwrap().clone();
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

/// Thumbnails and waveforms (§8.2, §8.3), computed in the background and
/// cached in `<cache dir>` by path + size + date. Two workers, so a long
/// waveform never holds up the thumbnails.
pub struct AssetService {
    thumbs: Arc<(Mutex<VecDeque<Arc<MediaInfo>>>, Condvar)>,
    waves: Arc<(Mutex<VecDeque<Arc<MediaInfo>>>, Condvar)>,
    tools: Arc<RwLock<Option<Tools>>>,
    pub results: Receiver<AssetResult>,
}

pub enum AssetResult {
    /// RGBA, `assets::THUMB_W`×`THUMB_H` (`None`: failed).
    Thumb(PathBuf, Option<Vec<u8>>),
    /// Peaks per 10 ms (`None`: failed; empty: no audio).
    Wave(PathBuf, Option<Arc<Vec<u8>>>),
}

#[derive(Clone, Copy)]
enum Kind {
    Thumb,
    Wave,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Thumb => "thumbs",
            Kind::Wave => "waves",
        }
    }
}

fn cache_file(kind: Kind, info: &MediaInfo) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&info.path, info.file_size, info.file_mtime).hash(&mut h);
    crate::config::dirs().map(|d| d.cache_dir().join(kind.dir()).join(format!("{:016x}", h.finish())))
}

type Queue = Arc<(Mutex<VecDeque<Arc<MediaInfo>>>, Condvar)>;

impl AssetService {
    pub fn new(tools: Option<Tools>, ctx: egui::Context) -> AssetService {
        let (tx, rx) = channel();
        let tools = Arc::new(RwLock::new(tools));
        let thumbs: Queue = Arc::default();
        let waves: Queue = Arc::default();
        for (kind, q) in [(Kind::Thumb, thumbs.clone()), (Kind::Wave, waves.clone())] {
            let (tx, tools, ctx) = (tx.clone(), tools.clone(), ctx.clone());
            std::thread::Builder::new()
                .name(format!("assets-{}", kind.dir()))
                .spawn(move || asset_worker(kind, q, tx, tools, ctx))
                .expect("spawning asset worker");
        }
        AssetService {
            thumbs,
            waves,
            tools,
            results: rx,
        }
    }

    pub fn set_tools(&self, tools: Option<Tools>) {
        *self.tools.write().unwrap() = tools;
    }

    pub fn request(&self, info: Arc<MediaInfo>) {
        for q in [&self.thumbs, &self.waves] {
            let (lock, cvar) = &**q;
            lock.lock().unwrap().push_back(info.clone());
            cvar.notify_one();
        }
    }

    /// The selected clip's waveform goes first.
    pub fn prioritize_wave(&self, path: &std::path::Path) {
        let (lock, _) = &*self.waves;
        let mut q = lock.lock().unwrap();
        if let Some(i) = q.iter().position(|i| i.path == path) {
            let item = q.remove(i).unwrap();
            q.push_front(item);
        }
    }
}

fn asset_worker(
    kind: Kind,
    queue: Queue,
    tx: Sender<AssetResult>,
    tools: Arc<RwLock<Option<Tools>>>,
    ctx: egui::Context,
) {
    use keepcut_core::assets;
    use keepcut_core::ffmpeg::CancelToken;
    loop {
        let info = {
            let (lock, cvar) = &*queue;
            let mut q = lock.lock().unwrap();
            loop {
                if let Some(p) = q.pop_front() {
                    break p;
                }
                q = cvar.wait(q).unwrap();
            }
        };
        let cache = cache_file(kind, &info);
        let cached = cache.as_ref().and_then(|f| std::fs::read(f).ok());
        let tools = tools.read().unwrap().clone();
        let data: Option<Vec<u8>> = match (cached, &tools) {
            (Some(d), _) => Some(d),
            (None, Some(t)) => {
                let r = match kind {
                    Kind::Thumb => assets::thumbnail(t, &info),
                    Kind::Wave => assets::waveform(t, &info, &CancelToken::new()),
                };
                match r {
                    Ok(d) => {
                        if let Some(f) = &cache {
                            let _ = std::fs::create_dir_all(f.parent().unwrap_or(f));
                            let _ = std::fs::write(f, &d);
                        }
                        Some(d)
                    }
                    Err(e) => {
                        tracing::warn!("{} of {}: {e}", kind.dir(), info.path.display());
                        None
                    }
                }
            }
            (None, None) => None,
        };
        let path = info.path.clone();
        let msg = match kind {
            Kind::Thumb => AssetResult::Thumb(path, data),
            Kind::Wave => AssetResult::Wave(path, data.map(Arc::new)),
        };
        if tx.send(msg).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}
