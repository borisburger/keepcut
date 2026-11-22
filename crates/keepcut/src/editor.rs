//! Editor panel under the video: transport, segment commands, timeline and the
//! segment list (§8.1, §8.4).

use eframe::egui::{self, RichText};
use keepcut_core::{chapters, timefmt};

use crate::app::{App, Loaded};

impl App {
    pub fn editor(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let s = self.player.state.clone();
            if ui
                .button("|< Key")
                .on_hover_text("Previous keyframe (Shift+←)")
                .clicked()
            {
                self.keyframe_jump(false);
            }
            if ui
                .button("< Frame")
                .on_hover_text("Previous frame (←)")
                .clicked()
            {
                self.step(false);
            }
            let play = if s.pause { "Play" } else { "Pause" };
            if ui
                .button(RichText::new(play).strong())
                .on_hover_text("Play/pause (Space)")
                .clicked()
            {
                self.play_pause();
            }
            if ui
                .button("Frame >")
                .on_hover_text("Next frame (→)")
                .clicked()
            {
                self.step(true);
            }
            if ui
                .button("Key >|")
                .on_hover_text("Next keyframe (Shift+→)")
                .clicked()
            {
                self.keyframe_jump(true);
            }
            ui.add_space(8.0);
            let (t, d) = match &self.loaded {
                Loaded::Preview { .. } => (s.time_pos, s.duration),
                _ => (
                    self.source_time(),
                    self.selected
                        .and_then(|id| self.info(id))
                        .map_or(s.duration, |i| i.video_end()),
                ),
            };
            ui.monospace(format!("{} / {}", timefmt::display(t), timefmt::display(d)));
        });
        ui.horizontal(|ui| {
            let has = self.selected.is_some_and(|id| self.info(id).is_some());
            ui.add_enabled_ui(has, |ui| {
                if ui
                    .button("Set start  I")
                    .on_hover_text("Start the segment under the playhead here, or a new one")
                    .clicked()
                {
                    self.set_start();
                }
                if ui
                    .button("Set end  O")
                    .on_hover_text("End the segment under the playhead here, or a new one")
                    .clicked()
                {
                    self.set_end();
                }
                if ui
                    .button("Split  S")
                    .on_hover_text("Split the segment under the playhead")
                    .clicked()
                {
                    self.split();
                }
                if ui
                    .add_enabled(
                        self.selected_seg.is_some(),
                        egui::Button::new("Delete  Del"),
                    )
                    .on_hover_text("Remove the selected segment (the footage becomes a gap)")
                    .clicked()
                {
                    self.delete_selected_segment();
                }
                ui.separator();
                let on = matches!(self.loaded, Loaded::Preview { clip: Some(_), .. });
                if ui
                    .selectable_label(on, "Cut preview  P")
                    .on_hover_text("Play only the kept segments of this clip")
                    .clicked()
                {
                    self.toggle_cut_preview();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if (self.timeline.end > self.timeline.start)
                    && ui.small_button("Zoom to fit").clicked()
                {
                    self.timeline = Default::default();
                }
                ui.weak("Ctrl+wheel: zoom · Shift+drag edge: snap to keyframes");
            });
        });
        ui.add_space(2.0);
        self.timeline(ui);
        ui.add_space(2.0);
        self.segment_list(ui);
        ui.add_space(4.0);
    }

    fn segment_list(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected else { return };
        let Some(info) = self.info(id) else { return };
        let segs = self.segments(id).to_vec();
        let clip_title = self
            .clip(id)
            .and_then(|c| c.chapter_title.clone())
            .unwrap_or_else(|| info.file_stem());
        let mut select = None;
        let mut delete = None;
        let mut title_change: Option<(uuid::Uuid, String)> = None;
        let mut title_focus = false;
        egui::ScrollArea::vertical().max_height(130.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("segments").striped(true).num_columns(7).spacing([16.0, 2.0]).show(ui, |ui| {
                for h in ["#", "Start", "End", "Duration", "Start frame", "Chapter title", ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for (i, s) in segs.iter().enumerate() {
                    let selected = self.selected_seg == Some(s.id);
                    if ui.selectable_label(selected, format!("{}", i + 1)).clicked() {
                        select = Some((s.id, s.start));
                    }
                    ui.monospace(timefmt::display(s.start));
                    ui.monospace(timefmt::display(s.end));
                    ui.monospace(timefmt::display(s.duration()));
                    if info.frames.is_key_at(s.start) {
                        ui.label("keyframe: copied");
                    } else {
                        let head = info.frames.next_key_at_or_after(s.start).filter(|k| *k < s.end).map(|k| k - s.start);
                        match head {
                            Some(h) => ui.colored_label(ui.visuals().warn_fg_color, format!("re-encodes {h:.1} s")),
                            None => ui.colored_label(ui.visuals().warn_fg_color, "re-encoded"),
                        };
                    }
                    let mut text = s.chapter_title.clone().unwrap_or_default();
                    let hint = chapters::default_segment_title(&clip_title, i, segs.len());
                    let r = ui
                        .add(egui::TextEdit::singleline(&mut text).hint_text(hint).desired_width(180.0))
                        .on_hover_text("Title of this segment's chapter (per-segment chapters). Empty: the default shown.");
                    title_focus |= r.gained_focus();
                    if r.changed() {
                        title_change = Some((s.id, text));
                    }
                    if ui.small_button("Remove").on_hover_text("Delete this segment").clicked() {
                        delete = Some(s.id);
                    }
                    ui.end_row();
                }
            });
            if segs.is_empty() {
                ui.weak("Nothing kept from this clip. Use Set start (I) / Set end (O) to keep a part.");
            }
        });
        // inline title edit: changes apply as you type; one undo step when the field
        // loses focus (App::commit_title_edit)
        if title_focus && self.title_before.is_none() {
            self.title_before = Some(self.project.clips.clone());
        }
        if let Some((sid, text)) = title_change
            && let Some(seg) = self
                .project
                .clips
                .iter_mut()
                .filter_map(|c| c.segments.as_mut())
                .flatten()
                .find(|s| s.id == sid)
        {
            // stored as typed (trimming here would eat the space just typed);
            // the planner trims
            seg.chapter_title = (!text.trim().is_empty()).then_some(text);
            self.mark_plan_dirty();
        }
        if let Some((sid, t)) = select {
            self.selected_seg = Some(sid);
            self.seek(t);
        }
        if let Some(sid) = delete {
            self.selected_seg = Some(sid);
            self.delete_selected_segment();
        }
    }
}
