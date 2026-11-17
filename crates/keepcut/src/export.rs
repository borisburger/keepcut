//! Export dialog, background export job, progress and finish windows (§8.6).

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use keepcut_core::exec::{self, ExecOptions, OutputResult};
use keepcut_core::ffmpeg::{CancelToken, LogLine, LogSink};
use keepcut_core::model::{EncodingMode, OutputMode};
use keepcut_core::plan;
use keepcut_core::probe::MediaInfo;
use keepcut_core::timefmt;
use keepcut_core::verify::DecodeCheck;

use crate::app::App;
use crate::config::OutputLocation;

#[derive(Default)]
pub struct ExportUi {
    dialog: bool,
    /// Joined-mode file name (without folder).
    name: String,
    job: Option<Job>,
    finished: Option<Finished>,
}

struct Job {
    cancel: CancelToken,
    progress: Arc<Mutex<exec::Progress>>,
    log: Arc<Mutex<Vec<String>>>,
    rx: Receiver<Result<Vec<OutputResult>, String>>,
    started: Instant,
    show_log: bool,
}

struct Finished {
    result: Result<Vec<OutputResult>, String>,
    log: Vec<String>,
    elapsed: Duration,
    show_log: bool,
}

impl ExportUi {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }
    pub fn is_modal_open(&self) -> bool {
        self.dialog || self.job.is_some() || self.finished.is_some()
    }
}

impl App {
    pub fn open_export(&mut self) {
        if self.export.is_running() {
            return;
        }
        self.export.finished = None;
        self.apply_output_settings();
        // default joined name = the planner's default for the first clip
        self.export.name = match self.build_plan() {
            Some(Ok(p)) if self.project.export.output_name.is_none() => p
                .outputs
                .first()
                .and_then(|o| o.path.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => self.project.export.output_name.clone().unwrap_or_default(),
        };
        self.export.dialog = true;
    }

    /// Copies the output location from the config into the project settings.
    fn apply_output_settings(&mut self) {
        self.project.export.output_dir = match self.config.output_location {
            OutputLocation::SameAsSource => None,
            OutputLocation::Custom => self.config.custom_output_dir.clone(),
        };
        self.mark_plan_dirty();
    }

    pub fn poll_export(&mut self) {
        let Some(job) = &self.export.job else { return };
        if let Ok(result) = job.rx.try_recv() {
            let log = job.log.lock().unwrap().clone();
            self.export.finished = Some(Finished {
                result,
                log,
                elapsed: job.started.elapsed(),
                show_log: false,
            });
            self.export.job = None;
        }
    }

    pub fn export_window(&mut self, ctx: &egui::Context) {
        if self.export.dialog {
            self.export_dialog(ctx);
        } else if self.export.job.is_some() {
            self.progress_window(ctx);
        } else if self.export.finished.is_some() {
            self.finished_window(ctx);
        }
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        let mut start = false;
        let mut close = false;
        let mut changed = false;
        let modal = egui::Modal::new(egui::Id::unique("export-dialog")).show(ctx, |ui| {
            ui.set_width(620.0);
            ui.heading("Export");
            ui.add_space(6.0);
            egui::Grid::new("export-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Output");
                ui.vertical(|ui| {
                    let m = &mut self.project.export.output_mode;
                    changed |= ui.radio_value(m, OutputMode::Joined, "Single joined file").changed();
                    changed |= ui.radio_value(m, OutputMode::PerClip, "One file per clip").changed();
                    changed |= ui.radio_value(m, OutputMode::PerSegment, "One file per segment").changed();
                });
                ui.end_row();

                ui.label("Folder");
                ui.vertical(|ui| {
                    let mut loc = self.config.output_location;
                    changed |= ui.radio_value(&mut loc, OutputLocation::SameAsSource, "Same folder as the source").changed();
                    ui.horizontal(|ui| {
                        changed |= ui.radio_value(&mut loc, OutputLocation::Custom, "Custom:").changed();
                        let shown = self.config.custom_output_dir.as_ref().map_or("(none)".into(), |p| p.display().to_string());
                        ui.label(shown);
                        if ui.button("Browse…").clicked()
                            && let Some(d) = rfd::FileDialog::new().pick_folder()
                        {
                            self.config.custom_output_dir = Some(d);
                            loc = OutputLocation::Custom;
                            changed = true;
                        }
                    });
                    self.config.output_location = loc;
                });
                ui.end_row();

                if self.project.export.output_mode == OutputMode::Joined {
                    ui.label("File name");
                    changed |= ui.add(egui::TextEdit::singleline(&mut self.export.name).desired_width(400.0)).changed();
                    ui.end_row();
                }

                ui.label("Encoding");
                ui.vertical(|ui| {
                    let e = &mut self.project.export.encoding;
                    changed |= ui
                        .radio_value(e, EncodingMode::Smart, "Smart: lossless where possible")
                        .on_hover_text("Copies the original video; re-encodes only from a cut to the next keyframe, and clips that don't match")
                        .changed();
                    changed |= ui
                        .radio_value(e, EncodingMode::Full, "Full re-encode (H.264, match source quality)")
                        .on_hover_text("Re-encodes everything with one set of settings; the safe fallback")
                        .changed();
                });
                ui.end_row();
            });

            ui.separator();
            ui.strong("Plan");
            match &self.plan {
                Some(Ok(p)) => {
                    let ids = self.planned_clip_ids();
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        if let Some(t) = p.target_idx
                            && self.project.export.output_mode == OutputMode::Joined
                        {
                            ui.label(format!("Target format: {}", p.signatures[t].0.label()));
                        }
                        for v in &p.clips {
                            let name = ids
                                .get(v.clip_idx)
                                .and_then(|id| self.clip(*id))
                                .and_then(|c| c.path.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(name).strong());
                                if v.reasons.is_empty() || v.copyable {
                                    ui.label(&v.summary);
                                } else {
                                    ui.colored_label(ui.visuals().warn_fg_color, &v.summary);
                                }
                            });
                            for w in &v.soft_warnings {
                                ui.weak(format!("    note: {w}"));
                            }
                        }
                        for w in &p.warnings {
                            ui.colored_label(ui.visuals().warn_fg_color, format!("Warning: {w}"));
                        }
                        ui.add_space(4.0);
                        for o in &p.outputs {
                            ui.label(format!("Output: {}  ({})", o.path.display(), timefmt::display(o.duration())));
                        }
                    });
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                None => {
                    ui.weak("Add clips first.");
                }
            }
            let unreadable = self.media.values().filter(|m| m.error.is_some()).count();
            let pending = self.media.values().filter(|m| m.info.is_none() && m.error.is_none()).count();
            if unreadable > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{unreadable} clip(s) can't be read and are left out"));
            }
            if pending > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{pending} clip(s) still being read; wait before exporting"));
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok = matches!(self.plan, Some(Ok(_))) && pending == 0;
                if ui.add_enabled(ok, egui::Button::new(RichText::new("Export").strong())).clicked() {
                    start = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if changed {
            self.project.export.output_name = (self.project.export.output_mode
                == OutputMode::Joined)
                .then(|| self.export.name.trim().to_string())
                .filter(|n| !n.is_empty())
                .map(|n| {
                    if n.to_lowercase().ends_with(".mp4") {
                        n
                    } else {
                        format!("{n}.mp4")
                    }
                });
            self.apply_output_settings();
            if self.script.is_none() {
                self.config.save();
            }
        }
        if modal.should_close() || close {
            self.export.dialog = false;
        }
        if start {
            self.export.dialog = false;
            self.start_export();
        }
    }

    pub fn start_export_from_script(&mut self) {
        self.export.dialog = false;
        self.start_export();
    }

    pub fn cancel_export(&mut self) {
        if let Some(j) = &self.export.job {
            j.cancel.cancel();
        }
    }

    pub fn close_export_windows(&mut self) {
        self.export.dialog = false;
        self.export.finished = None;
    }

    /// One-line result summary once an export has finished.
    pub fn export_finished_summary(&self) -> Option<String> {
        let f = self.export.finished.as_ref()?;
        Some(match &f.result {
            Ok(rs) => rs
                .iter()
                .map(|r| {
                    let v = &r.verification;
                    format!(
                        "{} ok={} video={:.3} expected={:.3} av_ms={:+.1} frames={}/{} problems={:?}",
                        r.path.display(),
                        v.ok(),
                        v.video_duration,
                        v.expected_duration,
                        (v.audio_duration - v.video_duration) * 1000.0,
                        v.frames,
                        v.expected_frames,
                        v.problems
                    )
                })
                .collect::<Vec<_>>()
                .join(" | "),
            Err(e) => format!("error: {e}"),
        })
    }

    fn start_export(&mut self) {
        let Some(tools) = self.tools.clone() else {
            self.set_status("ffmpeg not found");
            return;
        };
        let mut project = self.project.clone();
        project.clips.retain(|c| self.info(c.id).is_some());
        let infos: Vec<MediaInfo> = project
            .clips
            .iter()
            .map(|c| (*self.info(c.id).unwrap()).clone())
            .collect();
        let plan = match plan::build(&project, &infos, &self.encoders) {
            Ok(p) => p,
            Err(e) => {
                self.set_status(format!("cannot export: {e}"));
                return;
            }
        };
        // keep the player from holding a decoder busy while exporting
        self.player.command("set", &["pause", "yes"]);
        let cancel = CancelToken::new();
        let progress = Arc::new(Mutex::new(exec::Progress {
            fraction: 0.0,
            step: "Starting".into(),
        }));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        {
            let (cancel, progress, log) = (cancel.clone(), progress.clone(), log.clone());
            let settings = project.export.clone();
            std::thread::Builder::new()
                .name("export".into())
                .spawn(move || {
                    let sink: LogSink = {
                        let log = log.clone();
                        Arc::new(move |l: LogLine<'_>| {
                            let line = match l {
                                LogLine::Command(c) => format!("$ {c}"),
                                LogLine::Stderr(s) => format!("  {s}"),
                                LogLine::Info(s) => format!("i {s}"),
                            };
                            tracing::debug!("{line}");
                            let mut v = log.lock().unwrap();
                            v.push(line);
                            if v.len() > 5000 {
                                v.drain(..1000);
                            }
                        })
                    };
                    exec::cleanup_stale(None);
                    let opts = ExecOptions {
                        decode_check: DecodeCheck::Seams,
                        ..Default::default()
                    };
                    let mut on_progress = |p: exec::Progress| *progress.lock().unwrap() = p;
                    let r = exec::execute(
                        &tools,
                        &plan,
                        &infos,
                        &settings,
                        &opts,
                        &cancel,
                        &sink,
                        &mut on_progress,
                    )
                    .map_err(|e| e.to_string());
                    let _ = tx.send(r);
                })
                .expect("spawning export thread");
        }
        self.export.job = Some(Job {
            cancel,
            progress,
            log,
            rx,
            started: Instant::now(),
            show_log: false,
        });
    }

    fn progress_window(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.export.job else {
            return;
        };
        let p = job.progress.lock().unwrap().clone();
        let elapsed = job.started.elapsed();
        egui::Modal::new(egui::Id::unique("export-progress")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Exporting…");
            ui.add(egui::ProgressBar::new(p.fraction as f32).show_percentage());
            ui.label(&p.step);
            let eta = if p.fraction > 0.02 {
                let total = elapsed.as_secs_f64() / p.fraction;
                format!(", about {} left", fmt_dur(total - elapsed.as_secs_f64()))
            } else {
                String::new()
            };
            ui.weak(format!("elapsed {}{eta}", fmt_dur(elapsed.as_secs_f64())));
            ui.horizontal(|ui| {
                if job.cancel.is_cancelled() {
                    ui.label("Cancelling…");
                } else if ui.button("Cancel").clicked() {
                    job.cancel.cancel();
                }
                ui.toggle_value(&mut job.show_log, "Show log");
            });
            if job.show_log {
                log_view(ui, &job.log.lock().unwrap());
            }
        });
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn finished_window(&mut self, ctx: &egui::Context) {
        let Some(f) = &mut self.export.finished else {
            return;
        };
        let mut close = false;
        let mut retry_full = false;
        let modal = egui::Modal::new(egui::Id::unique("export-finished")).show(ctx, |ui| {
            ui.set_width(620.0);
            match &f.result {
                Ok(results) => {
                    let all_ok = results.iter().all(|r| r.verification.ok());
                    ui.heading(if all_ok {
                        "Export finished"
                    } else {
                        "Export finished with warnings"
                    });
                    ui.weak(format!("in {}", fmt_dur(f.elapsed.as_secs_f64())));
                    for r in results {
                        let v = &r.verification;
                        ui.separator();
                        ui.label(RichText::new(r.path.display().to_string()).strong());
                        ui.label(format!(
                            "{}  ·  video {}  ·  audio/video {:+.0} ms",
                            if v.ok() {
                                "Verified OK"
                            } else {
                                "Verification problems"
                            },
                            timefmt::display(v.video_duration),
                            (v.audio_duration - v.video_duration) * 1000.0
                        ));
                        for p in &v.problems {
                            ui.colored_label(ui.visuals().warn_fg_color, format!("Problem: {p}"));
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Show in folder").clicked() {
                                reveal(&r.path);
                            }
                            if ui.button("Play").clicked() {
                                open_with_default_app(&r.path);
                            }
                        });
                    }
                    if !all_ok && self.project.export.encoding == EncodingMode::Smart {
                        ui.add_space(6.0);
                        if ui.button("Re-export with Full re-encode").clicked() {
                            retry_full = true;
                        }
                    }
                }
                Err(e) if e.contains("cancelled") => {
                    ui.heading("Export cancelled");
                    ui.label("Temporary and partial files were removed.");
                }
                Err(e) => {
                    ui.heading("Export failed");
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
                ui.toggle_value(&mut f.show_log, "Show log");
                if ui.button("Copy log").clicked() {
                    ui.ctx().copy_text(f.log.join("\n"));
                }
            });
            if f.show_log {
                log_view(ui, &f.log);
            }
        });
        if modal.should_close() || close {
            self.export.finished = None;
        }
        if retry_full {
            self.export.finished = None;
            self.project.export.encoding = EncodingMode::Full;
            self.mark_plan_dirty();
            self.start_export();
        }
    }
}

fn log_view(ui: &mut egui::Ui, log: &[String]) {
    egui::ScrollArea::vertical()
        .max_height(260.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for l in log.iter().rev().take(400).rev() {
                ui.monospace(l);
            }
        });
}

fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Opens the file manager with `path` selected (or its folder).
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let dir = path.parent().map(PathBuf::from).unwrap_or_default();
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

pub fn open_with_default_app(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}
