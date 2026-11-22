//! Editor panel under the video: transport, segment commands, timeline and the
//! segment list (§8.1, §8.4). The list moves beside the video when it is narrow enough
//! to leave room (portrait video).

use eframe::egui;
use keepcut_core::{chapters, timefmt};

use crate::app::{App, Loaded};
use crate::icons::{self, Icon};

/// A shortcut shown on a button, in the monospace font: "I" in the UI font looks like "|".
fn key(k: &str) -> egui::RichText {
    egui::RichText::new(k).monospace()
}

impl App {
    pub fn editor(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let s = self.player.state.clone();
            if icons::button(ui, Icon::PrevKeyframe)
                .on_hover_text("Previous keyframe (Shift + ←)")
                .clicked()
            {
                self.keyframe_jump(false);
            }
            if icons::button(ui, Icon::PrevFrame)
                .on_hover_text("Previous frame (←)")
                .clicked()
            {
                self.step(false);
            }
            let (icon, tip) = if s.pause {
                (Icon::Play, "Play (Space)")
            } else {
                (Icon::Pause, "Pause (Space)")
            };
            if icons::button(ui, icon).on_hover_text(tip).clicked() {
                self.play_pause();
            }
            if icons::button(ui, Icon::NextFrame)
                .on_hover_text("Next frame (→)")
                .clicked()
            {
                self.step(true);
            }
            if icons::button(ui, Icon::NextKeyframe)
                .on_hover_text("Next keyframe (Shift + →)")
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
                let sz = icons::size(ui);
                if ui
                    .add(
                        egui::Button::new((icons::atom(Icon::MarkIn, sz), "Set start"))
                            .shortcut_text(key("I")),
                    )
                    .on_hover_text("Start the segment under the playhead here, or a new one")
                    .clicked()
                {
                    self.set_start();
                }
                if ui
                    .add(
                        egui::Button::new((icons::atom(Icon::MarkOut, sz), "Set end"))
                            .shortcut_text(key("O")),
                    )
                    .on_hover_text("End the segment under the playhead here, or a new one")
                    .clicked()
                {
                    self.set_end();
                }
                if ui
                    .add(egui::Button::new("Split").shortcut_text(key("S")))
                    .on_hover_text("Split the segment under the playhead")
                    .clicked()
                {
                    self.split();
                }
                if ui
                    .add_enabled(
                        self.selected_seg.is_some(),
                        egui::Button::new("Delete").shortcut_text(key("Del")),
                    )
                    .on_hover_text("Remove the selected segment (the footage becomes a gap)")
                    .clicked()
                {
                    self.delete_selected_segment();
                }
                ui.separator();
                let on = matches!(self.loaded, Loaded::Preview { clip: Some(_), .. });
                if ui
                    .add(egui::Button::selectable(on, "Cut preview").shortcut_text(key("P")))
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
                // only when it fits: in a narrow window it would run over the buttons
                let hint = egui::WidgetText::from(
                    egui::RichText::new(
                        "Ctrl + wheel: zoom · Shift + drag edge: snap to keyframes",
                    )
                    .weak(),
                )
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Body,
                );
                if hint.size().x <= ui.available_width() {
                    ui.label(hint);
                }
            });
        });
        ui.add_space(2.0);
        self.timeline(ui);
        // the list is under the timeline unless there's room beside the video
        // (App::segments_beside); its height is measured for that decision
        if self.segments_beside {
            self.editor_list_height = 0.0;
        } else {
            let top = ui.cursor().top();
            ui.add_space(2.0);
            self.segment_list(ui);
            self.editor_list_height = ui.cursor().top() - top;
        }
        ui.add_space(4.0);
    }

    /// The selected clip's segments as shown in the list.
    fn segment_rows(&self) -> Option<Vec<SegRow>> {
        let id = self.selected?;
        let info = self.info(id)?;
        let segs = self.segments(id);
        let clip_title = self
            .clip(id)
            .and_then(|c| c.chapter_title.clone())
            .unwrap_or_else(|| info.file_stem());
        Some(
            segs.iter()
                .enumerate()
                .map(|(i, s)| SegRow {
                    id: s.id,
                    start: s.start,
                    end: s.end,
                    head: if info.frames.is_key_at(s.start) {
                        Head::Copied
                    } else {
                        info.frames
                            .next_key_at_or_after(s.start)
                            .filter(|k| *k < s.end)
                            .map_or(Head::Reencoded, |k| Head::Reencodes(k - s.start))
                    },
                    title: s.chapter_title.clone().unwrap_or_default(),
                    hint: chapters::default_segment_title(&clip_title, i, segs.len()),
                })
                .collect(),
        )
    }

    /// The segment list under the timeline: a table.
    fn segment_list(&mut self, ui: &mut egui::Ui) {
        let Some(rows) = self.segment_rows() else {
            return;
        };
        let mut act = SegActions::default();
        egui::ScrollArea::vertical()
            .max_height(130.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("segments")
                    .striped(true)
                    .num_columns(7)
                    .spacing([16.0, 2.0])
                    .show(ui, |ui| {
                        for h in [
                            "#",
                            "Start",
                            "End",
                            "Duration",
                            "Start frame",
                            "Chapter title",
                            "",
                        ] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for (i, r) in rows.iter().enumerate() {
                            let selected = self.selected_seg == Some(r.id);
                            if ui
                                .selectable_label(selected, format!("{}", i + 1))
                                .clicked()
                            {
                                act.select = Some((r.id, r.start));
                            }
                            ui.monospace(timefmt::display(r.start));
                            ui.monospace(timefmt::display(r.end));
                            ui.monospace(timefmt::display(r.end - r.start));
                            r.head.label(ui);
                            r.title_edit(ui, 180.0, &mut act);
                            r.remove_button(ui, &mut act);
                            ui.end_row();
                        }
                    });
                if rows.is_empty() {
                    ui.weak(EMPTY_HINT);
                }
            });
        self.apply_segment_actions(act);
    }

    /// The segment list beside a narrow video: one card per segment, filling the height.
    pub fn segment_cards(&mut self, ui: &mut egui::Ui) {
        let Some(rows) = self.segment_rows() else {
            return;
        };
        let mut act = SegActions::default();
        let kept: f64 = rows.iter().map(|r| r.end - r.start).sum();
        ui.horizontal(|ui| {
            ui.strong("Segments");
            if !rows.is_empty() {
                ui.weak(format!("{} · {} kept", rows.len(), timefmt::display(kept)));
            }
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, r) in rows.iter().enumerate() {
                    let selected = self.selected_seg == Some(r.id);
                    let mut frame = egui::Frame::group(ui.style()).inner_margin(6.0);
                    if selected {
                        let sel = ui.visuals().selection.bg_fill;
                        frame = frame
                            .fill(sel.gamma_multiply(0.2))
                            .stroke(egui::Stroke::new(1.0, sel));
                    }
                    frame.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(selected, format!("{}", i + 1))
                                .clicked()
                            {
                                act.select = Some((r.id, r.start));
                            }
                            let times = egui::RichText::new(format!(
                                "{} – {}",
                                timefmt::display(r.start),
                                timefmt::display(r.end)
                            ))
                            .monospace();
                            if ui
                                .add(egui::Label::new(times).sense(egui::Sense::click()))
                                .on_hover_text("Go to the start of this segment")
                                .clicked()
                            {
                                act.select = Some((r.id, r.start));
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    r.remove_button(ui, &mut act);
                                },
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(timefmt::display(r.end - r.start)).monospace(),
                            )
                            .on_hover_text("Duration");
                            ui.weak("·");
                            r.head.label(ui);
                        });
                        r.title_edit(ui, f32::INFINITY, &mut act);
                    });
                    ui.add_space(4.0);
                }
                if rows.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(EMPTY_HINT).weak()).wrap());
                }
            });
        self.apply_segment_actions(act);
    }

    fn apply_segment_actions(&mut self, act: SegActions) {
        // inline title edit: changes apply as you type; one undo step when the field
        // loses focus (App::commit_title_edit)
        if act.title_focus && self.title_before.is_none() {
            self.title_before = Some(self.project.clips.clone());
        }
        if let Some((sid, text)) = act.title_change
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
        if let Some((sid, t)) = act.select {
            self.selected_seg = Some(sid);
            self.seek(t);
        }
        if let Some(sid) = act.delete {
            self.selected_seg = Some(sid);
            self.delete_selected_segment();
        }
    }
}

const EMPTY_HINT: &str =
    "Nothing kept from this clip. Use Set start (I) / Set end (O) to keep a part.";

/// One segment in the list.
struct SegRow {
    id: uuid::Uuid,
    start: f64,
    end: f64,
    head: Head,
    title: String,
    /// The default chapter title, shown when the title is empty.
    hint: String,
}

/// How the segment's first frames are exported.
enum Head {
    /// Starts on a keyframe: copied.
    Copied,
    /// Re-encoded up to the next keyframe, this many seconds in.
    Reencodes(f64),
    /// No keyframe inside: re-encoded as a whole.
    Reencoded,
}

impl Head {
    fn label(&self, ui: &mut egui::Ui) {
        let warn = ui.visuals().warn_fg_color;
        match self {
            Head::Copied => ui.label("keyframe: copied"),
            Head::Reencodes(h) => ui.colored_label(warn, format!("re-encodes {h:.1} s")),
            Head::Reencoded => ui.colored_label(warn, "re-encoded"),
        };
    }
}

impl SegRow {
    fn title_edit(&self, ui: &mut egui::Ui, width: f32, act: &mut SegActions) {
        let mut text = self.title.clone();
        let r = ui
            .add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text(self.hint.as_str())
                    .desired_width(width),
            )
            .on_hover_text(
                "Title of this segment's chapter (per-segment chapters). Empty: the default shown.",
            );
        act.title_focus |= r.gained_focus();
        if r.changed() {
            act.title_change = Some((self.id, text));
        }
    }

    fn remove_button(&self, ui: &mut egui::Ui, act: &mut SegActions) {
        if ui
            .small_button("Remove")
            .on_hover_text("Delete this segment")
            .clicked()
        {
            act.delete = Some(self.id);
        }
    }
}

/// What the user did in the segment list this frame.
#[derive(Default)]
struct SegActions {
    select: Option<(uuid::Uuid, f64)>,
    delete: Option<uuid::Uuid>,
    title_change: Option<(uuid::Uuid, String)>,
    title_focus: bool,
}
