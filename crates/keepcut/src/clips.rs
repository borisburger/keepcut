//! Clip list (§8.2): order = output order, drag to reorder, enable checkbox,
//! durations and badges (re-encode reasons, read errors, audio tracks).

use eframe::egui::{self, RichText};
use keepcut_core::{edit, timefmt};
use uuid::Uuid;

use crate::app::App;

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

        let clips = self.project.clips.clone();
        let mut select = None;
        let mut toggle = None;
        let mut remove = None;
        let mut moved: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, c) in clips.iter().enumerate() {
                let selected = self.selected == Some(c.id);
                let m = self.media.get(&c.id);
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let item_id = egui::Id::unique(("clip", c.id));
                let row = ui.dnd_drag_source(item_id, i, |ui| {
                    let frame = egui::Frame::group(ui.style())
                        .fill(if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.5) } else { ui.visuals().faint_bg_color })
                        .inner_margin(6.0);
                    frame.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let mut en = c.enabled;
                            if ui.checkbox(&mut en, "").on_hover_text("Include in the export").changed() {
                                toggle = Some(c.id);
                            }
                            let text = if c.enabled { RichText::new(&name) } else { RichText::new(&name).weak().strikethrough() };
                            ui.label(text.strong()).on_hover_text(c.path.display().to_string());
                        });
                        ui.horizontal(|ui| match m {
                            Some(m) if m.info.is_some() => {
                                let info = m.info.as_ref().unwrap();
                                let kept = edit::kept_duration(c.segments.as_deref().unwrap_or(&[]));
                                ui.monospace(format!("{} → {}", timefmt::display(info.video_end()), timefmt::display(kept)));
                                if let Some(v) = &info.video {
                                    ui.weak(format!("{}×{}", v.width, v.height));
                                }
                                if info.audio.len() > 1 {
                                    ui.weak(format!("{} audio", info.audio.len())).on_hover_text("Audio tracks are mixed in the export");
                                }
                                if let Some((_, ok, reasons)) = verdicts.iter().find(|(id, _, _)| *id == c.id)
                                    && !ok
                                {
                                    ui.colored_label(ui.visuals().warn_fg_color, "re-encode")
                                        .on_hover_text(format!("Can't be copied into the joined file:\n{}", reasons.join("\n")));
                                }
                            }
                            Some(m) if m.error.is_some() => {
                                ui.colored_label(ui.visuals().error_fg_color, "can't read")
                                    .on_hover_text(m.error.clone().unwrap_or_default());
                            }
                            _ => {
                                ui.spinner();
                                ui.weak("reading…");
                            }
                        });
                    })
                    .response
                });
                let resp = row.response.interact(egui::Sense::click());
                if resp.clicked() {
                    select = Some(c.id);
                }
                resp.context_menu(|ui| {
                    if ui.button("Remove from list").clicked() {
                        remove = Some(c.id);
                        ui.close();
                    }
                    if ui.button("Show in folder").clicked() {
                        crate::export::reveal(&c.path);
                        ui.close();
                    }
                });
                // drop target: dropping a dragged clip on this row moves it here
                if let Some(from) = resp.dnd_release_payload::<usize>() {
                    moved = Some((*from, i));
                }
                if resp.dnd_hover_payload::<usize>().is_some() {
                    let r = resp.rect;
                    ui.painter().hline(r.x_range(), r.top() - 2.0, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
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
