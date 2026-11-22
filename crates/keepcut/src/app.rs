//! Application state, edits (with undo), mpv loading/previews, layout and shortcuts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui;
use keepcut_core::encoders::EncoderSet;
use keepcut_core::ffmpeg::Tools;
use keepcut_core::history::History;
use keepcut_core::model::{Clip, Project, Segment};
use keepcut_core::plan::{self, ExportPlan};
use keepcut_core::probe::MediaInfo;
use keepcut_core::{edit, timefmt};
use keepcut_player::player::{self, Player, PlayerEvent, RenderKind};
use keepcut_player::render::{GlRenderer, SwRenderer};
use uuid::Uuid;

use crate::config::{Config, RenderPath};
use crate::export::ExportUi;
use crate::media::{self, ProbeService};
use crate::timeline::TimelineView;

pub enum Renderer {
    Gl(GlRenderer),
    Sw(SwRenderer),
}

/// Probe state of a source file. Kept per *path* (not per clip), so removing,
/// undoing or redoing a clip never loses or strands a probe result.
#[derive(Clone)]
pub enum ProbeState {
    Pending,
    Ready(Arc<MediaInfo>),
    Failed(String),
    /// The file is gone (a project opened after files moved): relink or remove.
    Missing,
}

/// One kept range in an EDL preview: `len` seconds of `clip`/`seg` starting at
/// `src_start` play at preview time `out_start`.
#[derive(Debug, Clone)]
pub struct Span {
    pub clip: Uuid,
    pub seg: Uuid,
    pub out_start: f64,
    pub src_start: f64,
    pub len: f64,
}

/// What mpv currently plays.
#[derive(Debug, Clone)]
pub enum Loaded {
    Nothing,
    Clip(Uuid),
    /// Cut preview of one clip, or "Preview all".
    Preview {
        clip: Option<Uuid>,
        spans: Vec<Span>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// A segment edge being dragged on the timeline (one undo step on release).
pub struct Drag {
    pub clip: Uuid,
    pub seg: Uuid,
    pub edge: Edge,
    pub before: Vec<Clip>,
}

pub struct App {
    // Declared before `player`: the render context must go before the mpv handle.
    pub renderer: Option<Renderer>,
    pub player: Player,
    pub render_kind: RenderKind,
    pub tools: Option<Tools>,
    pub config: Config,
    pub project: Project,
    pub probes: HashMap<PathBuf, ProbeState>,
    /// Playhead to restore when a clip is selected again.
    pub last_pos: HashMap<Uuid, f64>,
    pub probe: ProbeService,
    pub assets: media::AssetService,
    /// Clip thumbnails by file (`None`: could not be made).
    pub thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// Clips added but not yet probed: placed by recording time once probed.
    pub newly_added: std::collections::HashSet<PathBuf>,
    /// Waveform peaks by file (`None`: failed; empty: no audio).
    pub waves: HashMap<PathBuf, Option<Arc<Vec<u8>>>>,
    pub encoders: EncoderSet,
    encoders_rx: Option<Receiver<EncoderSet>>,
    pub history: History<Vec<Clip>>,
    pub selected: Option<Uuid>,
    pub selected_seg: Option<Uuid>,
    pub loaded: Loaded,
    pub timeline: TimelineView,
    pub drag: Option<Drag>,
    /// Plan for the current project (badges, status bar, export summary).
    pub plan: Option<Result<ExportPlan, String>>,
    plan_dirty: bool,
    pub export: ExportUi,
    pub status: String,
    status_at: Instant,
    /// A file dialog couldn't be shown (no xdg-desktop-portal); explain it.
    pub portal_notice: bool,
    /// Size the window to the screen once the monitor size is known (first run only).
    pub fit_window: bool,
    fit_window_tries: u32,
    /// A fast frame step (speed 32) is in flight.
    fast_step: Option<Instant>,
    pending_seek: Option<f64>,
    /// Target of the last seek until mpv reports playback-restart.
    seek_target: Option<f64>,
    seek_target_age: u32,
    /// Test automation (`--script`).
    pub script: Option<crate::script::Script>,
    pub title_dialog: Option<crate::clips::TitleDialog>,
    /// Clip list before an inline segment-title edit (one undo step per edit).
    pub title_before: Option<Vec<Clip>>,
    pub proj: crate::project::ProjectState,
    pub settings: crate::settings::SettingsUi,
    pub ctx: egui::Context,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> anyhow::Result<App> {
        let config = Config::load();
        let tools = crate::settings::locate_tools(config.ffmpeg_dir.as_deref()).ok();
        cc.egui_ctx.set_theme(config.theme.preference());
        cc.egui_ctx.set_fonts(fonts());
        let want_sw = config.render == RenderPath::Software;
        let mut kind = if want_sw {
            RenderKind::Sw
        } else {
            RenderKind::Gl
        };
        let mut player = Player::new(kind, None, cc.egui_ctx.clone())?;
        let renderer = match (kind, cc.gl.clone(), cc.get_proc_address.clone()) {
            (RenderKind::Gl, Some(gl), Some(gp)) => {
                match GlRenderer::new(&player.mpv, gl, gp, cc.egui_ctx.clone()) {
                    Ok(r) => Renderer::Gl(r),
                    Err(e) => {
                        tracing::warn!("OpenGL video path failed ({e}); using the software path");
                        kind = RenderKind::Sw;
                        drop(player);
                        player = Player::new(kind, None, cc.egui_ctx.clone())?;
                        Renderer::Sw(SwRenderer::new(&player.mpv, cc.egui_ctx.clone())?)
                    }
                }
            }
            _ => {
                kind = RenderKind::Sw;
                Renderer::Sw(SwRenderer::new(&player.mpv, cc.egui_ctx.clone())?)
            }
        };
        let probe = ProbeService::new(tools.clone(), cc.egui_ctx.clone());
        let assets = media::AssetService::new(tools.clone(), cc.egui_ctx.clone());
        let encoders_rx = Some(media::detect_encoders(tools.clone(), cc.egui_ctx.clone()));
        let mut project = Project {
            version: keepcut_core::model::PROJECT_VERSION,
            id: Uuid::new_v4(),
            clips: Vec::new(),
            export: Default::default(),
        };
        config.apply_export_defaults(&mut project.export);
        let mut app = App {
            renderer: Some(renderer),
            player,
            render_kind: kind,
            tools,
            config,
            project,
            probes: HashMap::new(),
            last_pos: HashMap::new(),
            probe,
            assets,
            thumbs: HashMap::new(),
            newly_added: Default::default(),
            waves: HashMap::new(),
            encoders: EncoderSet::software_only(),
            encoders_rx,
            history: History::new(200),
            selected: None,
            selected_seg: None,
            loaded: Loaded::Nothing,
            timeline: TimelineView::default(),
            drag: None,
            plan: None,
            plan_dirty: true,
            export: ExportUi::default(),
            status: String::new(),
            status_at: Instant::now(),
            portal_notice: false,
            fit_window: false,
            fit_window_tries: 0,
            fast_step: None,
            pending_seek: None,
            seek_target: None,
            seek_target_age: 0,
            script: None,
            title_dialog: None,
            title_before: None,
            proj: Default::default(),
            settings: Default::default(),
            ctx: cc.egui_ctx.clone(),
        };
        if app.tools.is_none() {
            app.set_status("ffmpeg/ffprobe not found: install ffmpeg, put it next to Keepcut, or choose its folder in Settings");
        }
        // a project file on the command line opens it; anything else is added as clips
        let (projects, clips): (Vec<PathBuf>, Vec<PathBuf>) =
            files.into_iter().partition(|f| is_project_file(f));
        if let Some(p) = projects.first() {
            app.open_project(p);
        }
        app.add_files(clips);
        Ok(app)
    }

    pub fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_at = Instant::now();
    }

    // ------------------------------------------------------------------
    // Clips and edits
    // ------------------------------------------------------------------

    /// Adds files as clips (one undo step for the whole batch).
    pub fn add_files(&mut self, files: Vec<PathBuf>) {
        let before = self.project.clips.clone();
        let mut added = 0;
        for f in files {
            let f = std::path::absolute(&f).unwrap_or(f);
            if !f.is_file() || self.project.clips.iter().any(|c| c.path == f) {
                continue;
            }
            self.project.clips.push(Clip {
                id: Uuid::new_v4(),
                path: f.clone(),
                file_size: 0,
                file_mtime: 0,
                enabled: true,
                // None = "the whole clip"; becomes one real segment once probed
                segments: None,
                chapter_title: None,
            });
            if !self.probes.contains_key(&f) {
                self.probes.insert(f.clone(), ProbeState::Pending);
                self.probe.enqueue(f.clone());
            }
            self.newly_added.insert(f.clone());
            self.sort_new_clip(&f);
            added += 1;
        }
        if added > 0 {
            self.history.push(before);
            self.fill_whole_clip_segments();
            self.plan_dirty = true;
            if self.selected.is_none() {
                self.select_clip(self.project.clips.first().map(|c| c.id));
            }
        }
    }

    fn on_probed(&mut self, path: PathBuf, r: Result<Arc<MediaInfo>, String>) {
        if let Ok(info) = &r {
            self.check_changed_source(&path, info);
            // thumbnail and waveform, once per file (a re-probe after a change asks again)
            self.thumbs.remove(&path);
            self.waves.remove(&path);
            self.assets.request(info.clone());
            if self.clip_path(self.selected) == Some(&path) {
                self.assets.prioritize_wave(&path);
            }
        }
        self.probes.insert(
            path.clone(),
            match r {
                Ok(info) => ProbeState::Ready(info),
                Err(e) => ProbeState::Failed(e),
            },
        );
        self.fill_whole_clip_segments();
        // only clips added just now are placed by recording time; an opened
        // project keeps its order
        if self.newly_added.remove(&path) {
            self.sort_new_clip(&path);
        }
        self.plan_dirty = true;
    }

    /// Clips whose segments are `None` ("whole clip", e.g. not yet probed when
    /// added, or restored by undo) get one real segment once their file is probed.
    pub fn fill_whole_clip_segments(&mut self) {
        for c in &mut self.project.clips {
            if let Some(ProbeState::Ready(info)) = self.probes.get(&c.path) {
                c.file_size = info.file_size;
                c.file_mtime = info.file_mtime;
                if c.segments.is_none() {
                    c.segments = Some(vec![Segment::new(0.0, info.video_end())]);
                }
            }
        }
    }

    pub fn probe_state(&self, id: Uuid) -> Option<&ProbeState> {
        self.probes.get(&self.clip(id)?.path)
    }

    pub fn probe_error(&self, id: Uuid) -> Option<String> {
        match self.probe_state(id)? {
            ProbeState::Failed(e) => Some(e.clone()),
            ProbeState::Missing => Some("file not found (right-click the clip: Locate…)".into()),
            _ => None,
        }
    }

    /// Clips of the project whose files are still being read.
    pub fn pending_count(&self) -> usize {
        self.project
            .clips
            .iter()
            .filter(|c| matches!(self.probes.get(&c.path), Some(ProbeState::Pending) | None))
            .count()
    }

    pub fn unreadable_count(&self) -> usize {
        self.project
            .clips
            .iter()
            .filter(|c| {
                matches!(
                    self.probes.get(&c.path),
                    Some(ProbeState::Failed(_) | ProbeState::Missing)
                )
            })
            .count()
    }

    /// New clips are placed by recording time once their metadata is known.
    fn sort_new_clip(&mut self, path: &PathBuf) {
        let Some(pos) = self.project.clips.iter().position(|c| &c.path == path) else {
            return;
        };
        let Some(t) = self.recording_time(self.project.clips[pos].id) else {
            return;
        };
        let clip = self.project.clips.remove(pos);
        let mut target = self.project.clips.len();
        for (i, c) in self.project.clips.iter().enumerate() {
            if let Some(ct) = self.recording_time(c.id)
                && ct > t
            {
                target = i;
                break;
            }
        }
        self.project.clips.insert(target, clip);
    }

    pub fn recording_time(&self, id: Uuid) -> Option<f64> {
        self.info(id).map(|i| i.recording_time())
    }

    pub fn sort_by_recording_time(&mut self) {
        let before = self.project.clips.clone();
        let mut clips = self.project.clips.clone();
        clips.sort_by(|a, b| {
            let ta = self.recording_time(a.id).unwrap_or(f64::MAX);
            let tb = self.recording_time(b.id).unwrap_or(f64::MAX);
            ta.total_cmp(&tb)
        });
        if clips.iter().map(|c| c.id).ne(before.iter().map(|c| c.id)) {
            self.history.push(before);
            self.project.clips = clips;
            self.plan_dirty = true;
        }
    }

    pub fn clip_path(&self, id: Option<Uuid>) -> Option<&PathBuf> {
        self.clip(id?).map(|c| &c.path)
    }

    fn on_asset(&mut self, ctx: &egui::Context, r: media::AssetResult) {
        use keepcut_core::assets::{THUMB_H, THUMB_W};
        match r {
            media::AssetResult::Thumb(path, data) => {
                let tex = data
                    .filter(|d| d.len() == (THUMB_W * THUMB_H * 4) as usize)
                    .map(|d| {
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [THUMB_W as usize, THUMB_H as usize],
                            &d,
                        );
                        ctx.load_texture(
                            format!("thumb:{}", path.display()),
                            img,
                            egui::TextureOptions::LINEAR,
                        )
                    });
                self.thumbs.insert(path, tex);
            }
            media::AssetResult::Wave(path, data) => {
                self.waves.insert(path, data);
            }
        }
    }

    pub fn clip(&self, id: Uuid) -> Option<&Clip> {
        self.project.clips.iter().find(|c| c.id == id)
    }

    pub fn info(&self, id: Uuid) -> Option<Arc<MediaInfo>> {
        match self.probe_state(id)? {
            ProbeState::Ready(i) => Some(i.clone()),
            _ => None,
        }
    }

    pub fn segments(&self, id: Uuid) -> &[Segment] {
        self.clip(id)
            .and_then(|c| c.segments.as_deref())
            .unwrap_or(&[])
    }

    /// Applies an edit to the clip list as one undo step; `f` returns whether
    /// anything changed.
    pub fn edit(&mut self, f: impl FnOnce(&mut Vec<Clip>) -> bool) -> bool {
        let before = self.project.clips.clone();
        if f(&mut self.project.clips) {
            self.history.push(before);
            self.after_edit();
            true
        } else {
            self.project.clips = before;
            false
        }
    }

    /// Edits the segments of the selected clip.
    pub fn edit_segments(&mut self, f: impl FnOnce(&mut Vec<Segment>) -> Option<usize>) {
        let Some(id) = self.selected else { return };
        let mut new_sel = None;
        self.edit(|clips| {
            let Some(c) = clips.iter_mut().find(|c| c.id == id) else {
                return false;
            };
            let segs = c.segments.get_or_insert_with(Vec::new);
            match f(segs) {
                Some(i) => {
                    new_sel = segs.get(i).map(|s| s.id);
                    true
                }
                None => false,
            }
        });
        if new_sel.is_some() {
            self.selected_seg = new_sel;
        }
    }

    fn after_edit(&mut self) {
        self.plan_dirty = true;
        // an edit invalidates a running preview: leave it where it was playing
        if matches!(self.loaded, Loaded::Preview { .. }) {
            self.leave_preview();
        }
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.history.undo(self.project.clips.clone()) {
            self.project.clips = prev;
            self.after_undo_redo();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo(self.project.clips.clone()) {
            self.project.clips = next;
            self.after_undo_redo();
        }
    }

    fn after_undo_redo(&mut self) {
        // snapshots may hold "whole clip" (None) segments of clips probed since
        self.fill_whole_clip_segments();
        if self.selected.is_none_or(|s| self.clip(s).is_none()) {
            self.select_clip(self.project.clips.first().map(|c| c.id));
        }
        if let Some(s) = self.selected_seg
            && !self
                .selected
                .is_some_and(|c| self.segments(c).iter().any(|x| x.id == s))
        {
            self.selected_seg = None;
        }
        self.after_edit();
    }

    pub fn remove_clip(&mut self, id: Uuid) {
        self.edit(|clips| {
            let n = clips.len();
            clips.retain(|c| c.id != id);
            clips.len() != n
        });
        if self.selected == Some(id) {
            self.select_clip(self.project.clips.first().map(|c| c.id));
        }
    }

    // ------------------------------------------------------------------
    // Player
    // ------------------------------------------------------------------

    /// Current position in source time of the selected clip. During a preview
    /// this maps the preview time back (Preview all keeps the selection on the
    /// clip that is playing, see `follow_preview`).
    pub fn source_time(&self) -> f64 {
        let t = self.player.state.time_pos;
        match &self.loaded {
            Loaded::Preview { .. } => self.preview_to_source(t).map_or(t, |(_, _, s)| s),
            _ => t,
        }
    }

    /// Preview time → (clip, segment, source time).
    pub fn preview_to_source(&self, t: f64) -> Option<(Uuid, Uuid, f64)> {
        let Loaded::Preview { spans, .. } = &self.loaded else {
            return None;
        };
        let s = spans
            .iter()
            .rev()
            .find(|s| s.out_start <= t + 1e-6)
            .or(spans.first())?;
        Some((
            s.clip,
            s.seg,
            s.src_start + (t - s.out_start).clamp(0.0, s.len),
        ))
    }

    /// In "Preview all", select the clip that is currently playing, so edits
    /// and the timeline refer to it.
    fn follow_preview(&mut self) {
        if let Loaded::Preview { clip: None, .. } = self.loaded
            && let Some((clip, seg, _)) = self.preview_to_source(self.player.state.time_pos)
            && self.selected != Some(clip)
        {
            self.selected = Some(clip);
            self.selected_seg = Some(seg);
            self.timeline = TimelineView::default();
        }
    }

    pub fn select_clip(&mut self, id: Option<Uuid>) {
        if id.is_some()
            && self.selected == id
            && matches!(self.loaded, Loaded::Clip(c) if Some(c) == id)
        {
            return; // already showing it: keep the playhead
        }
        self.remember_position();
        if self.selected != id {
            self.selected_seg = None;
            self.timeline = TimelineView::default();
        }
        self.selected = id;
        if let Some(p) = self.clip_path(id).cloned() {
            self.assets.prioritize_wave(&p);
        }
        self.reload_selected();
    }

    /// Stores the playhead of the selected clip (also from inside a preview).
    fn remember_position(&mut self) {
        let Some(cur) = self.selected else { return };
        let pos = match &self.loaded {
            Loaded::Clip(c) if *c == cur => Some(self.player.state.time_pos),
            Loaded::Preview { .. } => self
                .preview_to_source(self.player.state.time_pos)
                .filter(|(c, _, _)| *c == cur)
                .map(|(_, _, t)| t),
            _ => None,
        };
        if let Some(p) = pos {
            self.last_pos.insert(cur, p.max(0.0));
        }
    }

    fn cancel_fast_step(&mut self) {
        if self.fast_step.take().is_some() {
            self.player.command("set", &["speed", "1"]);
        }
    }

    /// Loads the selected clip into mpv (keeping its last playhead).
    pub fn reload_selected(&mut self) {
        self.cancel_fast_step();
        self.pending_seek = None;
        self.seek_target = None;
        let Some(id) = self.selected else {
            self.player.command("stop", &[]);
            self.loaded = Loaded::Nothing;
            return;
        };
        let Some(c) = self.clip(id) else { return };
        let path = c.path.to_string_lossy().into_owned();
        let pos = self.last_pos.get(&id).copied().unwrap_or(0.0).max(0.0);
        self.player.command(
            "loadfile",
            &[&path, "replace", "-1", &format!("start={pos:.6}")],
        );
        self.player.command("set", &["pause", "yes"]);
        // until mpv reports on the new file, don't keep the previous file's position
        self.player.state.time_pos = pos;
        self.loaded = Loaded::Clip(id);
    }

    fn load_preview(&mut self, clip: Option<Uuid>) {
        self.cancel_fast_step();
        self.remember_position();
        let clips: Vec<&Clip> = match clip {
            Some(id) => self.clip(id).into_iter().collect(),
            None => self.project.clips.iter().filter(|c| c.enabled).collect(),
        };
        let mut spans = Vec::new();
        let mut items = Vec::new();
        let mut t = 0.0;
        for c in clips {
            if self.info(c.id).is_none() {
                continue;
            }
            for s in c.segments.iter().flatten() {
                let len = s.end - s.start;
                spans.push(Span {
                    clip: c.id,
                    seg: s.id,
                    out_start: t,
                    src_start: s.start,
                    len,
                });
                items.push((c.path.to_string_lossy().into_owned(), s.start, s.end));
                t += len;
            }
        }
        if items.is_empty() {
            self.set_status("nothing to preview: no kept segments");
            return;
        }
        self.player.load(&player::edl_url(&items));
        self.player.command("set", &["pause", "no"]);
        self.player.state.time_pos = 0.0;
        self.loaded = Loaded::Preview { clip, spans };
    }

    /// Leaves a preview, landing on the selected clip where the preview was.
    fn leave_preview(&mut self) {
        self.remember_position();
        self.reload_selected();
    }

    pub fn toggle_cut_preview(&mut self) {
        match &self.loaded {
            Loaded::Preview { .. } => self.leave_preview(),
            _ => self.load_preview(self.selected),
        }
    }

    pub fn toggle_preview_all(&mut self) {
        match &self.loaded {
            Loaded::Preview { clip: None, .. } => self.leave_preview(),
            _ => self.load_preview(None),
        }
    }

    /// Seeks to a source time of the selected clip (leaving any preview).
    pub fn seek(&mut self, t: f64) {
        let t = t.max(0.0);
        if matches!(self.loaded, Loaded::Preview { .. }) {
            if let Some(id) = self.selected {
                self.last_pos.insert(id, t);
            }
            self.reload_selected();
            return;
        }
        self.pending_seek = Some(t);
    }

    fn flush_seek(&mut self) {
        // coalesce: one seek per UI frame while dragging
        if let Some(t) = self.pending_seek.take() {
            self.player.seek_exact(t);
            self.seek_target = Some(t);
            self.seek_target_age = 0;
        }
    }

    /// Where the playhead is, or will be once a pending/in-flight seek lands.
    fn effective_time(&self) -> f64 {
        self.pending_seek
            .or(self.seek_target)
            .unwrap_or_else(|| self.source_time())
    }

    /// Frame step (§3.5): forward plays the step at speed 32; backward seeks
    /// exactly to the previous frame from the frame index. While a seek is
    /// still landing, steps are computed from its target instead.
    pub fn step(&mut self, forward: bool) {
        if matches!(self.loaded, Loaded::Preview { .. }) {
            self.player.command(
                if forward {
                    "frame-step"
                } else {
                    "frame-back-step"
                },
                &[],
            );
            return;
        }
        let info = self.selected.and_then(|id| self.info(id));
        let seeking = self.pending_seek.is_some() || self.seek_target.is_some();
        match (forward, info) {
            (true, Some(info)) if seeking => {
                let fi = &info.frames;
                let t = self.effective_time();
                if let Some(next) = fi.frames.get(fi.idx_at_or_after(t + 1e-4)).map(|f| f.t) {
                    self.seek(next);
                }
            }
            (true, _) => {
                self.player.command("set", &["speed", "32"]);
                self.player.command("frame-step", &[]);
                self.fast_step = Some(Instant::now());
            }
            (false, Some(info)) => {
                if let Some(p) = info.frames.prev_frame_time(self.effective_time() - 1e-4) {
                    self.seek(p);
                }
            }
            (false, None) => self.player.command("frame-back-step", &[]),
        }
    }

    pub fn keyframe_jump(&mut self, forward: bool) {
        let Some(info) = self.selected.and_then(|id| self.info(id)) else {
            return;
        };
        let t = self.effective_time();
        let k = if forward {
            info.frames.next_key_at_or_after(t + 1e-3)
        } else {
            info.frames.last_key_at_or_before(t - 1e-3)
        };
        if let Some(k) = k {
            self.seek(k);
        }
    }

    pub fn play_pause(&mut self) {
        self.cancel_fast_step();
        self.player.command("cycle", &["pause"]);
    }

    /// Playhead snapped to the nearest frame of the selected clip.
    pub fn snapped_playhead(&self) -> Option<f64> {
        let info = self.info(self.selected?)?;
        Some(info.frames.snap(self.effective_time()))
    }

    pub fn set_start(&mut self) {
        let (Some(t), Some(info)) = (
            self.snapped_playhead(),
            self.selected.and_then(|i| self.info(i)),
        ) else {
            return;
        };
        let end = info.video_end();
        self.edit_segments(|s| edit::set_start(s, t, end));
    }

    pub fn set_end(&mut self) {
        let Some(t) = self.snapped_playhead() else {
            return;
        };
        self.edit_segments(|s| edit::set_end(s, t));
    }

    pub fn split(&mut self) {
        let Some(t) = self.snapped_playhead() else {
            return;
        };
        self.edit_segments(|s| edit::split(s, t));
    }

    pub fn delete_selected_segment(&mut self) {
        let Some(seg) = self.selected_seg else { return };
        self.edit_segments(|s| {
            let i = s.iter().position(|x| x.id == seg)?;
            edit::delete(s, i);
            // select the neighbour (index clamps below)
            Some(i.min(s.len().saturating_sub(1)))
        });
        if self.selected.is_some_and(|c| self.segments(c).is_empty()) {
            self.selected_seg = None;
        }
    }

    fn handle_player_events(&mut self) {
        for e in self.player.poll() {
            match e {
                PlayerEvent::Prop("time-pos", _) | PlayerEvent::Prop("pause", _) => {
                    if self.fast_step.is_some() && self.player.state.pause {
                        self.cancel_fast_step();
                    }
                }
                PlayerEvent::PlaybackRestart => self.seek_target = None,
                _ => {}
            }
        }
        if self
            .fast_step
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.cancel_fast_step();
        }
        // safety net in case a playback-restart event never arrives
        if self.seek_target.is_some() {
            self.seek_target_age += 1;
            if self.seek_target_age > 300 {
                self.seek_target = None;
            }
        }
        self.follow_preview();
    }

    // ------------------------------------------------------------------
    // Plan (badges, status, export summary)
    // ------------------------------------------------------------------

    /// Builds the plan over all probed clips. Unprobed/failed clips are left out.
    pub fn build_plan(&self) -> Option<Result<ExportPlan, String>> {
        let mut project = self.project.clone();
        project.clips.retain(|c| self.info(c.id).is_some());
        if project.clips.is_empty() {
            return None;
        }
        let infos: Vec<MediaInfo> = project
            .clips
            .iter()
            .map(|c| (*self.info(c.id).unwrap()).clone())
            .collect();
        Some(plan::build(&project, &infos, &self.encoders).map_err(|e| e.to_string()))
    }

    /// Plan-index → clip id mapping for verdicts.
    pub fn planned_clip_ids(&self) -> Vec<Uuid> {
        self.project
            .clips
            .iter()
            .filter(|c| self.info(c.id).is_some())
            .map(|c| c.id)
            .collect()
    }

    /// Runs the encoder detection again (Settings).
    pub fn redetect_encoders(&mut self) {
        self.encoders = EncoderSet::software_only();
        self.encoders_rx = Some(media::detect_encoders(self.tools.clone(), self.ctx.clone()));
        self.plan_dirty = true;
    }

    /// The hardware encoder test (startup) has finished.
    pub fn encoders_detected(&self) -> bool {
        self.encoders_rx.is_none()
    }

    pub fn mark_plan_dirty(&mut self) {
        self.plan_dirty = true;
    }

    fn refresh_plan(&mut self) {
        if self.plan_dirty && self.drag.is_none() {
            self.plan = self.build_plan();
            self.plan_dirty = false;
        }
    }

    // ------------------------------------------------------------------
    // Shortcuts (§8.5)
    // ------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        // no shortcuts while typing, in a dialog, or while dragging a segment edge
        if ctx.egui_wants_keyboard_input() || self.dialog_open() || self.drag.is_some() {
            return;
        }
        let ev = |k: Key, m: Modifiers| ctx.input_mut(|i| i.consume_key(m, k));
        let (none, shift, cmd) = (Modifiers::NONE, Modifiers::SHIFT, Modifiers::COMMAND);
        if ev(Key::S, cmd | shift) {
            self.save_as();
        }
        if ev(Key::S, cmd) {
            self.save();
        }
        if ev(Key::O, cmd) {
            self.request(crate::project::After::Open(None));
        }
        if ev(Key::N, cmd) {
            self.request(crate::project::After::New);
        }
        if ev(Key::Z, cmd | shift) || ev(Key::Y, cmd) {
            self.redo();
        }
        if ev(Key::Z, cmd) {
            self.undo();
        }
        if ev(Key::E, cmd) {
            self.open_export();
        }
        if ev(Key::ArrowLeft, cmd) {
            let t = self.source_time() - 5.0;
            self.seek(t);
        }
        if ev(Key::ArrowRight, cmd) {
            let t = self.source_time() + 5.0;
            self.seek(t);
        }
        if ev(Key::ArrowLeft, shift) {
            self.keyframe_jump(false);
        }
        if ev(Key::ArrowRight, shift) {
            self.keyframe_jump(true);
        }
        if ev(Key::ArrowLeft, none) {
            self.step(false);
        }
        if ev(Key::ArrowRight, none) {
            self.step(true);
        }
        if ev(Key::Space, none) {
            self.play_pause();
        }
        if ev(Key::Home, none) {
            self.seek(0.0);
        }
        if ev(Key::End, none)
            && let Some(info) = self.selected.and_then(|id| self.info(id))
        {
            let last = info.frames.frames.last().map_or(0.0, |f| f.t);
            self.seek(last);
        }
        // I / O, or [ / ] (on other layouts: the keys where a US keyboard has the brackets)
        if ev(Key::I, none) || ev(Key::OpenBracket, none) {
            self.set_start();
        }
        if ev(Key::O, none) || ev(Key::CloseBracket, none) {
            self.set_end();
        }
        if ev(Key::S, none) {
            self.split();
        }
        if ev(Key::Delete, none) {
            self.delete_selected_segment();
        }
        if ev(Key::P, none) {
            self.toggle_cut_preview();
        }
        let idx = self
            .selected
            .and_then(|s| self.project.clips.iter().position(|c| c.id == s));
        if ev(Key::PageUp, none)
            && let Some(i) = idx
            && i > 0
        {
            self.select_clip(Some(self.project.clips[i - 1].id));
        }
        if ev(Key::PageDown, none)
            && let Some(i) = idx
            && i + 1 < self.project.clips.len()
        {
            self.select_clip(Some(self.project.clips[i + 1].id));
        }
    }

    // ------------------------------------------------------------------
    // Layout
    // ------------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("+ Add clips…")
                .on_hover_text("or drop files onto the window")
                .clicked()
            {
                self.pick_and_add_files();
            }
            ui.menu_button("Project", |ui| {
                use crate::project::After;
                let item = |ui: &mut egui::Ui, text: &str, keys: &str| {
                    ui.add(egui::Button::new(text).shortcut_text(keys))
                        .clicked()
                };
                if item(ui, "New", "Ctrl+N") {
                    self.request(After::New);
                }
                if item(ui, "Open…", "Ctrl+O") {
                    self.request(After::Open(None));
                }
                if item(ui, "Save", "Ctrl+S") {
                    self.save();
                }
                if item(ui, "Save as…", "Ctrl+Shift+S") {
                    self.save_as();
                }
                if self.missing_count() > 0 {
                    ui.separator();
                    if ui.button("Missing files…").clicked() {
                        self.proj.relink_open = true;
                    }
                }
            })
            .response
            .on_hover_text("Save the clips and their segments to continue later");
            ui.separator();
            let all_on = matches!(self.loaded, Loaded::Preview { clip: None, .. });
            if ui
                .selectable_label(all_on, "Preview all")
                .on_hover_text("Play the kept segments of all enabled clips in order")
                .clicked()
            {
                self.toggle_preview_all();
            }
            ui.separator();
            if ui
                .add_enabled(self.history.can_undo(), egui::Button::new("Undo"))
                .on_hover_text("Ctrl + Z")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("Ctrl + Y")
                .clicked()
            {
                self.redo();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_export =
                    self.plan.as_ref().is_some_and(|p| p.is_ok()) && !self.export.is_running();
                if ui
                    .add_enabled(
                        can_export,
                        egui::Button::new(egui::RichText::new("Export…").strong()),
                    )
                    .on_hover_text("Ctrl + E")
                    .clicked()
                {
                    self.open_export();
                }
                if ui
                    .selectable_label(self.export.log_panel_open(), "Log")
                    .on_hover_text("The ffmpeg commands of this session's exports")
                    .clicked()
                {
                    self.export.toggle_log_panel();
                }
                if ui.button("Settings").clicked() {
                    self.open_settings();
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let n = self.project.clips.len();
            let kept: f64 = self
                .project
                .clips
                .iter()
                .filter(|c| c.enabled)
                .map(|c| edit::kept_duration(c.segments.as_deref().unwrap_or(&[])))
                .sum();
            ui.label(format!(
                "{n} clip{} · kept {}",
                if n == 1 { "" } else { "s" },
                timefmt::display(kept)
            ));
            if let Some(Ok(plan)) = &self.plan {
                if let Some(t) = plan.target_idx {
                    ui.separator();
                    ui.label(format!("target {}", plan.signatures[t].0.label()));
                }
                let re = plan
                    .clips
                    .iter()
                    .filter(|v| !v.copyable && !v.reasons.is_empty())
                    .count();
                if re > 0 {
                    ui.separator();
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("{re} clip{} re-encoded", if re == 1 { "" } else { "s" }),
                    );
                }
            }
            let probing = self.pending_count();
            if probing > 0 {
                ui.separator();
                ui.spinner();
                ui.label(format!(
                    "reading {probing} clip{}…",
                    if probing == 1 { "" } else { "s" }
                ));
            }
            if !self.status.is_empty() && self.status_at.elapsed() < Duration::from_secs(8) {
                ui.separator();
                ui.label(&self.status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!(
                    "video: {} · hwdec {}",
                    if self.render_kind == RenderKind::Gl {
                        "OpenGL"
                    } else {
                        "software"
                    },
                    if self.player.state.hwdec.is_empty() {
                        "–"
                    } else {
                        &self.player.state.hwdec
                    }
                ));
            });
        });
    }

    fn video(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let avail = ui.available_rect_before_wrap();
        ui.allocate_rect(avail, egui::Sense::hover());
        ui.painter().rect_filled(avail, 0.0, egui::Color32::BLACK);
        if let Some(err) = self.selected.and_then(|id| self.probe_error(id)) {
            ui.painter().text(
                avail.center(),
                egui::Align2::CENTER_CENTER,
                format!(
                    "This file can't be read:
{err}"
                ),
                egui::FontId::proportional(16.0),
                egui::Color32::from_rgb(230, 120, 100),
            );
            return;
        }
        if matches!(self.loaded, Loaded::Nothing) {
            ui.painter().text(
                avail.center(),
                egui::Align2::CENTER_CENTER,
                "Add clips (button above, or drop video files here)",
                egui::FontId::proportional(18.0),
                egui::Color32::GRAY,
            );
            return;
        }
        let ds = self.player.state.dsize;
        let aspect = if ds[0] > 0 && ds[1] > 0 {
            ds[0] as f32 / ds[1] as f32
        } else {
            16.0 / 9.0
        };
        let mut size = avail.size();
        if size.x / size.y.max(1.0) > aspect {
            size.x = size.y * aspect;
        } else {
            size.y = size.x / aspect;
        }
        let rect = egui::Rect::from_center_size(avail.center(), size);
        let ppp = ui.ctx().pixels_per_point();
        let px = [(size.x * ppp).round() as u32, (size.y * ppp).round() as u32];
        let tex = match self.renderer.as_mut() {
            Some(Renderer::Gl(r)) => Some(r.frame(frame, px)),
            Some(Renderer::Sw(r)) => r.frame(ui.ctx(), px),
            None => None,
        };
        if let Some(id) = tex {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(id, rect, uv, egui::Color32::WHITE);
        }
        if let Loaded::Preview { clip, .. } = &self.loaded {
            let label = if clip.is_some() {
                "CUT PREVIEW (P to leave)"
            } else {
                "PREVIEW ALL"
            };
            let painter = ui.painter();
            let g = painter.layout_no_wrap(
                label.into(),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
            let r = egui::Rect::from_min_size(avail.left_top() + egui::vec2(8.0, 8.0), g.size())
                .expand(4.0);
            painter.rect_filled(
                r,
                3.0,
                egui::Color32::from_rgba_unmultiplied(200, 120, 0, 200),
            );
            painter.galley(r.min + egui::vec2(4.0, 4.0), g, egui::Color32::WHITE);
        }
    }

    fn drop_files(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        // a dropped project file opens it (asking to save first); videos are added
        let (projects, clips): (Vec<PathBuf>, Vec<PathBuf>) =
            files.into_iter().partition(|f| is_project_file(f));
        if let Some(p) = projects.into_iter().next() {
            self.request(crate::project::After::Open(Some(p)));
        }
        if !clips.is_empty() {
            self.add_files(clips);
        }
    }

    /// Any dialog that takes the keyboard (shortcuts are off while it is open).
    fn dialog_open(&self) -> bool {
        self.export.is_modal_open()
            || self.title_dialog.is_some()
            || self.proj.prompt.is_some()
            || self.proj.recovery.is_some()
            || self.proj.relink_open
            || self.settings_open()
    }

    /// "Add clips…": the native file dialog, then add what was chosen.
    pub fn pick_and_add_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter(
                "Video",
                &crate::dialogs::filter_extensions(crate::dialogs::VIDEO_EXTENSIONS),
            )
            .pick_files()
        {
            self.add_files(files);
        }
    }

    /// Explains a file dialog that couldn't be shown (see `dialogs`).
    fn portal_notice_window(&mut self, ctx: &egui::Context) {
        if crate::dialogs::take_portal_failure() {
            tracing::warn!("no file dialog available (xdg-desktop-portal)");
            self.portal_notice = true;
        }
        if !self.portal_notice {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("portal-notice")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("No file dialog available");
            ui.add_space(6.0);
            ui.add(egui::Label::new(crate::dialogs::PORTAL_HELP).selectable(true));
            ui.add_space(8.0);
            ui.button("OK").clicked()
        });
        if modal.inner || modal.should_close() {
            self.portal_notice = false;
        }
    }
}

/// egui's fonts, with Hack (the bundled monospace font) as a fallback for proportional text:
/// Ubuntu-Light lacks arrows (← →) and other symbols, and egui 0.37 no longer bundles its emoji
/// fonts by default.
fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        let at = family
            .iter()
            .position(|f| f == "Ubuntu-Light")
            .map_or(0, |i| i + 1);
        family.insert(at, "Hack".to_owned());
    }
    fonts
}

/// Window size before the monitor is known, and for scripted runs.
pub const DEFAULT_WINDOW_SIZE: [f32; 2] = [1280.0, 800.0];
pub const MIN_WINDOW_SIZE: [f32; 2] = [900.0, 600.0];

/// First-run window size for a monitor of `monitor` points: 80% of it, at most 1600 × 1000.
/// (The monitor size includes the taskbar/panels and the title bar isn't part of the inner size,
/// so the full size would not fit.)
fn first_window_size(monitor: egui::Vec2) -> egui::Vec2 {
    (monitor * 0.8)
        .min(egui::vec2(1600.0, 1000.0))
        .max(egui::Vec2::from(MIN_WINDOW_SIZE))
}

impl App {
    /// First run: resize the window to suit the monitor and center it. eframe only knows the
    /// monitor once the window exists, and only limits the default size to the whole monitor.
    fn fit_window_to_screen(&mut self, ctx: &egui::Context) {
        if !self.fit_window {
            return;
        }
        let (monitor, outer, inner) = ctx.input(|i| {
            let v = i.viewport();
            (v.monitor_size, v.outer_rect, v.inner_rect)
        });
        let Some(monitor) = monitor.filter(|m| m.x > 1.0 && m.y > 1.0) else {
            // not known yet; give up after a few frames rather than resize late
            self.fit_window_tries += 1;
            self.fit_window = self.fit_window_tries < 10;
            return;
        };
        self.fit_window = false;
        let size = first_window_size(monitor);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        // centered on the primary monitor like eframe's `centered` (no positions on Wayland)
        if let (Some(outer), Some(inner)) = (outer, inner) {
            let frame = outer.size() - inner.size();
            let pos = ((monitor - size - frame) * 0.5).max(egui::Vec2::ZERO);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos.to_pos2()));
        }
        tracing::debug!("first run: window {size:?} for a {monitor:?} monitor");
    }
}

fn is_project_file(p: &std::path::Path) -> bool {
    p.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(keepcut_core::projfile::EXTENSION))
}

impl eframe::App for App {
    /// Only the window geometry is kept between runs (main.rs), not egui's memory
    /// (panel sizes, dialog positions): it could restore stale dialog state.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        // a running export must not outlive the window (its ffmpeg would keep writing)
        self.cancel_export_and_wait();
        // free the render context while the GL context is current, before the mpv handle
        self.renderer = None;
        self.project_on_exit();
        // scripted test runs must not change the user's settings
        if self.script.is_none() {
            self.config.remember_export(&self.project.export);
            self.config.save();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fit_window_to_screen(&ctx);
        while let Ok((path, r)) = self.probe.results.try_recv() {
            self.on_probed(path, r);
        }
        while let Ok(r) = self.assets.results.try_recv() {
            self.on_asset(&ctx, r);
        }
        if let Some(rx) = &self.encoders_rx
            && let Ok(set) = rx.try_recv()
        {
            self.encoders = set;
            self.encoders_rx = None;
            self.plan_dirty = true;
        }
        self.handle_player_events();
        self.project_tick(&ctx);
        self.drop_files(&ctx);
        self.shortcuts(&ctx);
        self.run_script(&ctx);
        self.refresh_plan();
        self.poll_export();

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            ui.add_space(3.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("clips")
            .resizable(true)
            .default_size(300.0)
            .min_size(200.0)
            .show(ui, |ui| {
                self.clip_list(ui);
            });
        egui::Panel::bottom("editor")
            .resizable(false)
            .show(ui, |ui| {
                self.editor(ui);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                self.video(ui, frame);
            });
        self.log_panel(&ctx);
        self.export_window(&ctx);
        self.title_window(&ctx);
        self.commit_title_edit(&ctx);
        self.project_dialogs(&ctx);
        self.settings_window(&ctx);
        self.portal_notice_window(&ctx);
        self.flush_seek();

        if let Some(Renderer::Gl(r)) = &self.renderer {
            r.report_swap();
        }
        let busy =
            self.export.is_running() || self.encoders_rx.is_some() || self.pending_count() > 0;
        if busy {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;

    #[test]
    fn first_window_fits_the_monitor() {
        let size = |w, h| first_window_size(egui::vec2(w, h));
        let near = |a: egui::Vec2, b: egui::Vec2| (a - b).length() < 0.01;
        // 1920×1080 at 125 %: smaller than the old 1500×950 default
        assert!(near(size(1536.0, 864.0), egui::vec2(1228.8, 691.2)));
        // large monitors: capped
        assert!(near(size(2560.0, 1440.0), egui::vec2(1600.0, 1000.0)));
        // tiny monitors: never below the minimum window size
        assert!(near(size(1024.0, 600.0), egui::vec2(900.0, 600.0)));
    }
}
