//! Soundtrack: an audio file that replaces the clips' sound in the joined output and in
//! "Preview all". Set in a row under the clip list; stored in the project.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use keepcut_core::model::{Loudness, OutputMode, Soundtrack};
use keepcut_core::timefmt;

use crate::app::{App, ProbeState};

/// File types offered in the dialog (any file ffmpeg reads with an audio stream works).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "m4a", "mp3", "aac", "wav", "flac", "ogg", "opus", "wma", "aiff", "aif",
];

/// The start offset as typed back to the user: `m:ss`, `h:mm:ss`, with decimals if any.
pub fn format_offset(t: f64) -> String {
    if (t - t.round()).abs() < 0.0005 {
        timefmt::youtube(t.round(), t >= 3600.0)
    } else {
        timefmt::display(t)
    }
}

/// Seconds of the file after `offset`.
fn remaining(info: &keepcut_core::probe::MediaInfo, offset: f64) -> f64 {
    (info.duration - offset).max(0.0)
}

impl App {
    /// The soundtrack's probe state.
    pub fn soundtrack_state(&self) -> Option<&ProbeState> {
        self.probes.get(&self.project.soundtrack.as_ref()?.path)
    }

    /// Sets (or replaces) the soundtrack. Loudness normalization is turned off: the music
    /// usually has its own level ("whole video" can still be chosen).
    pub fn set_soundtrack(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let offset = self.project.soundtrack.as_ref().map_or(0.0, |s| s.offset);
        self.project.soundtrack = Some(Soundtrack {
            path: path.clone(),
            offset,
        });
        self.project.export.loudness = Loudness::Off;
        self.probe_soundtrack();
        self.soundtrack_edit = None;
        self.mark_plan_dirty();
        self.reload_preview_all();
    }

    pub fn remove_soundtrack(&mut self) {
        self.project.soundtrack = None;
        self.soundtrack_edit = None;
        self.mark_plan_dirty();
        self.reload_preview_all();
    }

    pub fn set_soundtrack_offset(&mut self, offset: f64) {
        if let Some(st) = self.project.soundtrack.as_mut()
            && (st.offset - offset).abs() > 1e-9
        {
            st.offset = offset.max(0.0);
            self.reload_preview_all();
        }
    }

    /// Reads the soundtrack's file (duration, audio streams), or marks it missing.
    pub fn probe_soundtrack(&mut self) {
        let Some(p) = self.project.soundtrack.as_ref().map(|s| s.path.clone()) else {
            return;
        };
        if !p.is_file() {
            self.probes.insert(p, ProbeState::Missing);
        } else if !matches!(
            self.probes.get(&p),
            Some(ProbeState::Ready(_) | ProbeState::Pending)
        ) {
            self.probes.insert(p.clone(), ProbeState::Pending);
            self.probe.enqueue(p);
        }
    }

    fn pick_soundtrack(&self, title: &str) -> Option<PathBuf> {
        let mut dlg = rfd::FileDialog::new()
            .set_title(title)
            .add_filter(
                "Audio",
                &crate::dialogs::filter_extensions(AUDIO_EXTENSIONS),
            )
            .add_filter(
                "Video (its sound)",
                &crate::dialogs::filter_extensions(crate::dialogs::VIDEO_EXTENSIONS),
            );
        let near = self
            .project
            .soundtrack
            .as_ref()
            .and_then(|s| {
                s.path
                    .parent()
                    .filter(|d| d.is_dir())
                    .map(Path::to_path_buf)
            })
            .or_else(|| {
                self.proj
                    .path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            });
        if let Some(d) = near {
            dlg = dlg.set_directory(d);
        }
        dlg.pick_file()
    }

    /// If "Preview all" is playing, starts it again so the soundtrack change is heard.
    fn reload_preview_all(&mut self) {
        if matches!(self.loaded, crate::app::Loaded::Preview { clip: None, .. }) {
            self.restart_preview_all();
        }
    }

    /// mpv URL of the soundtrack from its offset (mpv's EDL can start a file later), if
    /// the file is there.
    pub fn soundtrack_url(&self) -> Option<String> {
        let st = self.project.soundtrack.as_ref()?;
        if !matches!(self.soundtrack_state(), Some(ProbeState::Ready(i)) if !i.audio.is_empty()) {
            return None;
        }
        let p = st.path.to_string_lossy();
        Some(if st.offset > 0.0 {
            format!("edl://%{}%{},start={:.6}", p.len(), p, st.offset)
        } else {
            p.into_owned()
        })
    }

    /// The row under the clip list.
    pub fn soundtrack_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong("Soundtrack");
            if self.project.soundtrack.is_none() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Choose…")
                        .on_hover_text("An audio file that plays instead of the clips' sound")
                        .clicked()
                        && let Some(p) = self.pick_soundtrack("Choose a soundtrack")
                    {
                        self.set_soundtrack(p);
                    }
                });
            }
        });
        let Some(st) = self.project.soundtrack.clone() else {
            ui.weak("None: the clips keep their own sound.");
            ui.add_space(4.0);
            return;
        };
        let name = st
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Remove")
                    .on_hover_text("Use the clips' own sound again")
                    .clicked()
                {
                    self.remove_soundtrack();
                }
                if ui.small_button("Change…").clicked()
                    && let Some(p) = self.pick_soundtrack("Choose a soundtrack")
                {
                    self.set_soundtrack(p);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(format!("♪ {name}")).strong()).truncate(),
                    )
                    .on_hover_text(st.path.display().to_string());
                });
            });
        });
        if self.project.soundtrack.is_none() {
            return;
        }

        // start offset: shown formatted, edited as text, applied on Enter / leaving the field
        ui.horizontal(|ui| {
            ui.label("Start at");
            let mut text = self.soundtrack_edit.clone().unwrap_or_else(|| {
                if st.offset > 0.0 {
                    format_offset(st.offset)
                } else {
                    String::new()
                }
            });
            let parsed = if text.trim().is_empty() {
                Some(0.0)
            } else {
                timefmt::parse(&text)
            };
            let mut edit = egui::TextEdit::singleline(&mut text)
                .hint_text("0:00")
                .desired_width(80.0);
            if parsed.is_none() {
                edit = edit.text_color(ui.visuals().error_fg_color);
            }
            let r = ui.add(edit).on_hover_text(
                "Where in the audio file to start, as mm:ss (or h:mm:ss, seconds with decimals). \
                 Empty: from the beginning.",
            );
            if r.changed() || r.has_focus() {
                self.soundtrack_edit = Some(text.clone());
            }
            if r.lost_focus() {
                if let Some(t) = parsed {
                    self.set_soundtrack_offset(t);
                }
                self.soundtrack_edit = None;
            }
            if let Some(ProbeState::Ready(info)) = self.soundtrack_state() {
                ui.weak(format!("of {}", timefmt::display(info.duration)));
            }
        });

        // what will happen, and what is wrong
        let joined = self.project.export.output_mode == OutputMode::Joined;
        let output_len = match &self.plan {
            Some(Ok(p)) if joined => p.outputs.first().map(|o| o.duration()),
            _ => None,
        };
        let warn = ui.visuals().warn_fg_color;
        let error = ui.visuals().error_fg_color;
        match self.soundtrack_state().cloned() {
            Some(ProbeState::Missing) => {
                ui.horizontal(|ui| {
                    ui.colored_label(error, "missing file");
                    if ui.small_button("Locate…").clicked()
                        && let Some(p) = self.pick_soundtrack(&format!("Locate {name}"))
                    {
                        self.set_soundtrack(p);
                    }
                });
            }
            Some(ProbeState::Failed(e)) => {
                ui.colored_label(error, "can't read this file")
                    .on_hover_text(e);
            }
            Some(ProbeState::Ready(info)) if info.audio.is_empty() => {
                ui.colored_label(error, "this file has no audio");
            }
            Some(ProbeState::Ready(info)) if remaining(&info, st.offset) <= 0.0 => {
                ui.colored_label(error, "the start is after the end of the file");
            }
            Some(ProbeState::Ready(info)) => {
                let left = remaining(&info, st.offset);
                match output_len {
                    Some(len) if left + 0.05 < len => {
                        ui.colored_label(
                            warn,
                            format!(
                                "ends at {} of the {} output; silence after",
                                timefmt::display(left),
                                timefmt::display(len)
                            ),
                        );
                    }
                    Some(_) => {
                        ui.weak("Replaces the clips' sound; cut at the end of the video.");
                    }
                    None => {}
                }
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("reading…");
                });
            }
        }
        if !joined {
            ui.colored_label(
                warn,
                "Used only for a single joined file: separate files keep their own sound.",
            );
        }
        ui.add_space(4.0);
    }

    /// Whether the soundtrack replaces the audio of the export as set up now.
    pub fn soundtrack_active(&self) -> bool {
        self.project.soundtrack.is_some() && self.project.export.output_mode == OutputMode::Joined
    }

    /// Short name of the soundtrack file.
    pub fn soundtrack_name(&self) -> Option<String> {
        let st = self.project.soundtrack.as_ref()?;
        Some(
            st.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_read_back_as_typed() {
        assert_eq!(format_offset(30.0), "0:30");
        assert_eq!(format_offset(95.0), "1:35");
        assert_eq!(format_offset(3725.0), "1:02:05");
        assert_eq!(format_offset(65.5), "1:05.500");
        for t in [30.0, 95.0, 3725.0, 65.5] {
            assert_eq!(timefmt::parse(&format_offset(t)), Some(t));
        }
    }
}
