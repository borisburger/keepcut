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

/// Probe state of one clip.
#[derive(Default)]
pub struct ClipMedia {
    pub info: Option<Arc<MediaInfo>>,
    pub error: Option<String>,
    /// Playhead to restore when the clip is selected again.
    pub last_pos: f64,
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
    pub media: HashMap<Uuid, ClipMedia>,
    pub probe: ProbeService,
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
    /// A fast frame step (speed 32) is in flight.
    fast_step: Option<Instant>,
    pending_seek: Option<f64>,
    /// Test automation (`--script`).
    pub script: Option<crate::script::Script>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> anyhow::Result<App> {
        let config = Config::load();
        let tools = Tools::locate().ok();
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
        let encoders_rx = Some(media::detect_encoders(tools.clone(), cc.egui_ctx.clone()));
        let mut project = Project {
            version: keepcut_core::model::PROJECT_VERSION,
            id: Uuid::new_v4(),
            clips: Vec::new(),
            export: Default::default(),
        };
        project.export.encoding = config.encoding;
        project.export.output_mode = config.output_mode;
        let mut app = App {
            renderer: Some(renderer),
            player,
            render_kind: kind,
            tools,
            config,
            project,
            media: HashMap::new(),
            probe,
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
            fast_step: None,
            pending_seek: None,
            script: None,
        };
        if app.tools.is_none() {
            app.set_status("ffmpeg/ffprobe not found: install ffmpeg or put it next to Keepcut");
        }
        app.add_files(files);
        Ok(app)
    }

    pub fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_at = Instant::now();
    }

    // ------------------------------------------------------------------
    // Clips and edits
    // ------------------------------------------------------------------

    pub fn add_files(&mut self, files: Vec<PathBuf>) {
        let mut added = 0;
        for f in files {
            let f = std::path::absolute(&f).unwrap_or(f);
            if !f.is_file() || self.project.clips.iter().any(|c| c.path == f) {
                continue;
            }
            let clip = Clip {
                id: Uuid::new_v4(),
                path: f.clone(),
                file_size: 0,
                file_mtime: 0,
                enabled: true,
                segments: Some(Vec::new()), // filled with the whole clip once probed
                chapter_title: None,
            };
            let before = self.project.clips.clone();
            self.media.insert(clip.id, ClipMedia::default());
            self.project.clips.push(clip);
            self.history.push(before);
            self.probe.enqueue(f);
            added += 1;
        }
        if added > 0 {
            self.plan_dirty = true;
            if self.selected.is_none() {
                self.select_clip(self.project.clips.first().map(|c| c.id));
            }
        }
    }

    fn on_probed(&mut self, path: PathBuf, r: Result<Arc<MediaInfo>, String>) {
        let ids: Vec<Uuid> = self
            .project
            .clips
            .iter()
            .filter(|c| c.path == path)
            .map(|c| c.id)
            .collect();
        for id in ids {
            let m = self.media.entry(id).or_default();
            match &r {
                Ok(info) => {
                    m.info = Some(info.clone());
                    m.error = None;
                    if let Some(c) = self.project.clips.iter_mut().find(|c| c.id == id) {
                        c.file_size = info.file_size;
                        c.file_mtime = info.file_mtime;
                        // a new clip gets one segment covering the whole clip
                        if c.segments.as_ref().is_none_or(Vec::is_empty) {
                            c.segments = Some(vec![Segment::new(0.0, info.video_end())]);
                        }
                    }
                }
                Err(e) => m.error = Some(e.clone()),
            }
        }
        self.sort_new_clip(&path);
        self.plan_dirty = true;
        if self
            .selected
            .is_some_and(|s| self.clip(s).is_some_and(|c| c.path == path))
        {
            self.reload_selected();
        }
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
        self.media
            .get(&id)?
            .info
            .as_ref()
            .map(|i| i.recording_time())
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

    pub fn clip(&self, id: Uuid) -> Option<&Clip> {
        self.project.clips.iter().find(|c| c.id == id)
    }

    pub fn info(&self, id: Uuid) -> Option<Arc<MediaInfo>> {
        self.media.get(&id)?.info.clone()
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
        // an edited clip invalidates a running cut preview of it
        if matches!(self.loaded, Loaded::Preview { .. }) {
            self.reload_selected();
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
        if self.selected.is_some_and(|s| self.clip(s).is_none()) {
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

    /// Current position in source time of the selected clip.
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

    pub fn select_clip(&mut self, id: Option<Uuid>) {
        if let Some(cur) = self.selected
            && let Some(m) = self.media.get_mut(&cur)
            && matches!(self.loaded, Loaded::Clip(c) if c == cur)
        {
            m.last_pos = self.player.state.time_pos;
        }
        if self.selected != id {
            self.selected_seg = None;
            self.timeline = TimelineView::default();
        }
        self.selected = id;
        self.reload_selected();
    }

    /// Loads the selected clip into mpv (keeping its last playhead).
    pub fn reload_selected(&mut self) {
        let Some(id) = self.selected else {
            self.player.command("stop", &[]);
            self.loaded = Loaded::Nothing;
            return;
        };
        let Some(c) = self.clip(id) else { return };
        let path = c.path.to_string_lossy().into_owned();
        let pos = self.media.get(&id).map_or(0.0, |m| m.last_pos);
        self.player.command(
            "loadfile",
            &[&path, "replace", "-1", &format!("start={pos:.6}")],
        );
        self.player.command("set", &["pause", "yes"]);
        self.loaded = Loaded::Clip(id);
    }

    fn load_preview(&mut self, clip: Option<Uuid>) {
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
        self.loaded = Loaded::Preview { clip, spans };
    }

    pub fn toggle_cut_preview(&mut self) {
        match &self.loaded {
            Loaded::Preview { .. } => self.reload_selected(),
            _ => self.load_preview(self.selected),
        }
    }

    pub fn toggle_preview_all(&mut self) {
        match &self.loaded {
            Loaded::Preview { clip: None, .. } => self.reload_selected(),
            _ => self.load_preview(None),
        }
    }

    /// Seeks to a source time of the selected clip (leaving any preview).
    pub fn seek(&mut self, t: f64) {
        if matches!(self.loaded, Loaded::Preview { .. }) {
            if let Some(id) = self.selected
                && let Some(m) = self.media.get_mut(&id)
            {
                m.last_pos = t;
            }
            self.reload_selected();
            return;
        }
        self.pending_seek = Some(t.max(0.0));
    }

    fn flush_seek(&mut self) {
        // coalesce: one seek per UI frame while dragging
        if let Some(t) = self.pending_seek.take() {
            self.player.seek_exact(t);
        }
    }

    /// Frame step (§3.5): forward plays the step at speed 32; backward seeks
    /// exactly to the previous frame from the frame index.
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
        let t = self.player.state.time_pos;
        if forward {
            self.player.command("set", &["speed", "32"]);
            self.player.command("frame-step", &[]);
            self.fast_step = Some(Instant::now());
        } else if let Some(p) = self
            .selected
            .and_then(|id| self.info(id))
            .and_then(|i| i.frames.prev_frame_time(t - 1e-4))
        {
            self.seek(p);
        } else {
            self.player.command("frame-back-step", &[]);
        }
    }

    pub fn keyframe_jump(&mut self, forward: bool) {
        let Some(info) = self.selected.and_then(|id| self.info(id)) else {
            return;
        };
        let t = self.source_time();
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
        if self.fast_step.take().is_some() {
            self.player.command("set", &["speed", "1"]);
        }
        self.player.command("cycle", &["pause"]);
    }

    /// Playhead snapped to the nearest frame of the selected clip.
    pub fn snapped_playhead(&self) -> Option<f64> {
        let info = self.info(self.selected?)?;
        Some(info.frames.snap(self.source_time()))
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
                        self.player.command("set", &["speed", "1"]);
                        self.fast_step = None;
                    }
                }
                PlayerEvent::EndFile => {}
                _ => {}
            }
        }
        if self
            .fast_step
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.player.command("set", &["speed", "1"]);
            self.fast_step = None;
        }
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
        if ctx.egui_wants_keyboard_input() || self.export.is_modal_open() {
            return;
        }
        let ev = |k: Key, m: Modifiers| ctx.input_mut(|i| i.consume_key(m, k));
        let (none, shift, cmd) = (Modifiers::NONE, Modifiers::SHIFT, Modifiers::COMMAND);
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
        if ev(Key::I, none) {
            self.set_start();
        }
        if ev(Key::O, none) {
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
                && let Some(files) = rfd::FileDialog::new()
                    .add_filter("Video", &["mp4", "mkv", "mov", "m4v", "ts", "avi", "webm"])
                    .pick_files()
            {
                self.add_files(files);
            }
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
                .on_hover_text("Ctrl+Z")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.history.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("Ctrl+Y")
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
                    .on_hover_text("Ctrl+E")
                    .clicked()
                {
                    self.open_export();
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
            let probing = self
                .media
                .values()
                .filter(|m| m.info.is_none() && m.error.is_none())
                .count();
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
        if let Some(err) = self
            .selected
            .and_then(|id| self.media.get(&id))
            .and_then(|m| m.error.clone())
        {
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
        if !files.is_empty() {
            self.add_files(files);
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        // free the render context while the GL context is current, before the mpv handle
        self.renderer = None;
        // scripted test runs must not change the user's settings
        if self.script.is_none() {
            self.config.encoding = self.project.export.encoding;
            self.config.output_mode = self.project.export.output_mode;
            self.config.save();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok((path, r)) = self.probe.results.try_recv() {
            self.on_probed(path, r);
        }
        if let Some(rx) = &self.encoders_rx
            && let Ok(set) = rx.try_recv()
        {
            self.encoders = set;
            self.encoders_rx = None;
            self.plan_dirty = true;
        }
        self.handle_player_events();
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
        self.export_window(&ctx);
        self.flush_seek();

        if let Some(Renderer::Gl(r)) = &self.renderer {
            r.report_swap();
        }
        let busy = self.export.is_running()
            || self.encoders_rx.is_some()
            || self
                .media
                .values()
                .any(|m| m.info.is_none() && m.error.is_none());
        if busy {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
