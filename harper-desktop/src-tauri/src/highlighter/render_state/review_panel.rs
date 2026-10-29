//! The panel that gathers every outstanding lint once the user stops typing.
//!
//! Fixing a page of writing one highlight at a time means hunting for each squiggle. After a pause
//! the whole list is laid out in one place instead, each row showing what was written and what
//! Harper would put there, so the work can be done from one spot — or taken in a single click.

use std::time::Duration;

use harper_core::linting::{Lint, Suggestion};

use super::lint_kind_color32;
use crate::rect::ActionableLint;

/// How long the lints must sit unchanged before the panel appears.
///
/// Short enough to come up when someone finishes a thought, long enough to stay out of the way
/// while they are still typing.
pub const IDLE_DELAY: Duration = Duration::from_secs(5);

const WIDTH: f32 = 320.0;
const ROW_HEIGHT: f32 = 44.0;
const HEADER_HEIGHT: f32 = 34.0;
const FOOTER_HEIGHT: f32 = 44.0;
const PADDING: f32 = 10.0;
const GAP_FROM_TEXT: f32 = 16.0;

/// The most rows shown at once, so a page full of mistakes cannot grow a panel taller than the
/// screen it has to fit on.
const MAX_ROWS: usize = 8;

const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(0xff, 0xff, 0xff);
const BORDER: egui::Color32 = egui::Color32::from_rgb(0xd0, 0xd7, 0xde);
const HOVER: egui::Color32 = egui::Color32::from_rgb(0xf3, 0xf4, 0xf6);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0x1f, 0x23, 0x28);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x6b, 0x72, 0x80);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x7f, 0x37);

/// What the user asked the panel to do.
pub enum ReviewAction {
    ApplyOne(usize, Suggestion),
    ApplyAll,
    Dismiss,
}

/// One line of the panel: where the lint sits in the full list, and what it offers.
pub struct ReviewRow {
    pub index: usize,
    pub written: String,
    pub replacement: String,
    pub suggestion: Suggestion,
    pub color: egui::Color32,
}

/// Builds the rows for the lints that actually offer a fix.
///
/// A lint with nothing to suggest, such as a sentence merely being long, has no row: the panel is
/// a list of things that can be done, not a list of complaints.
pub fn review_rows(lints: &[ActionableLint]) -> Vec<ReviewRow> {
    lints
        .iter()
        .enumerate()
        .filter_map(|(index, actionable_lint)| {
            let suggestion = actionable_lint.lint.suggestions.first()?;
            let written = written_text(&actionable_lint.lint, &actionable_lint.source_text);

            Some(ReviewRow {
                index,
                replacement: replacement_text(suggestion, &written),
                written,
                suggestion: suggestion.clone(),
                color: lint_kind_color32(actionable_lint.lint.lint_kind),
            })
        })
        .take(MAX_ROWS)
        .collect()
}

/// The text the lint is about, shortened so a long run-on does not stretch the row.
fn written_text(lint: &Lint, source_text: &str) -> String {
    let characters: Vec<char> = source_text.chars().collect();
    let span = lint.span;

    if span.start >= characters.len() {
        return String::new();
    }

    let end = span.end.min(characters.len());

    ellipsized(&characters[span.start..end].iter().collect::<String>())
}

/// How the row describes what the suggestion would do.
fn replacement_text(suggestion: &Suggestion, written: &str) -> String {
    match suggestion {
        Suggestion::ReplaceWith(characters) => ellipsized(&characters.iter().collect::<String>()),
        Suggestion::InsertAfter(characters) => ellipsized(&format!(
            "{written}{}",
            characters.iter().collect::<String>()
        )),
        Suggestion::Remove => "Remove".to_string(),
    }
}

/// Caps a string at a length the row can show, ending it with an ellipsis when it is cut.
fn ellipsized(text: &str) -> String {
    const LIMIT: usize = 38;

    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");

    if single_line.chars().count() <= LIMIT {
        return single_line;
    }

    single_line.chars().take(LIMIT - 1).collect::<String>() + "\u{2026}"
}

/// Where the panel sits, in the same screen coordinates as the highlights.
///
/// It is placed beside the text rather than over it, so the writing being corrected stays readable
/// while the list is up.
pub fn panel_bounds(
    lints: &[ActionableLint],
    row_count: usize,
    screens: &[egui::Rect],
) -> egui::Rect {
    let height = HEADER_HEIGHT + ROW_HEIGHT * row_count as f32 + FOOTER_HEIGHT;

    let extent = lints
        .iter()
        .map(|actionable_lint| &actionable_lint.rect)
        .fold(None::<(f64, f64)>, |extent, rect| {
            let right = rect.x + rect.width;

            Some(match extent {
                Some((top, far)) => (top.min(rect.y), far.max(right)),
                None => (rect.y, right),
            })
        });

    let (top, right) = extent.unwrap_or((0.0, 0.0));

    let size = egui::vec2(WIDTH, height);
    let position = egui::pos2(right as f32 + GAP_FROM_TEXT, top as f32);

    // Anchoring beside the text puts the panel off the edge whenever the writing runs close to the
    // right of the screen, and a panel the user has to go looking for on another monitor is worse
    // than one overlapping a little. Clamp it to the monitor the writing is on, found from where
    // the text itself sits rather than from whichever overlay window drew last. Clamping here,
    // where hit-testing reads the bounds too, keeps what is drawn and what is clickable the same.
    let anchor = egui::pos2(right as f32, top as f32);

    let Some(screen) = screens
        .iter()
        .find(|screen| screen.contains(anchor))
        .or_else(|| screens.iter().find(|screen| screen.contains(position)))
    else {
        return egui::Rect::from_min_size(position, size);
    };

    egui::Rect::from_min_size(
        egui::pos2(
            position
                .x
                .min(screen.right() - size.x - PADDING)
                .max(screen.left() + PADDING),
            position
                .y
                .min(screen.bottom() - size.y - PADDING)
                .max(screen.top() + PADDING),
        ),
        size,
    )
}

/// Draws the panel and reports what the user clicked.
pub fn render_review_panel(
    ui: &mut egui::Ui,
    bounds: egui::Rect,
    rows: &[ReviewRow],
    total: usize,
) -> Option<ReviewAction> {
    let mut action = None;

    egui::Area::new(egui::Id::new("harper-review-panel"))
        .order(egui::Order::Foreground)
        .fixed_pos(bounds.min)
        .show(ui.ctx(), |ui| {
            let (rect, _) = ui.allocate_exact_size(bounds.size(), egui::Sense::hover());

            ui.painter().rect(
                rect,
                egui::CornerRadius::same(8),
                BACKGROUND,
                egui::Stroke::new(1.0_f32, BORDER),
                egui::StrokeKind::Inside,
            );

            let heading = if total == 1 {
                "1 suggestion".to_string()
            } else {
                format!("{total} suggestions")
            };
            ui.painter().text(
                rect.left_top() + egui::vec2(PADDING, HEADER_HEIGHT / 2.0),
                egui::Align2::LEFT_CENTER,
                heading,
                egui::FontId::proportional(13.0),
                MUTED,
            );

            let close = egui::Rect::from_min_size(
                rect.right_top() + egui::vec2(-HEADER_HEIGHT, 0.0),
                egui::vec2(HEADER_HEIGHT, HEADER_HEIGHT),
            );
            if ui
                .interact(
                    close,
                    ui.id().with("harper-review-close"),
                    egui::Sense::click(),
                )
                .clicked()
            {
                action = Some(ReviewAction::Dismiss);
            }
            ui.painter().text(
                close.center(),
                egui::Align2::CENTER_CENTER,
                "\u{00d7}",
                egui::FontId::proportional(17.0),
                MUTED,
            );

            for (position, row) in rows.iter().enumerate() {
                let bounds = egui::Rect::from_min_size(
                    rect.left_top() + egui::vec2(0.0, HEADER_HEIGHT + ROW_HEIGHT * position as f32),
                    egui::vec2(rect.width(), ROW_HEIGHT),
                );

                let response = ui.interact(
                    bounds,
                    ui.id().with(("harper-review-row", row.index)),
                    egui::Sense::click(),
                );

                if response.hovered() {
                    ui.painter().rect_filled(bounds, 0.0, HOVER);
                }
                if response.clicked() {
                    action = Some(ReviewAction::ApplyOne(row.index, row.suggestion.clone()));
                }

                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        bounds.left_top() + egui::vec2(PADDING, ROW_HEIGHT * 0.25),
                        egui::vec2(3.0, ROW_HEIGHT * 0.5),
                    ),
                    1.5,
                    row.color,
                );
                ui.painter().text(
                    bounds.left_center() + egui::vec2(PADDING + 10.0, -8.0),
                    egui::Align2::LEFT_CENTER,
                    &row.written,
                    egui::FontId::proportional(12.0),
                    MUTED,
                );
                ui.painter().text(
                    bounds.left_center() + egui::vec2(PADDING + 10.0, 9.0),
                    egui::Align2::LEFT_CENTER,
                    &row.replacement,
                    egui::FontId::proportional(13.5),
                    TEXT,
                );

                response.on_hover_cursor(egui::CursorIcon::PointingHand);
            }

            let accept = egui::Rect::from_min_size(
                rect.left_bottom() + egui::vec2(PADDING, -FOOTER_HEIGHT + 7.0),
                egui::vec2(rect.width() - PADDING * 2.0, FOOTER_HEIGHT - 14.0),
            );
            let response = ui.interact(
                accept,
                ui.id().with("harper-review-accept-all"),
                egui::Sense::click(),
            );

            if response.clicked() {
                action = Some(ReviewAction::ApplyAll);
            }

            ui.painter().rect(
                accept,
                egui::CornerRadius::same(6),
                if response.hovered() {
                    HOVER
                } else {
                    BACKGROUND
                },
                egui::Stroke::new(1.0_f32, ACCENT),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                accept.center(),
                egui::Align2::CENTER_CENTER,
                "Accept all",
                egui::FontId::proportional(13.0),
                ACCENT,
            );

            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        });

    action
}

#[cfg(test)]
mod tests {
    use harper_core::linting::Lint;

    use super::panel_bounds;
    use crate::rect::{ActionableLint, Rect};

    /// A lint occupying `rect`, which is all `panel_bounds` looks at.
    fn lint_at(rect: Rect) -> ActionableLint {
        ActionableLint::new(rect, "Test".to_string(), Lint::default(), String::new(), |_| {})
    }

    /// Two monitors side by side: the main one, and a second to the right of it.
    const MAIN: [f32; 4] = [0.0, 0.0, 1920.0, 1080.0];
    const SECOND: [f32; 4] = [1920.0, 0.0, 3840.0, 1080.0];

    fn screens() -> Vec<egui::Rect> {
        [MAIN, SECOND]
            .iter()
            .map(|[left, top, right, bottom]| {
                egui::Rect::from_min_max(egui::pos2(*left, *top), egui::pos2(*right, *bottom))
            })
            .collect()
    }

    #[test]
    fn stays_on_the_monitor_the_writing_is_on() {
        let lints = vec![lint_at(Rect::new(2400.0, 300.0, 80.0, 20.0))];

        let bounds = panel_bounds(&lints, 2, &screens());

        assert!(
            bounds.min.x >= 1920.0,
            "the panel was pulled onto the main monitor: {bounds:?}"
        );
    }

    #[test]
    fn is_pulled_back_from_the_edge_of_its_own_monitor() {
        // Writing hard against the right edge of the second monitor: the panel cannot sit beside
        // it without running off, so it has to come back rather than spill onto nothing.
        let lints = vec![lint_at(Rect::new(3800.0, 300.0, 30.0, 20.0))];

        let bounds = panel_bounds(&lints, 2, &screens());

        assert!(bounds.max.x <= 3840.0, "ran off the right: {bounds:?}");
        assert!(bounds.min.x >= 1920.0, "ran onto the main screen: {bounds:?}");
    }

    #[test]
    fn sits_beside_writing_on_the_main_monitor() {
        let lints = vec![lint_at(Rect::new(400.0, 300.0, 80.0, 20.0))];

        let bounds = panel_bounds(&lints, 2, &screens());

        assert!(bounds.min.x >= 480.0, "overlapped the writing: {bounds:?}");
        assert!(bounds.max.x <= 1920.0, "ran onto the second screen: {bounds:?}");
    }
}
