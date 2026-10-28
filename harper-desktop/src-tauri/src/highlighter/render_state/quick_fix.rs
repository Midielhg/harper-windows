//! The one-click fix that floats under a highlight while the pointer is on it.
//!
//! Opening the full popup to take the obvious correction is three movements: point at the word,
//! click it, then find the right row. This offers the top suggestion straight away, so the common
//! case is a single click.

use harper_core::linting::{Lint, Suggestion};

use super::lint_kind_color32;
use crate::rect::Rect;

/// Space between the highlight and the chip.
const ANCHOR_GAP: f32 = 4.0;
const HEIGHT: f32 = 24.0;
const PADDING_X: f32 = 9.0;
/// Width allowed per character of the suggestion.
///
/// Drawing and hit-testing both size the chip from this rather than from a measured frame, so the
/// clickable area is the drawn area even on the first frame it appears.
const CHAR_WIDTH: f32 = 7.4;
const MIN_WIDTH: f32 = 52.0;
const MAX_WIDTH: f32 = 260.0;
const CORNER_RADIUS: u8 = 6;
const FONT_SIZE: f32 = 12.5;

const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(0xff, 0xff, 0xff);
const BORDER: egui::Color32 = egui::Color32::from_rgb(0xd0, 0xd7, 0xde);
const HOVER_BACKGROUND: egui::Color32 = egui::Color32::from_rgb(0xf3, 0xf4, 0xf6);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0x1f, 0x23, 0x28);

/// The suggestion a lint offers for one click, if it has one.
pub fn quick_fix_suggestion(lint: &Lint) -> Option<&Suggestion> {
    lint.suggestions.first()
}

/// The text shown on the chip for `suggestion`.
pub fn quick_fix_label(suggestion: &Suggestion) -> String {
    match suggestion {
        Suggestion::ReplaceWith(chars) => chars.iter().collect(),
        Suggestion::InsertAfter(chars) => format!("+{}", chars.iter().collect::<String>()),
        Suggestion::Remove => "Remove".to_string(),
    }
}

/// Where the chip sits for a lint, in the same screen coordinates as the highlight.
pub fn quick_fix_bounds(rect: &Rect, label: &str) -> egui::Rect {
    let width =
        (PADDING_X * 2.0 + label.chars().count() as f32 * CHAR_WIDTH).clamp(MIN_WIDTH, MAX_WIDTH);

    egui::Rect::from_min_size(
        egui::pos2(rect.x as f32, (rect.y + rect.height) as f32 + ANCHOR_GAP),
        egui::vec2(width, HEIGHT),
    )
}

/// Draws the chip and reports whether it was clicked.
pub fn render_quick_fix(ui: &mut egui::Ui, bounds: egui::Rect, lint: &Lint, label: &str) -> bool {
    let mut clicked = false;

    egui::Area::new(egui::Id::new("harper-quick-fix"))
        .order(egui::Order::Foreground)
        .fixed_pos(bounds.min)
        .show(ui.ctx(), |ui| {
            let (rect, response) = ui.allocate_exact_size(bounds.size(), egui::Sense::click());

            let fill = if response.hovered() {
                HOVER_BACKGROUND
            } else {
                BACKGROUND
            };

            ui.painter().rect(
                rect,
                egui::CornerRadius::same(CORNER_RADIUS),
                fill,
                egui::Stroke::new(1.0_f32, BORDER),
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    rect.left_top() + egui::vec2(PADDING_X * 0.5, HEIGHT * 0.25),
                    egui::vec2(3.0, HEIGHT * 0.5),
                ),
                1.5,
                lint_kind_color32(lint.lint_kind),
            );
            ui.painter().text(
                rect.left_center() + egui::vec2(PADDING_X + 2.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(FONT_SIZE),
                TEXT,
            );

            if response.clicked() {
                clicked = true;
            }

            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        });

    clicked
}
