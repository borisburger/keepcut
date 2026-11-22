//! Small vector icons for buttons (transport, segment marks), painted with egui's painter in
//! the button's text colour: no icon font, sharp at any scale, follows the theme.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke, pos2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    PrevFrame,
    NextFrame,
    PrevKeyframe,
    NextKeyframe,
    /// `[`: segment start (mark in)
    MarkIn,
    /// `]`: segment end (mark out)
    MarkOut,
}

/// The icon as a button atom of `size` × `size` points.
pub fn atom<'a>(icon: Icon, size: f32) -> egui::Atom<'a> {
    egui::Atom::paint(egui::Vec2::splat(size), move |ui, args| {
        paint(ui.painter(), args.rect, icon, args.fallback_text_color);
    })
}

/// Icon size for buttons with the current button font.
pub fn size(ui: &egui::Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Button) * 1.05).round()
}

/// An icon-only button (the tooltip says what it does).
pub fn button(ui: &mut egui::Ui, icon: Icon) -> egui::Response {
    let s = size(ui);
    ui.add(egui::Button::new(atom(icon, s)).min_size(egui::vec2(s * 2.0, 0.0)))
}

pub fn paint(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    // drawn facing right in a unit square, mirrored for the left-facing ones
    let (mirror, shapes): (bool, &[Part]) = match icon {
        Icon::Play => (false, &[Part::Tri(0.24, 0.86)]),
        Icon::Pause => (false, &[Part::Bar(0.24, 0.42), Part::Bar(0.58, 0.76)]),
        Icon::NextFrame => (false, FRAME),
        Icon::PrevFrame => (true, FRAME),
        Icon::NextKeyframe => (false, KEYFRAME),
        Icon::PrevKeyframe => (true, KEYFRAME),
        Icon::MarkIn => (false, BRACKET),
        Icon::MarkOut => (true, BRACKET),
    };
    let at = |x: f32, y: f32| -> Pos2 {
        let x = if mirror { 1.0 - x } else { x };
        pos2(
            rect.left() + x * rect.width(),
            rect.top() + y * rect.height(),
        )
    };
    let fill = |p: &Painter, x0: f32, y0: f32, x1: f32, y1: f32| {
        p.rect_filled(Rect::from_two_pos(at(x0, y0), at(x1, y1)), 0.0, color);
    };
    for part in shapes {
        match *part {
            Part::Tri(x0, x1) => {
                let pts = vec![at(x0, TOP), at(x1, 0.5), at(x0, BOTTOM)];
                p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
            }
            Part::Bar(x0, x1) => fill(p, x0, TOP, x1, BOTTOM),
            Part::Rect(x0, y0, x1, y1) => fill(p, x0, y0, x1, y1),
        }
    }
}

const TOP: f32 = 0.14;
const BOTTOM: f32 = 0.86;

#[derive(Clone, Copy)]
enum Part {
    /// right-pointing triangle from x0 to x1, full height
    Tri(f32, f32),
    /// vertical bar from x0 to x1, full height
    Bar(f32, f32),
    Rect(f32, f32, f32, f32),
}

/// ▶| one frame
const FRAME: &[Part] = &[Part::Tri(0.2, 0.66), Part::Bar(0.7, 0.82)];
/// ▶▶| the next keyframe (a longer jump)
const KEYFRAME: &[Part] = &[
    Part::Tri(0.04, 0.42),
    Part::Tri(0.38, 0.76),
    Part::Bar(0.8, 0.92),
];
/// [
const BRACKET: &[Part] = &[
    Part::Rect(0.3, 0.08, 0.44, 0.92),
    Part::Rect(0.3, 0.08, 0.72, 0.2),
    Part::Rect(0.3, 0.8, 0.72, 0.92),
];
