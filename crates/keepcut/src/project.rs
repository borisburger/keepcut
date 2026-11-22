//! Projects (§9): save/open `.keepcut` files, the unsaved-changes prompt,
//! autosave + crash recovery, and relinking missing source files (§8.7).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, RichText};
use keepcut_core::history::History;
use keepcut_core::model::{Clip, PROJECT_VERSION, Project, Soundtrack};
use keepcut_core::{edit, projfile};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::{App, ProbeState};

/// What to do once the unsaved-changes prompt is answered.
#[derive(Debug, Clone)]
pub enum After {
    New,
    /// Open a project (`None`: ask for the file).
    Open(Option<PathBuf>),
    Close,
}

#[derive(Serialize, Deserialize)]
struct Autosave {
    /// The project file this is unsaved work of (`None`: untitled).
    saved_path: Option<PathBuf>,
    project: Project,
}

pub struct Recovery {
    file: PathBuf,
    autosave: Autosave,
    when: SystemTime,
}

pub struct ProjectState {
    /// The project file (`None`: untitled).
    pub path: Option<PathBuf>,
    /// The clip list as last saved or opened (JSON), to tell unsaved changes.
    saved: String,
    /// The clip list when the last export started: closing an untitled project
    /// that was exported since its last change doesn't ask to save it.
    exported: Option<String>,
    pub dirty: bool,
    // autosave (§9): written 2 s after the last change
    seen: String,
    changed_at: Option<Instant>,
    checked_at: Instant,
    title: String,
    pub prompt: Option<After>,
    allow_close: bool,
    pub recovery: Option<Recovery>,
    startup_done: bool,
    pub relink_open: bool,
    /// Sources whose size/date differ from the project file (segments refitted).
    pub changed_files: HashSet<PathBuf>,
}

impl Default for ProjectState {
    fn default() -> Self {
        ProjectState {
            path: None,
            saved: content_json(&[], None),
            exported: None,
            dirty: false,
            seen: content_json(&[], None),
            changed_at: None,
            checked_at: Instant::now(),
            title: String::new(),
            prompt: None,
            allow_close: false,
            recovery: None,
            startup_done: false,
            relink_open: false,
            changed_files: HashSet::new(),
        }
    }
}

/// What counts as a change to the project (unsaved changes, autosave): the clips and the
/// soundtrack.
fn content_json(clips: &[Clip], soundtrack: Option<&Soundtrack>) -> String {
    serde_json::to_string(&(clips, soundtrack)).unwrap_or_default()
}

fn autosave_dir() -> Option<PathBuf> {
    crate::config::dirs().map(|d| d.data_dir().join("autosave"))
}

fn autosave_file(id: Uuid) -> Option<PathBuf> {
    autosave_dir().map(|d| d.join(format!("{id}.{}", projfile::EXTENSION)))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl App {
    /// Scripted runs never prompt, autosave or offer recovery: they must not
    /// touch the user's data.
    fn scripted(&self) -> bool {
        self.script.is_some()
    }

    pub fn project_name(&self) -> String {
        self.proj
            .path
            .as_deref()
            .map_or("Untitled".into(), file_name)
    }

    /// Unsaved changes that closing would lose.
    fn needs_save_prompt(&self) -> bool {
        if !self.proj.dirty || self.project.clips.is_empty() {
            return false;
        }
        let now = content_json(&self.project.clips, self.project.soundtrack.as_ref());
        !(self.proj.path.is_none() && self.proj.exported.as_deref() == Some(now.as_str()))
    }

    pub fn note_export_started(&mut self) {
        self.proj.exported = Some(content_json(
            &self.project.clips,
            self.project.soundtrack.as_ref(),
        ));
    }

    /// Runs `after` now, or first asks whether to save unsaved changes.
    pub fn request(&mut self, after: After) {
        if self.needs_save_prompt() && !self.scripted() {
            self.proj.prompt = Some(after);
        } else {
            self.run_after(after);
        }
    }

    fn run_after(&mut self, after: After) {
        match after {
            After::New => self.new_project(),
            After::Open(Some(p)) => self.open_project(&p),
            After::Open(None) => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter(
                        "Keepcut project",
                        &crate::dialogs::filter_extensions(&[projfile::EXTENSION]),
                    )
                    .pick_file()
                {
                    self.open_project(&p);
                }
            }
            After::Close => {
                self.proj.allow_close = true;
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    pub fn new_project(&mut self) {
        let mut p = Project {
            version: PROJECT_VERSION,
            id: Uuid::new_v4(),
            clips: Vec::new(),
            export: Default::default(),
            soundtrack: None,
        };
        self.config.apply_export_defaults(&mut p.export);
        self.replace_project(p, None);
    }

    /// Saves to the project file, or asks for one. Returns false if cancelled or failed.
    pub fn save(&mut self) -> bool {
        match self.proj.path.clone() {
            Some(p) => self.save_to(&p),
            None => self.save_as(),
        }
    }

    pub fn save_as(&mut self) -> bool {
        let default = self
            .project
            .clips
            .first()
            .and_then(|c| c.path.file_stem())
            .map(|s| format!("{}.{}", s.to_string_lossy(), projfile::EXTENSION))
            .unwrap_or_else(|| format!("Untitled.{}", projfile::EXTENSION));
        let mut dlg = rfd::FileDialog::new()
            .add_filter(
                "Keepcut project",
                &crate::dialogs::filter_extensions(&[projfile::EXTENSION]),
            )
            .set_file_name(default);
        if let Some(dir) = self
            .proj
            .path
            .as_deref()
            .or(self.project.clips.first().map(|c| c.path.as_path()))
            .and_then(Path::parent)
        {
            dlg = dlg.set_directory(dir);
        }
        match dlg.save_file() {
            Some(mut p) => {
                if p.extension().is_none() {
                    p.set_extension(projfile::EXTENSION);
                }
                self.save_to(&p)
            }
            None => false,
        }
    }

    pub fn save_to(&mut self, path: &Path) -> bool {
        match projfile::save(&self.project, path) {
            Ok(()) => {
                self.proj.path = Some(path.to_path_buf());
                self.proj.saved =
                    content_json(&self.project.clips, self.project.soundtrack.as_ref());
                self.proj.dirty = false;
                // the autosave is now older than the saved file
                self.delete_autosave();
                self.set_status(format!("Saved {}", file_name(path)));
                tracing::info!("saved project {}", path.display());
                true
            }
            Err(e) => {
                self.set_status(format!("Cannot save {}: {e}", path.display()));
                tracing::warn!("cannot save {}: {e}", path.display());
                false
            }
        }
    }

    pub fn open_project(&mut self, path: &Path) {
        match projfile::load(path) {
            Ok(p) => {
                tracing::info!("opened project {}", path.display());
                self.replace_project(p, Some(path.to_path_buf()));
            }
            Err(e) => self.set_status(format!("Cannot open {}: {e}", path.display())),
        }
    }

    /// Makes `p` the current project: probes its files (missing ones offer
    /// relinking), clears undo, selects the first clip.
    pub fn replace_project(&mut self, p: Project, path: Option<PathBuf>) {
        self.delete_autosave(); // of the project being replaced
        self.close_export_windows();
        self.project = p;
        self.proj.path = path;
        self.proj.saved = content_json(&self.project.clips, self.project.soundtrack.as_ref());
        self.proj.seen = self.proj.saved.clone();
        self.proj.dirty = false;
        self.proj.exported = None;
        self.proj.changed_at = None;
        self.proj.changed_files.clear();
        self.history = History::new(200);
        self.last_pos.clear();
        self.selected = None;
        self.selected_seg = None;
        let paths: Vec<PathBuf> = self.project.clips.iter().map(|c| c.path.clone()).collect();
        let mut missing = false;
        for f in paths {
            if !f.is_file() {
                self.probes.insert(f, ProbeState::Missing);
                missing = true;
            } else if !matches!(
                self.probes.get(&f),
                Some(ProbeState::Ready(_) | ProbeState::Pending)
            ) {
                self.probes.insert(f.clone(), ProbeState::Pending);
                self.probe.enqueue(f);
            }
        }
        // already-probed files: check for changes right away
        for f in self
            .project
            .clips
            .iter()
            .map(|c| c.path.clone())
            .collect::<Vec<_>>()
        {
            if let Some(ProbeState::Ready(info)) = self.probes.get(&f).cloned() {
                self.check_changed_source(&f, &info);
            }
        }
        self.soundtrack_edit = None;
        self.probe_soundtrack();
        self.fill_whole_clip_segments();
        self.proj.relink_open = missing;
        self.select_clip(self.project.clips.first().map(|c| c.id));
        self.mark_plan_dirty();
    }

    /// A source whose size or date differs from what the project stored: fit
    /// its segments to the (possibly new) length and flag it (§9). Must run
    /// before `fill_whole_clip_segments` overwrites the stored size/date.
    pub fn check_changed_source(&mut self, path: &Path, info: &keepcut_core::probe::MediaInfo) {
        let end = info.video_end();
        let mut refitted = false;
        let mut changed = false;
        for c in self.project.clips.iter_mut().filter(|c| c.path == path) {
            if c.file_size == 0
                || (c.file_size == info.file_size && c.file_mtime == info.file_mtime)
            {
                continue;
            }
            changed = true;
            if let Some(segs) = c.segments.as_mut() {
                let before = segs.clone();
                edit::normalize(segs, end);
                refitted |= *segs != before;
            }
        }
        if changed {
            self.proj.changed_files.insert(path.to_path_buf());
            let msg = if refitted {
                format!(
                    "{} changed since the project was saved; its segments were fitted to the new length",
                    file_name(path)
                )
            } else {
                format!("{} changed since the project was saved", file_name(path))
            };
            tracing::warn!("{msg}");
            self.set_status(msg);
            self.mark_plan_dirty();
        }
    }

    /// Points a clip at a new file; other missing clips whose file name exists
    /// in the same new folder are relinked too. One undo step.
    pub fn relink(&mut self, id: Uuid, new_path: PathBuf) {
        let dir = new_path.parent().map(Path::to_path_buf);
        let missing: Vec<(Uuid, PathBuf)> = self
            .project
            .clips
            .iter()
            .filter(|c| matches!(self.probes.get(&c.path), Some(ProbeState::Missing)))
            .map(|c| (c.id, c.path.clone()))
            .collect();
        let mut moves: Vec<(Uuid, PathBuf)> = vec![(id, new_path)];
        if let Some(dir) = dir {
            for (cid, old) in &missing {
                if *cid == id {
                    continue;
                }
                if let Some(name) = old.file_name()
                    && dir.join(name).is_file()
                {
                    moves.push((*cid, dir.join(name)));
                }
            }
        }
        let n = moves.len();
        self.edit(|clips| {
            for (cid, p) in &moves {
                if let Some(c) = clips.iter_mut().find(|c| c.id == *cid) {
                    c.path = p.clone();
                }
            }
            true
        });
        for (_, p) in moves {
            if !matches!(
                self.probes.get(&p),
                Some(ProbeState::Ready(_) | ProbeState::Pending)
            ) {
                self.probes.insert(p.clone(), ProbeState::Pending);
                self.probe.enqueue(p);
            }
        }
        self.set_status(if n == 1 {
            "Relinked 1 clip".to_string()
        } else {
            format!("Relinked {n} clips (the others were found in the same folder)")
        });
        if id == self.selected.unwrap_or_default() {
            self.reload_selected();
        }
    }

    pub fn locate(&mut self, id: Uuid) {
        let Some(c) = self.clip(id) else { return };
        let mut dlg = rfd::FileDialog::new()
            .set_title(format!("Locate {}", file_name(&c.path)))
            .add_filter(
                "Video",
                &crate::dialogs::filter_extensions(crate::dialogs::VIDEO_EXTENSIONS),
            );
        if let Some(d) = c.path.parent().filter(|d| d.is_dir()).or(self
            .proj
            .path
            .as_deref()
            .and_then(Path::parent))
        {
            dlg = dlg.set_directory(d);
        }
        if let Some(p) = dlg.pick_file() {
            self.relink(id, p);
        }
    }

    pub fn missing_count(&self) -> usize {
        self.project
            .clips
            .iter()
            .filter(|c| matches!(self.probes.get(&c.path), Some(ProbeState::Missing)))
            .count()
    }

    // ------------------------------------------------------------------
    // Autosave and recovery
    // ------------------------------------------------------------------

    fn delete_autosave(&self) {
        if let Some(f) = autosave_file(self.project.id) {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Once per second: track unsaved changes and write the autosave 2 s after
    /// the last change (§9).
    pub fn project_tick(&mut self, ctx: &egui::Context) {
        if !self.proj.startup_done {
            self.proj.startup_done = true;
            if !self.scripted() && self.project.clips.is_empty() {
                self.find_recovery();
            }
        }
        if self.proj.checked_at.elapsed() >= Duration::from_secs(1) && self.drag.is_none() {
            self.proj.checked_at = Instant::now();
            let now = content_json(&self.project.clips, self.project.soundtrack.as_ref());
            self.proj.dirty = now != self.proj.saved;
            if now != self.proj.seen {
                self.proj.seen = now;
                self.proj.changed_at = Some(Instant::now());
            }
        }
        if let Some(t) = self.proj.changed_at
            && t.elapsed() >= Duration::from_secs(2)
        {
            self.proj.changed_at = None;
            if !self.scripted() {
                self.write_autosave();
            }
        }
        if self.proj.changed_at.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let title = format!(
            "{}{} - Keepcut",
            self.project_name(),
            if self.proj.dirty { " *" } else { "" }
        );
        if title != self.proj.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.proj.title = title;
        }
        // closing the window with unsaved changes: ask first
        if ctx.input(|i| i.viewport().close_requested())
            && !self.proj.allow_close
            && !self.scripted()
            && self.needs_save_prompt()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.proj.prompt = Some(After::Close);
        }
    }

    fn write_autosave(&self) {
        let Some(file) = autosave_file(self.project.id) else {
            return;
        };
        if !self.proj.dirty {
            let _ = std::fs::remove_file(&file);
            return;
        }
        let a = Autosave {
            saved_path: self.proj.path.clone(),
            project: self.project.clone(),
        };
        let r = std::fs::create_dir_all(file.parent().unwrap_or(&file))
            .map_err(keepcut_core::Error::from)
            .and_then(|_| {
                projfile::write_atomic(&file, &serde_json::to_string(&a).unwrap_or_default())
            });
        if let Err(e) = r {
            tracing::warn!("cannot write autosave {}: {e}", file.display());
        }
    }

    /// On clean exit the autosave goes (saved, or the user chose not to save).
    pub fn project_on_exit(&self) {
        if !self.scripted() {
            self.delete_autosave();
        }
    }

    /// An autosave left by a crash: offer it when it is newer than its saved
    /// project file (or the project was never saved). Stale ones are removed.
    fn find_recovery(&mut self) {
        let Some(dir) = autosave_dir() else { return };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut best: Option<Recovery> = None;
        for e in rd.flatten() {
            let file = e.path();
            if file.extension().is_none_or(|x| x != projfile::EXTENSION) {
                continue;
            }
            let when = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let parsed = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| serde_json::from_str::<Autosave>(&t).ok());
            let Some(a) = parsed else {
                let _ = std::fs::remove_file(&file);
                continue;
            };
            let saved_newer = a.saved_path.as_ref().is_some_and(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .is_ok_and(|m| m >= when)
            });
            if saved_newer || a.project.clips.is_empty() {
                let _ = std::fs::remove_file(&file);
                continue;
            }
            match &best {
                Some(b) if b.when >= when => {}
                _ => {
                    best = Some(Recovery {
                        file,
                        autosave: a,
                        when,
                    })
                }
            }
        }
        self.proj.recovery = best;
    }

    // ------------------------------------------------------------------
    // Dialogs
    // ------------------------------------------------------------------

    pub fn project_dialogs(&mut self, ctx: &egui::Context) {
        self.save_prompt(ctx);
        self.recovery_dialog(ctx);
        self.relink_dialog(ctx);
    }

    fn save_prompt(&mut self, ctx: &egui::Context) {
        let Some(after) = self.proj.prompt.clone() else {
            return;
        };
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::unique("save-prompt")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(format!("Save changes to {}?", self.project_name()));
            ui.label("Your edits will be lost if you don't save them.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Save").strong()).clicked() {
                    choice = Some(true);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(false);
                }
                if ui.button("Cancel").clicked() {
                    self.proj.prompt = None;
                }
            });
        });
        if modal.should_close() {
            self.proj.prompt = None;
        }
        if let Some(save) = choice {
            self.proj.prompt = None;
            if !save || self.save() {
                if !save {
                    // "Don't save": the work is discarded, so is its autosave
                    self.delete_autosave();
                    self.proj.dirty = false;
                    self.proj.saved =
                        content_json(&self.project.clips, self.project.soundtrack.as_ref());
                }
                self.run_after(after);
            }
        }
    }

    fn recovery_dialog(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.proj.recovery else { return };
        let mut restore = None;
        let n = r.autosave.project.clips.len();
        let name = r
            .autosave
            .saved_path
            .as_deref()
            .map_or("an untitled project".into(), file_name);
        let ago = r.when.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        let ago = if ago < 120 {
            format!("{ago} s ago")
        } else if ago < 7200 {
            format!("{} min ago", ago / 60)
        } else if ago < 172_800 {
            format!("{} h ago", ago / 3600)
        } else {
            format!("{} days ago", ago / 86400)
        };
        egui::Modal::new(egui::Id::unique("recovery")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Restore unsaved work?");
            ui.label(format!(
                "Keepcut didn't close normally last time. There are unsaved changes to {name} \
                 ({n} clip{}, autosaved {ago}).",
                if n == 1 { "" } else { "s" }
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Restore").strong()).clicked() {
                    restore = Some(true);
                }
                if ui.button("Discard").clicked() {
                    restore = Some(false);
                }
            });
        });
        if let Some(yes) = restore {
            let r = self.proj.recovery.take().unwrap();
            if yes {
                let saved = r.autosave.saved_path.clone();
                self.replace_project(r.autosave.project, saved.clone());
                // still unsaved: compare against the project file as it is on disk
                self.proj.saved = saved.and_then(|p| projfile::load(&p).ok()).map_or_else(
                    || content_json(&[], None),
                    |p| content_json(&p.clips, p.soundtrack.as_ref()),
                );
                self.proj.dirty = true;
                self.set_status("Restored the unsaved work");
            } else {
                let _ = std::fs::remove_file(&r.file);
            }
        }
    }

    fn relink_dialog(&mut self, ctx: &egui::Context) {
        if !self.proj.relink_open {
            return;
        }
        let missing: Vec<(Uuid, PathBuf)> = self
            .project
            .clips
            .iter()
            .filter(|c| matches!(self.probes.get(&c.path), Some(ProbeState::Missing)))
            .map(|c| (c.id, c.path.clone()))
            .collect();
        if missing.is_empty() {
            self.proj.relink_open = false;
            return;
        }
        let mut locate = None;
        let mut remove = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("relink")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Missing source files");
            ui.label(
                "These files were moved, renamed or deleted. Locate them, or remove the clips. \
                      Locating one also finds the others if they are in the same folder.",
            );
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    // buttons first: the (long) folder path gets the rest and is truncated
                    for (id, p) in &missing {
                        ui.horizontal(|ui| {
                            if ui.button("Locate…").clicked() {
                                locate = Some(*id);
                            }
                            if ui.button("Remove").clicked() {
                                remove = Some(*id);
                            }
                            ui.vertical(|ui| {
                                ui.strong(file_name(p));
                                let dir = p
                                    .parent()
                                    .map(|d| d.display().to_string())
                                    .unwrap_or_default();
                                ui.add(egui::Label::new(RichText::new(&dir).weak()).truncate())
                                    .on_hover_text(dir);
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
            ui.add_space(6.0);
            if ui
                .button("Close")
                .on_hover_text("Missing clips stay in the list and are left out of exports")
                .clicked()
            {
                close = true;
            }
        });
        // only Close or Esc: a stray click next to this dialog must not dismiss it
        if close || (modal.should_close() && !modal.backdrop_response.clicked()) {
            self.proj.relink_open = false;
        }
        if let Some(id) = locate {
            self.locate(id);
        }
        if let Some(id) = remove {
            self.remove_clip(id);
        }
    }
}
