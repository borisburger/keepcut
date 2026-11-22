//! Timeline widget (§8.3): kept segments, keyframe ticks, start-edge colours,
//! playhead; click/drag to seek, drag edges to trim, Ctrl+wheel to zoom.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use keepcut_core::{assets, edit};
use keepcut_core::probe::FrameIndex;
use keepcut_core::timefmt;

use crate::app::{App, Drag, Edge, Loaded};

/// Visible time range; `end <= start` means "whole clip".
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineView {
    pub start: f64,
    pub end: f64,
}

const EDGE_GRAB_PX: f32 = 6.0;
const KEEP: Color32 = Color32::from_rgb(52, 120, 200);
const KEEP_SELECTED: Color32 = Color32::from_rgb(90, 160, 240);
const ON_KEY: Color32 = Color32::from_rgb(70, 200, 90);
const HEAD_REENCODE: Color32 = Color32::from_rgb(240, 170, 40);
const WAVE_KEPT: Color32 = Color32::from_rgb(110, 175, 235);
const WAVE_LANE: f32 = 34.0;

impl App {
    pub fn timeline(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected else {
            ui.allocate_space(vec2(ui.available_width(), 64.0));
            return;
        };
        let Some(info) = self.info(id) else {
            ui.horizontal(|ui| match self.probe_error(id) {
                Some(e) => {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("This file can't be read: {e}"),
                    );
                }
                None => {
                    ui.spinner();
                    ui.label("reading clip…");
                }
            });
            ui.allocate_space(vec2(ui.available_width(), 40.0));
            return;
        };
        let fi: &FrameIndex = &info.frames;
        let dur = info.video_end().max(1e-3);
        let (v0, v1) = if self.timeline.end > self.timeline.start {
            (self.timeline.start, self.timeline.end)
        } else {
            (0.0, dur)
        };

        // waveform lane under the bar (clips with audio, §8.3)
        let lane_h = if info.audio.is_empty() { 0.0 } else { WAVE_LANE };
        let (rect, resp) = ui.allocate_exact_size(
            vec2(ui.available_width(), 64.0 + lane_h),
            Sense::click_and_drag(),
        );
        let bar = Rect::from_min_max(
            rect.min + vec2(0.0, 14.0),
            rect.max - vec2(0.0, 12.0 + lane_h),
        );
        let w = bar.width().max(1.0);
        let x_of = |t: f64| bar.left() + ((t - v0) / (v1 - v0)) as f32 * w;
        let t_of = |x: f32| v0 + ((x - bar.left()) / w).clamp(0.0, 1.0) as f64 * (v1 - v0);
        let painter = ui.painter_at(rect);
        let vis = ui.visuals().clone();

        // background (gaps = removed footage, dimmed)
        painter.rect_filled(bar, 2.0, vis.extreme_bg_color);

        // kept segments
        let segs = self.segments(id).to_vec();
        for s in &segs {
            let r = Rect::from_x_y_ranges(
                x_of(s.start).max(bar.left())..=x_of(s.end).min(bar.right()),
                bar.y_range(),
            );
            if r.width() <= 0.0 {
                continue;
            }
            let selected = self.selected_seg == Some(s.id);
            painter.rect_filled(r, 2.0, if selected { KEEP_SELECTED } else { KEEP });
            if selected {
                painter.rect_stroke(
                    r,
                    2.0,
                    Stroke::new(2.0, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
            // start edge: green = on a keyframe (pure copy), amber = head re-encoded
            let col = if fi.is_key_at(s.start) {
                ON_KEY
            } else {
                HEAD_REENCODE
            };
            let x = x_of(s.start);
            if x >= bar.left() - 1.0 && x <= bar.right() + 1.0 {
                painter.line_segment(
                    [pos2(x, bar.top()), pos2(x, bar.bottom())],
                    Stroke::new(3.0, col),
                );
            }
            let xe = x_of(s.end);
            if xe >= bar.left() - 1.0 && xe <= bar.right() + 1.0 {
                painter.line_segment(
                    [pos2(xe, bar.top()), pos2(xe, bar.bottom())],
                    Stroke::new(2.0, Color32::WHITE),
                );
            }
        }

        // keyframe ticks, only when ≥ 3 px apart
        let mut last_x = f32::MIN;
        let first = fi.idx_at_or_after(v0);
        for f in fi.frames[first..].iter().take_while(|f| f.t <= v1) {
            if !f.key {
                continue;
            }
            let x = x_of(f.t);
            if x - last_x >= 3.0 {
                painter.line_segment(
                    [pos2(x, bar.bottom()), pos2(x, bar.bottom() + 6.0)],
                    Stroke::new(1.0, vis.warn_fg_color),
                );
                last_x = x;
            }
        }

        // waveform: peak per pixel column, brighter inside kept segments
        if lane_h > 0.0 {
            let lane = Rect::from_x_y_ranges(
                bar.x_range(),
                (bar.bottom() + 8.0)..=(bar.bottom() + lane_h - 2.0),
            );
            painter.rect_filled(lane, 2.0, vis.extreme_bg_color.gamma_multiply(0.6));
            match self.waves.get(&info.path) {
                Some(Some(peaks)) if !peaks.is_empty() => {
                    let mid = lane.center().y;
                    let half = lane.height() / 2.0 - 1.0;
                    let mut x = lane.left();
                    while x < lane.right() {
                        let (t0, t1) = (t_of(x), t_of(x + 1.0));
                        let h = assets::wave_height(peaks, t0, t1) * half;
                        if h > 0.3 {
                            let kept = edit::segment_at(&segs, (t0 + t1) / 2.0).is_some();
                            let col = if kept { WAVE_KEPT } else { vis.weak_text_color() };
                            painter.line_segment(
                                [pos2(x + 0.5, mid - h), pos2(x + 0.5, mid + h)],
                                Stroke::new(1.0, col),
                            );
                        }
                        x += 1.0;
                    }
                }
                Some(Some(_)) => {}
                Some(None) => {
                    painter.text(
                        lane.left_center() + vec2(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        "waveform not available",
                        egui::FontId::proportional(11.0),
                        vis.weak_text_color(),
                    );
                }
                None => {
                    painter.text(
                        lane.left_center() + vec2(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        "reading audio…",
                        egui::FontId::proportional(11.0),
                        vis.weak_text_color(),
                    );
                }
            }
        }

        // playhead (in a cut preview of this clip, mapped back to source time)
        let playhead = match &self.loaded {
            Loaded::Clip(c) if *c == id => Some(self.player.state.time_pos),
            Loaded::Preview { .. } => self
                .preview_to_source(self.player.state.time_pos)
                .filter(|(c, _, _)| *c == id)
                .map(|(_, _, t)| t),
            _ => None,
        };
        if let Some(t) = playhead {
            let x = x_of(t);
            if x >= bar.left() && x <= bar.right() {
                painter.line_segment(
                    [pos2(x, rect.top()), pos2(x, rect.bottom())],
                    Stroke::new(2.0, vis.strong_text_color()),
                );
                // keep the time label inside the widget near the ends
                let lx = x.clamp(rect.left() + 28.0, rect.right() - 28.0);
                painter.text(
                    pos2(lx, rect.top()),
                    egui::Align2::CENTER_TOP,
                    timefmt::display(t),
                    egui::FontId::monospace(10.0),
                    vis.strong_text_color(),
                );
            }
        }
        // view range labels
        for (t, align, x) in [
            (v0, egui::Align2::LEFT_BOTTOM, bar.left()),
            (v1, egui::Align2::RIGHT_BOTTOM, bar.right()),
        ] {
            painter.text(
                pos2(x, rect.bottom()),
                align,
                timefmt::display(t),
                egui::FontId::monospace(10.0),
                vis.weak_text_color(),
            );
        }

        // edge under the pointer
        let hover_edge = |p: Pos2| -> Option<(usize, Edge)> {
            if !bar.expand(4.0).contains(p) {
                return None;
            }
            let mut best: Option<(usize, Edge, f32)> = None;
            for (i, s) in segs.iter().enumerate() {
                for (edge, t) in [(Edge::Start, s.start), (Edge::End, s.end)] {
                    let x = x_of(t);
                    let d = (x - p.x).abs();
                    // where two edges meet (a split point), the pointer side decides:
                    // left of it grabs the End of the left segment, right of it the Start
                    let wrong_side = match edge {
                        Edge::Start => p.x < x,
                        Edge::End => p.x > x,
                    };
                    let score = d + if wrong_side { 0.5 } else { 0.0 };
                    if d <= EDGE_GRAB_PX && best.is_none_or(|b| score < b.2) {
                        best = Some((i, edge, score));
                    }
                }
            }
            best.map(|(i, e, _)| (i, e))
        };

        if let Some(p) = resp.hover_pos() {
            if let Some((i, edge)) = hover_edge(p) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                let s = &segs[i];
                let tip = match edge {
                    Edge::Start if fi.is_key_at(s.start) => {
                        "Starts on a keyframe: copied without re-encoding".to_string()
                    }
                    Edge::Start => match fi.next_key_at_or_after(s.start).filter(|k| *k < s.end) {
                        Some(k) => {
                            format!("Re-encodes {:.1} s up to the next keyframe", k - s.start)
                        }
                        None => "No keyframe inside: the whole segment is re-encoded".into(),
                    },
                    Edge::End => "Drag to trim the end (Shift: snap to keyframes)".into(),
                };
                resp.clone().on_hover_text(tip);
            }
            // zoom (Ctrl+wheel or pinch: egui reports it as zoom_delta) and pan (wheel)
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            let delta = if scroll.y != 0.0 { scroll.y } else { scroll.x };
            if zoom != 1.0 || delta != 0.0 {
                let span = v1 - v0;
                if zoom != 1.0 {
                    let factor = (1.0 / zoom as f64).clamp(0.5, 2.0);
                    let anchor = t_of(p.x);
                    let new_span = (span * factor).clamp(0.5_f64.min(dur), dur);
                    let mut s0 = anchor - (anchor - v0) * new_span / span;
                    s0 = s0.clamp(0.0, (dur - new_span).max(0.0));
                    self.timeline = if new_span >= dur - 1e-6 {
                        TimelineView::default()
                    } else {
                        TimelineView {
                            start: s0,
                            end: s0 + new_span,
                        }
                    };
                } else if span < dur {
                    let shift = -(delta as f64) / w as f64 * span;
                    let s0 = (v0 + shift).clamp(0.0, dur - span);
                    self.timeline = TimelineView {
                        start: s0,
                        end: s0 + span,
                    };
                }
            }
        }

        // start of a drag: an edge grabs it, otherwise scrub
        // (hit-test where the button went down: drag_started fires only after
        // the pointer has moved a few pixels)
        if resp.drag_started()
            && let Some(p) = ui
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos())
            && let Some((i, edge)) = hover_edge(p)
        {
            self.selected_seg = Some(segs[i].id);
            self.drag = Some(Drag {
                clip: id,
                seg: segs[i].id,
                edge,
                before: self.project.clips.clone(),
            });
        }

        if let Some(p) = resp.interact_pointer_pos()
            && (resp.dragged() || resp.clicked())
        {
            let shift = ui.input(|i| i.modifiers.shift);
            let raw = t_of(p.x);
            if let Some(d) = &self.drag {
                if d.clip == id {
                    let (seg_id, edge) = (d.seg, d.edge);
                    self.trim_edge(id, seg_id, edge, raw, shift);
                }
            } else {
                let t = fi.snap(raw);
                if resp.clicked()
                    && let Some(i) = edit::segment_at(&segs, raw)
                {
                    self.selected_seg = Some(segs[i].id);
                }
                self.seek(t);
            }
        }

        if resp.drag_stopped()
            && let Some(d) = self.drag.take()
        {
            let changed = d
                .before
                .iter()
                .find(|c| c.id == d.clip)
                .map(|c| c.segments.clone())
                != Some(self.clip(d.clip).and_then(|c| c.segments.clone()));
            if changed {
                self.history.push(d.before);
                self.mark_plan_dirty();
            }
        }
    }
}

/// Nearest keyframe (or the clip end).
fn snap_key(fi: &FrameIndex, t: f64) -> f64 {
    let a = fi.last_key_at_or_before(t);
    let b = fi.next_key_at_or_after(t).or(Some(fi.end));
    match (a, b) {
        (Some(a), Some(b)) => {
            if t - a <= b - t {
                a
            } else {
                b
            }
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => t,
    }
}

impl App {
    /// Moves a segment edge to `t` (snapped to a frame, or to a keyframe with
    /// `to_key`), clamped so the segment keeps at least one frame and doesn't
    /// overlap its neighbours. Shows the frame at the edge. No undo step here:
    /// the caller records one for the whole drag.
    pub fn trim_edge(
        &mut self,
        clip: uuid::Uuid,
        seg: uuid::Uuid,
        edge: Edge,
        t: f64,
        to_key: bool,
    ) {
        let Some(info) = self.info(clip) else { return };
        let fi = &info.frames;
        let t = if to_key { snap_key(fi, t) } else { fi.snap(t) };
        let dur = info.video_end();
        let Some(c) = self.project.clips.iter_mut().find(|c| c.id == clip) else {
            return;
        };
        let Some(segs) = c.segments.as_mut() else {
            return;
        };
        let Some(i) = segs.iter().position(|s| s.id == seg) else {
            return;
        };
        let t = match edge {
            Edge::Start => {
                let lo = if i > 0 { segs[i - 1].end } else { 0.0 };
                let hi = fi
                    .prev_frame_time(segs[i].end)
                    .unwrap_or(segs[i].start)
                    .max(lo);
                let t = t.clamp(lo, hi);
                segs[i].start = t;
                t
            }
            Edge::End => {
                let lo = fi.time_of(fi.idx_at_or_after(segs[i].start) + 1);
                let hi = segs.get(i + 1).map_or(dur, |s| s.start).max(lo);
                let t = t.clamp(lo, hi);
                segs[i].end = t;
                t
            }
        };
        self.mark_plan_dirty();
        // show the frame at the edge (for the end: the last kept frame)
        let show = if edge == Edge::End {
            fi.prev_frame_time(t).unwrap_or(t)
        } else {
            t
        };
        self.seek(show);
    }
}
