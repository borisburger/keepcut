//! Settings dialog (§8.7): ffmpeg location, encoder re-detection, temp folder,
//! default loudness target, full decode check, theme and video rendering.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use keepcut_core::encoders;
use keepcut_core::ffmpeg::Tools;
use keepcut_core::fsutil;

use crate::app::{App, ProbeState};
use crate::config::{HwDecoding, RenderPath, Theme};

#[derive(Default)]
pub struct SettingsUi {
    open: bool,
    /// `ffmpeg -version` of the tools in use (checked when the dialog opens).
    version: Option<Result<String, String>>,
}

/// ffmpeg/ffprobe from the configured folder, else bundled/PATH.
pub fn locate_tools(dir: Option<&std::path::Path>) -> Result<Tools, String> {
    match dir {
        Some(d) => Tools::in_dir(d).map_err(|e| format!("{e} in {}", d.display())),
        None => Tools::locate().map_err(|e| e.to_string()),
    }
}

impl App {
    pub fn settings_open(&self) -> bool {
        self.settings.open
    }

    pub fn close_settings(&mut self) {
        self.settings.open = false;
    }

    pub fn open_settings(&mut self) {
        self.settings.open = true;
        self.settings.version = self.tools.as_ref().map(Tools::version);
    }

    /// Uses ffmpeg from `dir` (`None`: automatic). Unreadable clips are read again.
    fn set_ffmpeg_dir(&mut self, dir: Option<PathBuf>) {
        match locate_tools(dir.as_deref()) {
            Ok(t) => {
                self.config.ffmpeg_dir = dir;
                self.settings.version = Some(t.version());
                self.tools = Some(t);
                self.probe.set_tools(self.tools.clone());
                self.assets.set_tools(self.tools.clone());
                let failed: Vec<PathBuf> = self
                    .probes
                    .iter()
                    .filter(|(_, s)| matches!(s, ProbeState::Failed(_)))
                    .map(|(p, _)| p.clone())
                    .collect();
                for p in failed {
                    self.probes.insert(p.clone(), ProbeState::Pending);
                    self.probe.enqueue(p);
                }
                self.redetect_encoders();
            }
            Err(e) => self.settings.version = Some(Err(e)),
        }
    }

    pub fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings.open {
            return;
        }
        let mut close = false;
        let mut new_dir: Option<Option<PathBuf>> = None;
        let mut redetect = false;
        let modal = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.heading("Settings");
            ui.add_space(6.0);
            egui::Grid::new("settings-grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label("ffmpeg");
                ui.vertical(|ui| {
                    match &self.tools {
                        Some(t) => ui.label(t.ffmpeg.display().to_string()),
                        None => ui.colored_label(ui.visuals().error_fg_color, "not found"),
                    };
                    match &self.settings.version {
                        Some(Ok(v)) => {
                            ui.weak(v);
                        }
                        Some(Err(e)) => {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                        None => {}
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button("Choose folder…")
                            .on_hover_text("A folder that contains both ffmpeg and ffprobe")
                            .clicked()
                            && let Some(d) = rfd::FileDialog::new().pick_folder()
                        {
                            new_dir = Some(Some(d));
                        }
                        if ui
                            .add_enabled(self.config.ffmpeg_dir.is_some(), egui::Button::new("Automatic"))
                            .on_hover_text("The ffmpeg next to Keepcut, otherwise the one on PATH")
                            .clicked()
                        {
                            new_dir = Some(None);
                        }
                    });
                });
                ui.end_row();

                ui.label("Encoders");
                ui.vertical(|ui| {
                    if self.encoders_detected() {
                        let hw: Vec<&str> = self
                            .encoders
                            .working
                            .iter()
                            .filter(|e| !encoders::is_software(e))
                            .map(String::as_str)
                            .collect();
                        ui.label(if hw.is_empty() {
                            "Software only (no working hardware encoder)".to_string()
                        } else {
                            format!("Hardware: {}", hw.join(", "))
                        });
                        redetect |= ui
                            .button("Detect again")
                            .on_hover_text("After a driver update or plugging in another GPU")
                            .clicked();
                    } else {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Testing the hardware encoders…");
                        });
                    }
                });
                ui.end_row();

                ui.label("Temporary files");
                ui.vertical(|ui| {
                    let sys = std::env::temp_dir();
                    let mut custom = self.config.temp_dir.is_some();
                    let free = |p: &std::path::Path| fsutil::free_space(p).map_or("?".into(), fsutil::human_bytes);
                    if ui.radio_value(&mut custom, false, format!("System temp folder ({} free)", free(&sys))).clicked() {
                        self.config.temp_dir = None;
                    }
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut custom, true, "Folder:");
                        match &self.config.temp_dir {
                            Some(d) => ui.label(format!("{} ({} free)", d.display(), free(d))),
                            None => ui.weak("(none)"),
                        };
                        if ui.button("Browse…").clicked()
                            && let Some(d) = rfd::FileDialog::new().pick_folder()
                        {
                            self.config.temp_dir = Some(d);
                        }
                    });
                    ui.weak("Copied parts of 4K camera footage can need several GB during an export.");
                });
                ui.end_row();

                ui.label("Volume target");
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::DragValue::new(&mut self.config.loudness_target).range(-30.0..=-5.0).speed(0.1).fixed_decimals(1).suffix(" LUFS"))
                        .changed()
                    {
                        self.project.export.loudness_target = self.config.loudness_target;
                        self.mark_plan_dirty();
                    }
                    ui.weak("default level for volume normalization (YouTube: −14)");
                });
                ui.end_row();

                ui.label("Verification");
                ui.checkbox(&mut self.config.full_decode_check, "Decode the whole output after each export (slow)")
                    .on_hover_text("Normally only the seams are decoded; durations, frame counts and A/V sync are always checked");
                ui.end_row();

                ui.label("Theme");
                ui.horizontal(|ui| {
                    let before = self.config.theme;
                    ui.radio_value(&mut self.config.theme, Theme::System, "System");
                    ui.radio_value(&mut self.config.theme, Theme::Light, "Light");
                    ui.radio_value(&mut self.config.theme, Theme::Dark, "Dark");
                    if self.config.theme != before {
                        ui.ctx().set_theme(self.config.theme.preference());
                    }
                });
                ui.end_row();

                ui.label("Video display");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.config.render, RenderPath::Auto, "OpenGL (falls back to software)");
                        ui.radio_value(&mut self.config.render, RenderPath::Software, "Software");
                    });
                    ui.weak("Takes effect after a restart. Software is slower; use it if the video stays black.");
                });
                ui.end_row();

                ui.label("Hardware decoding");
                ui.vertical(|ui| {
                    let before = self.config.hw_decoding;
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.config.hw_decoding, HwDecoding::Auto, "Automatic")
                            .on_hover_text("The GPU decodes the preview where mpv considers it safe");
                        ui.radio_value(&mut self.config.hw_decoding, HwDecoding::Off, "Off (CPU)")
                            .on_hover_text("The CPU decodes the preview; exports are not affected");
                    });
                    if self.config.hw_decoding != before {
                        self.apply_hw_decoding();
                    }
                    ui.weak("Turn off if the preview shows black lines or blocks. Applies at once.");
                });
                ui.end_row();

                ui.label("Files");
                ui.horizontal(|ui| {
                    if let Some(d) = crate::config::dirs() {
                        if ui.button("Open log folder").clicked() {
                            crate::export::open_with_default_app(&d.data_dir().join("logs"));
                        }
                        if ui.button("Open settings folder").clicked() {
                            crate::export::open_with_default_app(d.config_dir());
                        }
                    }
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            if ui.button(RichText::new("Close").strong()).clicked() {
                close = true;
            }
        });
        if let Some(d) = new_dir {
            self.set_ffmpeg_dir(d);
        }
        if redetect {
            self.redetect_encoders();
        }
        if close || modal.should_close() {
            self.settings.open = false;
            if self.script.is_none() {
                self.config.save();
            }
        }
    }
}
