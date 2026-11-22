//! Export dialog, background export job, progress and finish windows (§8.6).

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use keepcut_core::chapters::{self, Chapter};
use keepcut_core::compat::Signature;
use keepcut_core::encoders::{self, EncoderSet};
use keepcut_core::exec::{self, ExecOptions, OutputResult};
use keepcut_core::ffmpeg::{CancelToken, LogLine, LogSink};
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
            ui.set_width(640.0);
            ui.heading("Export");
            // disk space (§6.10): the export checks this again before it starts
            let space = match &self.plan {
                Some(Ok(p)) => Some(self.disk_space(p)),
                _ => None,
            };
            // the options and the plan scroll; the buttons below them stay in view
            let max_height = (ctx.content_rect().height() - 180.0).max(200.0);
            egui::ScrollArea::vertical()
                .id_salt("export-options")
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {

                    // target format (joined mode with clips of different formats)
                    // signatures sorted by kept duration: the first is the automatic choice
                    let sigs: Vec<Signature> = match &self.plan {
                        Some(Ok(p)) => p.signatures.iter().map(|(s, _)| s.clone()).collect(),
                        _ => Vec::new(),
                    };
                    let target_codec = match &self.plan {
                        Some(Ok(p)) => p.target_idx.and_then(|t| p.signatures.get(t)).map(|(s, _)| s.codec_name.clone()),
                        _ => None,
                    };
                    // a soundtrack replaces the audio of a joined output
                    let soundtrack = if self.soundtrack_active() { self.soundtrack_name() } else { None };
            // the track choice only matters for files with several audio tracks
            let multi_track = self
                .planned_clip_ids()
                .iter()
                .filter_map(|id| self.info(*id))
                .filter(|i| i.audio.len() > 1)
                .count();

                    section(ui, "Files", |ui| {
                        row(ui, "Output", None, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let m = &mut self.project.export.output_mode;
                                changed |= ui.radio_value(m, OutputMode::Joined, "Single joined file").changed();
                                changed |= ui.radio_value(m, OutputMode::PerClip, "One file per clip").changed();
                                changed |= ui.radio_value(m, OutputMode::PerSegment, "One file per segment").changed();
                            });
                        });
                        row(ui, "Folder", None, |ui| {
                            let mut loc = self.config.output_location;
                            changed |= ui.radio_value(&mut loc, OutputLocation::SameAsSource, "Same folder as the source").changed();
                            ui.horizontal(|ui| {
                                changed |= ui.radio_value(&mut loc, OutputLocation::Custom, "Custom:").changed();
                                if ui.button("Browse…").clicked()
                                    && let Some(d) = rfd::FileDialog::new().pick_folder()
                                {
                                    self.config.custom_output_dir = Some(d);
                                    loc = OutputLocation::Custom;
                                    changed = true;
                                }
                                let shown = self.config.custom_output_dir.as_ref().map_or("(none)".into(), |p| p.display().to_string());
                                ui.add(egui::Label::new(shown).truncate());
                            });
                            self.config.output_location = loc;
                        });
                        if self.project.export.output_mode == OutputMode::Joined {
                            row(ui, "File name", None, |ui| {
                                if !self.export.name_edited {
                                    self.export.name = self.default_joined_name();
                                }
                                if ui.add(egui::TextEdit::singleline(&mut self.export.name).desired_width(f32::INFINITY)).changed() {
                                    self.export.name_edited = true;
                                    changed = true;
                                }
                            });
                            if sigs.len() > 1 {
                                row(ui, "Target format", Some("Clips in this format are copied; the others are re-encoded to match it"), |ui| {
                                    let x = &mut self.project.export;
                                    let auto = format!("Automatic: {}", sigs[0].label());
                                    let shown = match &x.target_override {
                                        Some(s) if sigs.contains(s) => s.label(),
                                        _ => auto.clone(),
                                    };
                                    egui::ComboBox::from_id_salt("target-format").selected_text(shown).width(ui.available_width()).show_ui(ui, |ui| {
                                        changed |= ui.selectable_value(&mut x.target_override, None, auto).changed();
                                        for s in &sigs {
                                            changed |= ui.selectable_value(&mut x.target_override, Some(s.clone()), s.label()).changed();
                                        }
                                    });
                                });
                            }
                        }
                    });

                    section(ui, "Video", |ui| {
                        let x = &mut self.project.export;
                        row(ui, "Encoding", None, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let e = &mut x.encoding;
                                changed |= ui
                                    .radio_value(e, EncodingMode::Smart, "Smart")
                                    .on_hover_text(
                                        "Lossless where possible: copies the original video and re-encodes only \
                                         from a cut to the next keyframe, and clips that don't match the target format",
                                    )
                                    .changed();
                                changed |= ui
                                    .radio_value(e, EncodingMode::Full, "Full re-encode")
                                    .on_hover_text("Re-encodes everything with one set of settings: a smaller file, another codec, size or frame rate, or the safe fallback")
                                    .changed();
                            });
                        });
                        match x.encoding {
                            EncodingMode::Smart => {
                                let codec = target_codec.as_deref().and_then(encoders::codec_of_name).unwrap_or(VideoCodec::H264);
                                let tip = "For clips that don't match the target format: they are re-encoded whole. \
                                           The short parts from a cut to the next keyframe always use the software encoder.";
                                row(ui, "Encoder", Some(tip), |ui| {
                                    changed |= encoder_combo(ui, "smart-enc", &mut x.smart_encoder, codec, &self.encoders);
                                });
                                row(ui, "Quality", Some(tip), |ui| {
                                    changed |= quality_ui(ui, "smart-q", &mut x.smart_quality);
                                });
                                ui.weak("Encoder and quality apply to clips that don't match the target format.");
                            }
                            EncodingMode::Full => full_options(ui, &mut x.full, &self.encoders, &mut changed),
                        }
                    });

                    section(ui, "Audio", |ui| {
                        let x = &mut self.project.export;
                        let tracks_tip = "What to do when a video has more than one audio track, e.g. a \
                                          screen recording with the desktop sound and the microphone on separate \
                                          tracks. The exported file always has one audio track.";
                        // shown only when it matters: a soundtrack, or a clip with several tracks
                        if let Some(name) = &soundtrack {
                            row(ui, "Sound", None, |ui| {
                                ui.label(format!("Soundtrack ♪ {name}"))
                                    .on_hover_text("Replaces the clips' sound (set under the clip list)");
                            });
                        } else if multi_track > 0 {
                            row(ui, "Tracks", Some(tracks_tip), |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    changed |= ui
                                        .radio_value(&mut x.audio_tracks, AudioTracks::MixAll, "Mix all tracks")
                                        .on_hover_text("All of a video's audio tracks play together, each at its own level (desktop sound and microphone both heard)")
                                        .changed();
                                    changed |= ui
                                        .radio_value(&mut x.audio_tracks, AudioTracks::FirstOnly, "First track only")
                                        .on_hover_text("Only the video's first audio track is kept; the others are left out (e.g. a separate microphone track)")
                                        .changed();
                                });
                                ui.weak(format!(
                                    "{multi_track} clip{} more than one audio track.",
                                    if multi_track == 1 { " has" } else { "s have" }
                                ));
                            });
                        }
                        row(ui, "AAC bitrate", None, |ui| {
                            egui::ComboBox::from_id_salt("aac-rate")
                                .selected_text(format!("{} kbps", x.audio_bitrate_kbps))
                                .show_ui(ui, |ui| {
                                    for r in [128, 160, 192, 256, 320] {
                                        changed |= ui.selectable_value(&mut x.audio_bitrate_kbps, r, format!("{r} kbps")).changed();
                                    }
                                });
                        });
                        let tip = "Evens out the volume: measures the integrated loudness (EBU R128) and applies \
                                   one gain per unit, up to ±20 dB, with a limiter at −1 dB. Silence is left alone.";
                        row(ui, "Normalize", Some(tip), |ui| {
                            ui.horizontal(|ui| {
                                // (a per-unit choice from before the soundtrack is exported as "whole")
                                let shown = match x.loudness {
                                    Loudness::Off => "Off",
                                    Loudness::Whole => "Whole video",
                                    Loudness::PerClip | Loudness::PerSegment if soundtrack.is_some() => "Whole video",
                                    Loudness::PerClip => "Per clip",
                                    Loudness::PerSegment => "Per segment",
                                };
                                egui::ComboBox::from_id_salt("loudness").selected_text(shown).show_ui(ui, |ui| {
                                    changed |= ui.selectable_value(&mut x.loudness, Loudness::Off, "Off").changed();
                                    changed |= ui
                                        .selectable_value(&mut x.loudness, Loudness::Whole, "Whole video")
                                        .on_hover_text("One gain for the whole file (each file in the per-clip/per-segment modes)")
                                        .changed();
                                    // with a soundtrack there is one piece of audio: only "whole" applies
                                    let per_unit = soundtrack.is_none();
                                    let off_tip = "Not with a soundtrack (it is one piece of audio)";
                                    for (mode, label, tip) in [
                                        (Loudness::PerClip, "Per clip", "One gain per source clip: evens out recordings made at different levels"),
                                        (Loudness::PerSegment, "Per segment", "One gain per kept segment; segments shorter than 3 s use their clip's value"),
                                    ] {
                                        let item = egui::Button::selectable(x.loudness == mode, label);
                                        if ui.add_enabled(per_unit, item).on_hover_text(tip).on_disabled_hover_text(off_tip).clicked() {
                                            x.loudness = mode;
                                            changed = true;
                                        }
                                    }
                                });
                                if x.loudness != Loudness::Off {
                                    ui.label("target");
                                    changed |= ui
                                        .add(egui::DragValue::new(&mut x.loudness_target).range(-30.0..=-5.0).speed(0.1).fixed_decimals(1).suffix(" LUFS"))
                                        .on_hover_text("−14 LUFS is what YouTube and Spotify play at; −16 for podcasts, −23 for broadcast")
                                        .changed();
                                }
                            });
                        });
                    });

                    // chapters are off by default: the section starts closed unless they're on
                    let chapters_label = match self.project.export.chapters {
                        Chapters::None => "None",
                        Chapters::PerClip => "Per clip",
                        Chapters::PerSegment => "Per segment",
                    };
                    collapsible_section(
                        ui,
                        "export-chapters",
                        "Chapters",
                        self.project.export.chapters != Chapters::None,
                        |ui| {
                            ui.weak(format!("· {chapters_label}"));
                        },
                        |ui| {
                            let tip = "Chapter marks in the MP4 (players show them on the seek bar). \
                                       Titles: the clip's file name, or what you set in the clip list / segment list.";
                            row(ui, "In the file", Some(tip), |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let x = &mut self.project.export;
                                    changed |= ui.radio_value(&mut x.chapters, Chapters::None, "None").changed();
                                    changed |= ui.radio_value(&mut x.chapters, Chapters::PerClip, "Per clip").changed();
                                    changed |= ui.radio_value(&mut x.chapters, Chapters::PerSegment, "Per segment").changed();
                                });
                            });
                            // YouTube reads chapters from the description, not the file (§6.13)
                            if self.project.export.chapters != Chapters::None
                                && let Some(Ok(p)) = &self.plan
                                && let Some(first) = p.outputs.first()
                            {
                                let ch = exec::build_chapters(first, self.project.export.chapters);
                                row(ui, "YouTube", None, |ui| youtube_button(ui, &ch, p.outputs.len() > 1));
                            }
                        },
                    );

                    // the plan's details are for when something looks wrong: closed, with
                    // the outcome in the header (warnings are shown by the buttons)
                    collapsible_section(
                        ui,
                        "export-plan",
                        "Plan",
                        false,
                        |ui| match &self.plan {
                            Some(Ok(p)) => {
                                let n = p.outputs.len();
                                let duration: f64 = p.outputs.iter().map(|o| o.duration()).sum();
                                let bytes: u64 = p.outputs.iter().map(|o| o.est_bytes).sum();
                                ui.weak(format!(
                                    "· {n} file{} · {} · about {}",
                                    if n == 1 { "" } else { "s" },
                                    timefmt::display(duration),
                                    fsutil::human_bytes(bytes)
                                ));
                                let re = p
                                    .clips
                                    .iter()
                                    .filter(|v| p.encoding == EncodingMode::Smart && !v.copyable && !v.reasons.is_empty())
                                    .count();
                                if re > 0 {
                                    ui.colored_label(
                                        ui.visuals().warn_fg_color,
                                        format!("· {re} clip{} re-encoded", if re == 1 { "" } else { "s" }),
                                    );
                                }
                            }
                            Some(Err(_)) => {
                                ui.colored_label(ui.visuals().error_fg_color, "· can't export");
                            }
                            None => {
                                ui.weak("· no clips");
                            }
                        },
                        |ui| match &self.plan {
                            Some(Ok(p)) => {
                                let ids = self.planned_clip_ids();
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
                                ui.add_space(4.0);
                                for o in &p.outputs {
                                    ui.label(format!(
                                        "Output: {}  ({}, about {})",
                                        o.path.display(),
                                        timefmt::display(o.duration()),
                                        fsutil::human_bytes(o.est_bytes)
                                    ));
                                }
                                if let Some(sp) = &space {
                                    let free = |f: Option<u64>| f.map_or("unknown".into(), fsutil::human_bytes);
                                    ui.weak(format!(
                                        "Needs about {} of temporary space ({} free) and {} for the output ({} free)",
                                        fsutil::human_bytes(sp.temp),
                                        free(sp.temp_free),
                                        fsutil::human_bytes(sp.total),
                                        free(sp.out_free)
                                    ))
                                    .on_hover_text("Estimates. Copied parts cost what they cost in the source; re-encoded parts are guessed from the source bitrate.");
                                }
                                ui.add_space(4.0);
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
                            }
                            Some(Err(e)) => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            None => {
                                ui.weak("Add clips first.");
                            }
                        },
                    );
                });
            ui.add_space(4.0);
            let unreadable = self.unreadable_count();
            let pending = self.pending_count();
            let no_folder = self.config.output_location == OutputLocation::Custom && self.config.custom_output_dir.is_none();
            if no_folder {
                ui.colored_label(ui.visuals().warn_fg_color, "Choose a custom folder (Browse…) or use the source folder");
            }
            match &self.plan {
                Some(Ok(p)) => {
                    for w in &p.warnings {
                        ui.colored_label(ui.visuals().warn_fg_color, format!("Warning: {w}"));
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                None => {}
            }
            if let Some(sp) = &space {
                if sp.out_free.is_some_and(|f| f < sp.total) {
                    ui.colored_label(ui.visuals().error_fg_color, "Not enough space in the output folder");
                } else if sp.temp_free.is_some_and(|f| f < sp.temp) {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Not enough temporary space: temporary files will go to the output folder",
                    );
                }
            }
            if unreadable > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{unreadable} clip(s) can't be read and are left out"));
            }
            if pending > 0 {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{pending} clip(s) still being read; wait before exporting"));
            }
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

/// Codec, encoder, quality, resolution, frame rate and content type (§6.0), as rows.
fn full_options(
    ui: &mut egui::Ui,
    f: &mut FullEncodeSettings,
    set: &EncoderSet,
    changed: &mut bool,
) {
    row(ui, "Codec", None, |ui| {
        ui.horizontal(|ui| {
            *changed |= ui
                .radio_value(&mut f.codec, VideoCodec::H264, "H.264")
                .on_hover_text("Plays everywhere")
                .changed();
            *changed |= ui
                .radio_value(&mut f.codec, VideoCodec::Hevc, "HEVC (H.265)")
                .on_hover_text("About 30% smaller at the same quality; keeps 10-bit sources 10-bit")
                .changed();
        });
    });
    row(ui, "Encoder", None, |ui| {
        *changed |= encoder_combo(ui, "full-enc", &mut f.encoder, f.codec, set);
    });
    row(ui, "Quality", None, |ui| {
        *changed |= quality_ui(ui, "full-q", &mut f.quality);
    });
    row(ui, "Size", None, |ui| {
        ui.horizontal(|ui| {
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
        });
    });
    row(ui, "Frame rate", None, |ui| {
        let shown = f
            .fps
            .map_or("Keep original timing".to_string(), |r| format!("{r} fps"));
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
    row(ui, "Content", None, |ui| {
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

/// Disk space an export needs and has.
struct DiskSpace {
    /// Largest temporary space of one output.
    temp: u64,
    temp_free: Option<u64>,
    /// All outputs together.
    total: u64,
    out_free: Option<u64>,
}

impl App {
    fn disk_space(&self, p: &keepcut_core::plan::ExportPlan) -> DiskSpace {
        let temp_dir = self
            .config
            .temp_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let out_dir = p
            .outputs
            .first()
            .and_then(|o| o.path.parent())
            .map(Path::to_path_buf);
        DiskSpace {
            temp: p
                .outputs
                .iter()
                .map(|o| o.est_temp_bytes)
                .max()
                .unwrap_or(0),
            temp_free: fsutil::free_space(&temp_dir),
            total: p.outputs.iter().map(|o| o.est_bytes).sum(),
            out_free: out_dir.as_deref().and_then(fsutil::free_space),
        }
    }
}

/// Width of the label column in the export dialog's sections.
const LABEL_WIDTH: f32 = 110.0;

/// A titled group of export options: the title above a framed box.
fn section<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
    group_frame(ui, add)
}

/// A section that can be closed: the title (click it or the arrow) and a short summary
/// in the header; the options in the frame below when open.
fn collapsible_section(
    ui: &mut egui::Ui,
    id: &str,
    title: &str,
    default_open: bool,
    summary: impl FnOnce(&mut egui::Ui),
    add: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(6.0);
    let id = ui.make_persistent_id(id);
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    ui.horizontal(|ui| {
        state.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
        let title =
            ui.add(egui::Label::new(RichText::new(title).strong()).sense(egui::Sense::click()));
        if title.clicked() {
            state.toggle(ui);
        }
        summary(ui);
    });
    state.show_body_unindented(ui, |ui| {
        ui.add_space(2.0);
        group_frame(ui, add);
    });
}

/// The framed box of a section.
fn group_frame<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// One option in a section: its label in a column on the left, level with the first
/// line of the controls (not centred on them, which reads badly for tall groups).
fn row<R>(
    ui: &mut egui::Ui,
    label: &str,
    tip: Option<&str>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal_top(|ui| {
        let size = egui::vec2(LABEL_WIDTH, ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_size(size);
                let r = ui.label(label);
                if let Some(t) = tip {
                    r.on_hover_text(t);
                }
            },
        );
        ui.vertical(add).inner
    })
    .inner
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
