//! Export dialog, background export job, progress and finish windows (§8.6).

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use keepcut_core::exec::{self, ExecOptions, OutputResult};
use keepcut_core::ffmpeg::{CancelToken, LogLine, LogSink};
use keepcut_core::compat::Signature;
use keepcut_core::encoders::{self, EncoderSet};
use keepcut_core::chapters::{self, Chapter};
use keepcut_core::fsutil;
use keepcut_core::model::{
    AudioTracks, Chapters, ContentType, EncodingMode, FullEncodeSettings, Loudness, OutputMode,
    Quality, Resolution, VideoCodec,
};
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
    /// The user typed a name; otherwise it follows the first clip.
    name_edited: bool,
    job: Option<Job>,
    finished: Option<Finished>,
    /// Every export command and message of this session (the command log panel).
    session_log: Arc<Mutex<Vec<String>>>,
    log_panel: bool,
}

struct Job {
    cancel: CancelToken,
    progress: Arc<Mutex<exec::Progress>>,
    log: Arc<Mutex<Vec<String>>>,
    rx: Receiver<Result<Vec<OutputResult>, String>>,
    started: Instant,
    show_log: bool,
    /// Chapters of each output (for "Copy YouTube chapters").
    chapters: Vec<Vec<Chapter>>,
}

struct Finished {
    result: Result<Vec<OutputResult>, String>,
    log: Vec<String>,
    elapsed: Duration,
    show_log: bool,
    chapters: Vec<Vec<Chapter>>,
}

impl ExportUi {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }
    pub fn log_panel_open(&self) -> bool {
        self.log_panel
    }
    /// Shows the log in the finish window (script command `show-export-log`).
    pub fn show_finished_log(&mut self) {
        if let Some(f) = &mut self.finished {
            f.show_log = true;
        }
    }
    pub fn toggle_log_panel(&mut self) {
        self.log_panel = !self.log_panel;
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
        self.export.name_edited = self.project.export.output_name.is_some();
        self.export.name = match &self.project.export.output_name {
            Some(n) => n.clone(),
            None => self.default_joined_name(),
        };
        self.export.dialog = true;
    }

    /// `<first enabled, readable clip> - joined.mp4` (the planner's default, §6.12).
    fn default_joined_name(&self) -> String {
        self.project
            .clips
            .iter()
            .find(|c| c.enabled && self.info(c.id).is_some())
            .and_then(|c| c.path.file_stem())
            .map(|s| format!("{} - joined.mp4", s.to_string_lossy()))
            .unwrap_or_default()
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
        let result = match job.rx.try_recv() {
            Ok(r) => Some(r),
            // the export thread died without reporting (a panic): don't hang the UI
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("the export stopped unexpectedly (see the log)".into()))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        };
        if let Some(result) = result {
            let log = job.log.lock().unwrap().clone();
            self.export.finished = Some(Finished {
                result,
                log,
                elapsed: job.started.elapsed(),
                show_log: false,
                chapters: job.chapters.clone(),
            });
            self.export.job = None;
        }
    }

    /// The command log panel (§6.10): every ffmpeg command of this session,
    /// shell-quoted and copyable.
    pub fn log_panel(&mut self, ctx: &egui::Context) {
        if !self.export.log_panel {
            return;
        }
        let mut open = true;
        let session = self.export.session_log.clone();
        let screen = ctx.content_rect();
        egui::Window::new("Command log")
            .open(&mut open)
            .resizable(true)
            .default_size([screen.width() * 0.6, screen.height() * 0.6])
            .min_size([320.0, 160.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Copy all").clicked() {
                        ui.ctx().copy_text(session.lock().unwrap().join("\n"));
                    }
                    if ui.button("Clear").clicked() {
                        session.lock().unwrap().clear();
                    }
                    ui.weak("Commands of this session's exports; the full ffmpeg output is in each export's log");
                });
                let lines = session.lock().unwrap();
                if lines.is_empty() {
                    ui.weak("No exports yet.");
                }
                // fill the window, whatever size the user drags it to
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| log_lines(ui, &lines));
            });
        self.export.log_panel = open;
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
                    if !self.export.name_edited {
                        self.export.name = self.default_joined_name();
                    }
                    if ui.add(egui::TextEdit::singleline(&mut self.export.name).desired_width(400.0)).changed() {
                        self.export.name_edited = true;
                        changed = true;
                    }
                    ui.end_row();
                }

                // target format (joined mode with clips of different formats)
                // signatures sorted by kept duration: the first is the automatic choice
                let sigs: Vec<Signature> = match &self.plan {
                    Some(Ok(p)) => p.signatures.iter().map(|(s, _)| s.clone()).collect(),
                    _ => Vec::new(),
                };
                if self.project.export.output_mode == OutputMode::Joined && sigs.len() > 1 {
                    ui.label("Target format");
                    let x = &mut self.project.export;
                    let auto = format!("Automatic: {}", sigs[0].label());
                    let shown = match &x.target_override {
                        Some(s) if sigs.contains(s) => s.label(),
                        _ => auto.clone(),
                    };
                    egui::ComboBox::from_id_salt("target-format").selected_text(shown).width(400.0).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut x.target_override, None, auto).changed();
                        for s in &sigs {
                            changed |= ui.selectable_value(&mut x.target_override, Some(s.clone()), s.label()).changed();
                        }
                    })
                    .response
                    .on_hover_text("Clips in this format are copied; the others are re-encoded to match it");
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
                        .radio_value(e, EncodingMode::Full, "Full re-encode")
                        .on_hover_text("Re-encodes everything with one set of settings: a smaller file, another codec, size or frame rate, or the safe fallback")
                        .changed();
                });
                ui.end_row();

                let target_codec = match &self.plan {
                    Some(Ok(p)) => p.target_idx.and_then(|t| p.signatures.get(t)).map(|(s, _)| s.codec_name.clone()),
                    _ => None,
                };
                let x = &mut self.project.export;
                match x.encoding {
                    EncodingMode::Smart => {
                        let codec = target_codec.as_deref().and_then(encoders::codec_of_name).unwrap_or(VideoCodec::H264);
                        ui.label("Re-encoded clips").on_hover_text(
                            "Clips that don't match the target format are re-encoded whole. \
                             The short parts from a cut to the next keyframe always use the software encoder.",
                        );
                        ui.vertical(|ui| {
                            changed |= encoder_combo(ui, "smart-enc", &mut x.smart_encoder, codec, &self.encoders);
                            changed |= quality_ui(ui, "smart-q", &mut x.smart_quality);
                        });
                        ui.end_row();
                    }
                    EncodingMode::Full => {
                        ui.label("Video");
                        ui.vertical(|ui| full_options(ui, &mut x.full, &self.encoders, &mut changed));
                        ui.end_row();
                    }
                }

                ui.label("Audio");
                ui.horizontal(|ui| {
                    changed |= ui.radio_value(&mut x.audio_tracks, AudioTracks::MixAll, "Mix all tracks").changed();
                    changed |= ui.radio_value(&mut x.audio_tracks, AudioTracks::FirstOnly, "First track only").changed();
                    ui.add_space(12.0);
                    ui.label("AAC");
                    egui::ComboBox::from_id_salt("aac-rate")
                        .selected_text(format!("{} kbps", x.audio_bitrate_kbps))
                        .show_ui(ui, |ui| {
                            for r in [128, 160, 192, 256, 320] {
                                changed |= ui.selectable_value(&mut x.audio_bitrate_kbps, r, format!("{r} kbps")).changed();
                            }
                        });
                });
                ui.end_row();

                ui.label("Loudness").on_hover_text(
                    "Evens out the volume: measures the integrated loudness (EBU R128) and applies \
                     one gain per unit, up to ±20 dB, with a limiter at −1 dB. Silence is left alone.",
                );
                ui.horizontal(|ui| {
                    let shown = match x.loudness {
                        Loudness::Off => "Off",
                        Loudness::Whole => "Whole video",
                        Loudness::PerClip => "Per clip",
                        Loudness::PerSegment => "Per segment",
                    };
                    egui::ComboBox::from_id_salt("loudness").selected_text(shown).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut x.loudness, Loudness::Off, "Off").changed();
                        changed |= ui
                            .selectable_value(&mut x.loudness, Loudness::Whole, "Whole video")
                            .on_hover_text("One gain for the whole file (each file in the per-clip/per-segment modes)")
                            .changed();
                        changed |= ui
                            .selectable_value(&mut x.loudness, Loudness::PerClip, "Per clip")
                            .on_hover_text("One gain per source clip: evens out recordings made at different levels")
                            .changed();
                        changed |= ui
                            .selectable_value(&mut x.loudness, Loudness::PerSegment, "Per segment")
                            .on_hover_text("One gain per kept segment; segments shorter than 3 s use their clip's value")
                            .changed();
                    });
                    if x.loudness != Loudness::Off {
                        ui.label("target");
                        changed |= ui
                            .add(egui::DragValue::new(&mut x.loudness_target).range(-30.0..=-5.0).speed(0.1).fixed_decimals(1).suffix(" LUFS"))
                            .on_hover_text("−14 LUFS is what YouTube and Spotify play at; −16 for podcasts, −23 for broadcast")
                            .changed();
                    }
                });
                ui.end_row();

                ui.label("Chapters").on_hover_text(
                    "Chapter marks in the MP4 (players show them on the seek bar). \
                     Titles: the clip's file name, or what you set in the clip list / segment list.",
                );
                ui.horizontal(|ui| {
                    changed |= ui.radio_value(&mut x.chapters, Chapters::None, "None").changed();
                    changed |= ui.radio_value(&mut x.chapters, Chapters::PerClip, "Per clip").changed();
                    changed |= ui.radio_value(&mut x.chapters, Chapters::PerSegment, "Per segment").changed();
                });
                ui.end_row();
            });
            // YouTube reads chapters from the description, not the file (§6.13)
            if self.project.export.chapters != Chapters::None
                && let Some(Ok(p)) = &self.plan
                && let Some(first) = p.outputs.first()
            {
                let ch = exec::build_chapters(first, self.project.export.chapters);
                youtube_button(ui, &ch, p.outputs.len() > 1);
            }

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
                                if v.reasons.is_empty() || v.copyable || p.encoding == EncodingMode::Full {
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
                            ui.label(format!(
                                "Output: {}  ({}, about {})",
                                o.path.display(),
                                timefmt::display(o.duration()),
                                fsutil::human_bytes(o.est_bytes)
                            ));
                        }
                    });
                    // disk space (§6.10): the export checks this again before it starts
                    let temp = p.outputs.iter().map(|o| o.est_temp_bytes).max().unwrap_or(0);
                    let total: u64 = p.outputs.iter().map(|o| o.est_bytes).sum();
                    let temp_dir = self.config.temp_dir.clone().unwrap_or_else(std::env::temp_dir);
                    let temp_free = fsutil::free_space(&temp_dir);
                    let out_dir = p.outputs.first().and_then(|o| o.path.parent()).map(Path::to_path_buf);
                    let out_free = out_dir.as_deref().and_then(fsutil::free_space);
                    let free = |f: Option<u64>| f.map_or("unknown".into(), fsutil::human_bytes);
                    ui.weak(format!(
                        "Needs about {} of temporary space ({} free) and {} for the output ({} free)",
                        fsutil::human_bytes(temp),
                        free(temp_free),
                        fsutil::human_bytes(total),
                        free(out_free)
                    ))
                    .on_hover_text("Estimates. Copied parts cost what they cost in the source; re-encoded parts are guessed from the source bitrate.");
                    if out_free.is_some_and(|f| f < total) {
                        ui.colored_label(ui.visuals().error_fg_color, "Not enough space in the output folder");
                    } else if temp_free.is_some_and(|f| f < temp) {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Not enough temporary space: temporary files will go to the output folder",
                        );
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                None => {
                    ui.weak("Add clips first.");
                }
            }
            let unreadable = self.unreadable_count();
            let pending = self.pending_count();
            let no_folder = self.config.output_location == OutputLocation::Custom && self.config.custom_output_dir.is_none();
            if no_folder {
                ui.colored_label(ui.visuals().warn_fg_color, "Choose a custom folder (Browse…) or use the source folder");
            }
            if unreadable > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{unreadable} clip(s) can't be read and are left out"));
            }
            if pending > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{pending} clip(s) still being read; wait before exporting"));
            }
            egui::CollapsingHeader::new("Encoders on this computer").id_salt("enc-list").show(ui, |ui| {
                if !self.encoders_detected() {
                    ui.weak("Still testing the hardware encoders…");
                }
                for e in &self.encoders.working {
                    ui.label(format!("{}: works", encoders::describe(e)));
                }
                for (e, why) in &self.encoders.failed {
                    ui.weak(format!("{}: not available ({why})", encoders::describe(e)));
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok = matches!(self.plan, Some(Ok(_))) && pending == 0 && !no_folder;
                if ui.add_enabled(ok, egui::Button::new(RichText::new("Export").strong())).clicked() {
                    start = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if changed {
            // only a name the user typed is kept; otherwise the default follows the clips
            self.project.export.output_name =
                (self.project.export.output_mode == OutputMode::Joined && self.export.name_edited)
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
        }
        if (modal.should_close() || close || start) && self.script.is_none() {
            // remember the folder and export choices (once, not on every keystroke)
            self.config.remember_export(&self.project.export);
            self.config.save();
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

    /// On exit: cancel a running export and give it a moment to kill ffmpeg and
    /// remove its temporary and partial files.
    pub fn cancel_export_and_wait(&mut self) {
        if let Some(j) = self.export.job.take() {
            j.cancel.cancel();
            let _ = j.rx.recv_timeout(Duration::from_secs(5));
        }
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
        self.note_export_started();
        // keep the player from holding a decoder busy while exporting
        self.player.command("set", &["pause", "yes"]);
        let opts = ExecOptions {
            temp_root: self.config.temp_dir.clone(),
            decode_check: if self.config.full_decode_check {
                DecodeCheck::Full
            } else {
                DecodeCheck::Seams
            },
            ..Default::default()
        };
        let cancel = CancelToken::new();
        let progress = Arc::new(Mutex::new(exec::Progress {
            fraction: 0.0,
            step: "Starting".into(),
        }));
        let log = Arc::new(Mutex::new(Vec::new()));
        let session = self.export.session_log.clone();
        session.lock().unwrap().push(format!(
            "# export of {} output(s), {:?} mode",
            plan.outputs.len(),
            plan.encoding
        ));
        let chapters = plan
            .outputs
            .iter()
            .map(|o| exec::build_chapters(o, project.export.chapters))
            .collect();
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
                            // commands and notes also go to the session's command log
                            // and the log file; ffmpeg's stderr only to this job's log
                            let (line, keep) = match l {
                                LogLine::Command(c) => (format!("$ {c}"), true),
                                LogLine::Stderr(s) => (format!("  {s}"), false),
                                LogLine::Info(s) => (format!("i {s}"), true),
                            };
                            if keep {
                                tracing::info!("{line}");
                                let mut s = session.lock().unwrap();
                                s.push(line.clone());
                                if s.len() > 20000 {
                                    s.drain(..5000);
                                }
                            } else {
                                tracing::debug!("{line}");
                            }
                            let mut v = log.lock().unwrap();
                            v.push(line);
                            if v.len() > 5000 {
                                v.drain(..1000);
                            }
                        })
                    };
                    exec::cleanup_stale(opts.temp_root.as_deref());
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
            chapters,
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
                    for (i, r) in results.iter().enumerate() {
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
                        for n in &r.notes {
                            ui.weak(format!("Note: {n}"));
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Show in folder").clicked() {
                                reveal(&r.path);
                            }
                            if ui.button("Play").clicked() {
                                open_with_default_app(&r.path);
                            }
                            if let Some(ch) = f.chapters.get(i).filter(|c| !c.is_empty()) {
                                youtube_button(ui, ch, false);
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

/// "Copy YouTube chapters": `0:00 Title` lines for the video description, with
/// YouTube's rules checked (§6.13).
fn youtube_button(ui: &mut egui::Ui, ch: &[Chapter], first_of_many: bool) {
    let (text, warn) = chapters::youtube_block(ch);
    ui.vertical(|ui| {
        let label = if first_of_many {
            "Copy YouTube chapters (first file)"
        } else {
            "Copy YouTube chapters"
        };
        if ui
            .button(label)
            .on_hover_text(format!(
                "YouTube ignores chapters inside the file; paste these lines into the video description:\n\n{text}"
            ))
            .clicked()
        {
            ui.ctx().copy_text(text.clone());
        }
        for w in warn {
            ui.colored_label(ui.visuals().warn_fg_color, format!("YouTube: {w}"));
        }
    });
}

/// Encoder dropdown: Auto + the working encoders for `codec`. Returns "changed".
fn encoder_combo(
    ui: &mut egui::Ui,
    id: &str,
    choice: &mut Option<String>,
    codec: VideoCodec,
    set: &EncoderSet,
) -> bool {
    let mut changed = false;
    let options: Vec<&String> = set
        .working
        .iter()
        .filter(|e| encoders::codec_of_encoder(e) == Some(codec))
        .collect();
    // switching the codec keeps the encoder family: hevc_nvenc ↔ h264_nvenc,
    // libx265 ↔ libx264 (only once that encoder is known to work; until hardware
    // detection finishes, the choice is kept as it is)
    if let Some(c) = choice.clone()
        && encoders::codec_of_encoder(&c) != Some(codec)
        && let Some(other) = encoders::counterpart(&c, codec).filter(|o| options.contains(&o))
    {
        *choice = Some(other);
        changed = true;
    }
    ui.horizontal(|ui| {
        ui.label("Encoder");
        // a remembered encoder of another codec, or not (yet) detected as working,
        // is kept but not used: the planner falls back to Auto
        let shown = match choice.as_deref() {
            None => "Auto".to_string(),
            Some(c) if options.iter().any(|o| *o == c) => encoders::describe(c),
            Some(c) => format!("{} (not available here: Auto)", encoders::describe(c)),
        };
        egui::ComboBox::from_id_salt(id)
            .selected_text(shown)
            .width(260.0)
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(choice, None, "Auto")
                    .on_hover_text(
                        "Software for screen recordings below 4K (sharpest text, smallest files); \
                         otherwise the first working hardware encoder",
                    )
                    .changed();
                for e in options {
                    changed |= ui
                        .selectable_value(choice, Some(e.clone()), encoders::describe(e))
                        .changed();
                }
            });
    });
    changed
}

/// Quality: match source / CRF-CQ value / bitrate. Returns "changed".
fn quality_ui(ui: &mut egui::Ui, id: &str, q: &mut Quality) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Quality");
        let shown = match q {
            Quality::MatchSource => "Match source",
            Quality::Crf(_) => "Constant quality",
            Quality::BitrateKbps(_) => "Bitrate",
        };
        egui::ComboBox::from_id_salt(id)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                let was = q.clone();
                if ui
                    .selectable_label(matches!(q, Quality::MatchSource), "Match source")
                    .on_hover_text("Calibrated presets that look like the source (x264 CRF 18, NVENC CQ 19, …)")
                    .clicked()
                {
                    *q = Quality::MatchSource;
                }
                if ui
                    .selectable_label(matches!(q, Quality::Crf(_)), "Constant quality")
                    .on_hover_text("CRF (software) or CQ/QP (hardware): lower = better and larger; 18–28 is typical")
                    .clicked()
                    && !matches!(q, Quality::Crf(_))
                {
                    *q = Quality::Crf(23);
                }
                if ui
                    .selectable_label(matches!(q, Quality::BitrateKbps(_)), "Bitrate")
                    .on_hover_text("Average video bitrate; predictable size")
                    .clicked()
                    && !matches!(q, Quality::BitrateKbps(_))
                {
                    *q = Quality::BitrateKbps(8000);
                }
                changed |= *q != was;
            });
        match q {
            Quality::Crf(c) => {
                changed |= ui.add(egui::DragValue::new(c).range(0..=51)).changed();
            }
            Quality::BitrateKbps(b) => {
                changed |= ui
                    .add(egui::DragValue::new(b).range(100..=200_000).speed(50).suffix(" kbps"))
                    .changed();
            }
            Quality::MatchSource => {}
        }
    });
    changed
}

/// Codec, encoder, quality, resolution, frame rate and content type (§6.0).
fn full_options(ui: &mut egui::Ui, f: &mut FullEncodeSettings, set: &EncoderSet, changed: &mut bool) {
    ui.horizontal(|ui| {
        ui.label("Codec");
        *changed |= ui.radio_value(&mut f.codec, VideoCodec::H264, "H.264").on_hover_text("Plays everywhere").changed();
        *changed |= ui
            .radio_value(&mut f.codec, VideoCodec::Hevc, "HEVC (H.265)")
            .on_hover_text("About 30% smaller at the same quality; keeps 10-bit sources 10-bit")
            .changed();
    });
    *changed |= encoder_combo(ui, "full-enc", &mut f.encoder, f.codec, set);
    *changed |= quality_ui(ui, "full-q", &mut f.quality);
    ui.horizontal(|ui| {
        ui.label("Size");
        let shown = match f.resolution {
            Resolution::Same => "Same as source".to_string(),
            Resolution::ShortSide(s) => format!("{s}p"),
            Resolution::Custom(..) => "Custom".to_string(),
        };
        egui::ComboBox::from_id_salt("full-res")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                *changed |= ui.selectable_value(&mut f.resolution, Resolution::Same, "Same as source").changed();
                for s in [2160, 1440, 1080, 720] {
                    *changed |= ui
                        .selectable_value(&mut f.resolution, Resolution::ShortSide(s), format!("{s}p"))
                        .on_hover_text("The short side; portrait video stays portrait and keeps its aspect ratio")
                        .changed();
                }
                if ui
                    .selectable_label(matches!(f.resolution, Resolution::Custom(..)), "Custom")
                    .clicked()
                    && !matches!(f.resolution, Resolution::Custom(..))
                {
                    f.resolution = Resolution::Custom(1920, 1080);
                    *changed = true;
                }
            });
        if let Resolution::Custom(w, h) = &mut f.resolution {
            *changed |= ui.add(egui::DragValue::new(w).range(64..=7680)).changed();
            ui.label("×");
            *changed |= ui.add(egui::DragValue::new(h).range(64..=4320)).changed();
        }
        ui.add_space(12.0);
        ui.label("Frame rate");
        let shown = f.fps.map_or("Keep original timing".to_string(), |r| format!("{r} fps"));
        egui::ComboBox::from_id_salt("full-fps")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                *changed |= ui
                    .selectable_value(&mut f.fps, None, "Keep original timing")
                    .on_hover_text("Every frame keeps its time (variable frame rate stays variable): no duplicated frames")
                    .changed();
                for r in [15, 24, 25, 30, 50, 60] {
                    *changed |= ui.selectable_value(&mut f.fps, Some(r), format!("{r} fps")).changed();
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label("Content");
        let shown = match f.content {
            ContentType::Auto => "Detect",
            ContentType::Screen => "Screen recording",
            ContentType::Camera => "Camera",
        };
        egui::ComboBox::from_id_salt("full-content")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                *changed |= ui
                    .selectable_value(&mut f.content, ContentType::Auto, "Detect")
                    .on_hover_text("Screen recording when the file says Bandicam/OBS or skips unchanged frames")
                    .changed();
                *changed |= ui
                    .selectable_value(&mut f.content, ContentType::Screen, "Screen recording")
                    .on_hover_text("Keyframe every 10 s, no B-frames, software encoder by default below 4K")
                    .changed();
                *changed |= ui
                    .selectable_value(&mut f.content, ContentType::Camera, "Camera")
                    .on_hover_text("Keyframe every 2 s, B-frames, hardware encoder by default")
                    .changed();
            });
    });
}

/// Log lines in a scroll area. In a dialog the area gets half the window's
/// height; without an explicit minimum, egui shrinks it to ~4 lines there.
fn log_view(ui: &mut egui::Ui, log: &[String]) {
    let h = (ui.ctx().content_rect().height() * 0.5).max(160.0);
    egui::ScrollArea::vertical()
        .min_scrolled_height(h)
        .max_height(h)
        .auto_shrink([false, true])
        .stick_to_bottom(true)
        .show(ui, |ui| log_lines(ui, log));
}

fn log_lines(ui: &mut egui::Ui, log: &[String]) {
    for l in log.iter().rev().take(2000).rev() {
        ui.monospace(l);
    }
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
        // Explorer wants `/select,"C:\a b\c.mp4"`: the standard quoting of the whole
        // argument ("/select,C:\a b\c.mp4") makes it ignore the path and open
        // Documents. Windows paths can't contain `"`, so raw quoting is safe.
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let dir = path
            .parent()
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
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
