//! Clip list (§8.2): order = output order, drag to reorder, enable checkbox,
//! durations and badges (re-encode reasons, read errors, audio tracks).

use eframe::egui::{self, RichText};
use keepcut_core::{edit, timefmt};
use uuid::Uuid;

use crate::app::{App, ProbeState};

impl App {
    pub fn clip_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Clips");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Sort by recording time")
                    .on_hover_text("Creation time tag, Bandicam file name, then file date")
                    .clicked()
                {
                    self.sort_by_recording_time();
                }
            });
        });
        ui.separator();

        // verdicts from the plan, by clip id
        let ids = self.planned_clip_ids();
        let verdicts: Vec<(Uuid, bool, Vec<String>)> = match &self.plan {
            Some(Ok(p)) => p
                .clips
                .iter()
                .filter_map(|v| {
                    ids.get(v.clip_idx)
                        .map(|id| (*id, v.copyable || v.reasons.is_empty(), v.reasons.clone()))
                })
                .collect(),
            _ => Vec::new(),
        };

        // the soundtrack row sits at the bottom; the clips scroll above it
        egui::Panel::bottom("soundtrack").show(ui, |ui| self.soundtrack_ui(ui));

        let clips = self.project.clips.clone();
        let mut select = None;
        let mut toggle = None;
        let mut remove = None;
        let mut title = None;
        let mut locate = None;
        let mut moved: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, c) in clips.iter().enumerate() {
                let selected = self.selected == Some(c.id);
                let probe = self.probe_state(c.id).cloned();
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let item_id = egui::Id::unique(("clip", c.id));
                // The checkbox sits outside the draggable card: inside it, the
                // card's drag/click sense would win and the checkbox couldn't be clicked.
                let row = ui.horizontal(|ui| {
                    let mut en = c.enabled;
                    if ui.checkbox(&mut en, "").on_hover_text("Include in the export").changed() {
                        toggle = Some(c.id);
                    }
                    ui.dnd_drag_source(item_id, i, |ui| {
                        let fill = if selected {
                            ui.visuals().selection.bg_fill.gamma_multiply(0.5)
                        } else {
                            ui.visuals().faint_bg_color
                        };
                        egui::Frame::group(ui.style()).fill(fill).inner_margin(6.0).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                // thumbnail (a frame at 10%), or a placeholder while it is made
                                let size = egui::vec2(96.0, 54.0);
                                match self.thumbs.get(&c.path) {
                                    Some(Some(tex)) => {
                                        ui.add(egui::Image::new(tex).fit_to_exact_size(size).corner_radius(2.0));
                                    }
                                    _ => {
                                        let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                                        ui.painter().rect_filled(r, 2.0, ui.visuals().extreme_bg_color);
                                    }
                                }
                                ui.vertical(|ui| {
                                    let text = if c.enabled { RichText::new(&name) } else { RichText::new(&name).weak().strikethrough() };
                                    ui.add(egui::Label::new(text.strong()).truncate()).on_hover_text(c.path.display().to_string());
                                    if let Some(t) = &c.chapter_title {
                                        ui.weak(format!("chapter: {t}"));
                                    }
                                    ui.horizontal_wrapped(|ui| match &probe {
                                        Some(ProbeState::Ready(info)) => {
                                            let kept = edit::kept_duration(c.segments.as_deref().unwrap_or(&[]));
                                            ui.add(egui::Label::new(RichText::new(format!("{} → {}", timefmt::display(info.video_end()), timefmt::display(kept))).monospace()).extend());
                                            if let Some(v) = &info.video {
                                                ui.weak(format!("{}×{}", v.width, v.height));
                                            }
                                            if info.audio.len() > 1 {
                                                ui.weak(format!("{} audio", info.audio.len()))
                                                    .on_hover_text("Audio tracks are mixed in the export");
                                            }
                                            if let Some((_, ok, reasons)) = verdicts.iter().find(|(id, _, _)| *id == c.id)
                                                && !ok
                                            {
                                                ui.colored_label(ui.visuals().warn_fg_color, "re-encode").on_hover_text(format!(
                                                    "Can't be copied into the joined file:\n{}",
                                                    reasons.join("\n")
                                                ));
                                            }
                                            if self.proj.changed_files.contains(&c.path) {
                                                ui.colored_label(ui.visuals().warn_fg_color, "changed").on_hover_text(
                                                    "The file changed since the project was saved: check the segments",
                                                );
                                            }
                                        }
                                        Some(ProbeState::Failed(e)) => {
                                            ui.colored_label(ui.visuals().error_fg_color, "can't read").on_hover_text(e.clone());
                                        }
                                        Some(ProbeState::Missing) => {
                                            ui.colored_label(ui.visuals().error_fg_color, "missing file").on_hover_text(format!(
                                                "{} was not found.\nRight-click: Locate… or Remove",
                                                c.path.display()
                                            ));
                                        }
                                        _ => {
                                            ui.spinner();
                                            ui.weak("reading…");
                                        }
                                    });
                                });
                            });
                        })
                        .response
                    })
                    .response
                });
                let resp = row.inner.interact(egui::Sense::click());
                if resp.clicked() {
                    select = Some(c.id);
                }
                resp.context_menu(|ui| {
                    if matches!(probe, Some(ProbeState::Missing)) && ui.button("Locate…").clicked() {
                        locate = Some(c.id);
                        ui.close();
                    }
                    if ui.button("Remove from list").clicked() {
                        remove = Some(c.id);
                        ui.close();
                    }
                    if ui.button("Show in folder").clicked() {
                        crate::export::reveal(&c.path);
                        ui.close();
                    }
                    if ui
                        .button("Chapter title…")
                        .on_hover_text("Title of this clip's chapter (per-clip chapters), also the base of its segments' titles")
                        .clicked()
                    {
                        title = Some(c.id);
                        ui.close();
                    }
                });
                // drop target: dropping a dragged clip on this row moves it to this position
                if let Some(from) = resp.dnd_release_payload::<usize>() {
                    moved = Some((*from, i));
                }
                if let Some(from) = resp.dnd_hover_payload::<usize>() {
                    // moving down lands after this row, moving up before it
                    let r = resp.rect;
                    let y = if *from < i { r.bottom() + 2.0 } else { r.top() - 2.0 };
                    ui.painter().hline(r.x_range(), y, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
                }
                ui.add_space(2.0);
            }
            if clips.is_empty() {
                ui.add_space(20.0);
                ui.weak("No clips yet.\nClick \"+ Add clips…\" or drop video files onto the window.");
            }
        });

        if let Some(id) = select {
            self.select_clip(Some(id));
        }
        if let Some(id) = toggle {
            self.edit(|clips| {
                let Some(c) = clips.iter_mut().find(|c| c.id == id) else {
                    return false;
                };
                c.enabled = !c.enabled;
                true
            });
        }
        if let Some(id) = title
            && let Some(c) = self.clip(id)
        {
            self.title_dialog = Some(TitleDialog {
                clip: id,
                text: c.chapter_title.clone().unwrap_or_default(),
                focus: true,
            });
        }
        if let Some(id) = locate {
            self.locate(id);
        }
        if let Some(id) = remove {
            self.remove_clip(id);
        }
        if let Some((from, to)) = moved
            && from != to
        {
            self.edit(|clips| {
                if from >= clips.len() || to >= clips.len() {
                    return false;
                }
                let c = clips.remove(from);
                clips.insert(to, c);
                true
            });
        }
    }
}

/// The "Chapter title" dialog of a clip.
pub struct TitleDialog {
    pub clip: Uuid,
    pub text: String,
    /// Focus the text field on the first frame.
    pub focus: bool,
}

impl App {
    pub fn title_window(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.title_dialog else {
            return;
        };
        let stem = self
            .project
            .clips
            .iter()
            .find(|c| c.id == d.clip)
            .and_then(|c| c.path.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut ok = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::unique("chapter-title")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Chapter title");
            ui.label("Used for this clip's chapter, and as the base of its segments' titles (\"Title – 2\"). Empty: the file name.");
            let r = ui.add(egui::TextEdit::singleline(&mut d.text).hint_text(&stem).desired_width(f32::INFINITY));
            if d.focus {
                r.request_focus();
                d.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                ok = true;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ok |= ui.button("OK").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if ok {
            self.apply_title_dialog();
        } else if cancel || modal.should_close() {
            self.title_dialog = None;
        }
    }

    /// "OK" in the Chapter title dialog (one undo step).
    pub fn apply_title_dialog(&mut self) {
        let Some(d) = self.title_dialog.take() else {
            return;
        };
        let text = d.text.trim().to_string();
        let new = (!text.is_empty()).then_some(text);
        self.edit(|clips| match clips.iter_mut().find(|c| c.id == d.clip) {
            Some(c) if c.chapter_title != new => {
                c.chapter_title = new;
                true
            }
            _ => false,
        });
    }

    /// Ends an inline segment-title edit: one undo step for the whole edit.
    /// Called every frame; does nothing while the text field still has focus.
    pub fn commit_title_edit(&mut self, ctx: &egui::Context) {
        if self.title_before.is_none() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let before = self.title_before.take().unwrap();
        let same = before.len() == self.project.clips.len()
            && before
                .iter()
                .zip(&self.project.clips)
                .all(|(a, b)| a.id == b.id && a.segments == b.segments);
        if !same {
            self.history.push(before);
        }
    }
}
