mod draw_highlight;
mod popup_layout;
mod quick_fix;
mod render_lint_card;
mod review_panel;

use std::time::Instant;

use egui_commonmark::CommonMarkCache;
use harper_core::{Document, linting::LintKind, linting::Suggestion};

use self::quick_fix::{quick_fix_bounds, quick_fix_label, quick_fix_suggestion, render_quick_fix};
use self::review_panel::{
    REVIEW_BUTTON_GAP, REVIEW_BUTTON_SIZE, ReviewAction, ReviewRow, panel_bounds,
    render_review_button, render_review_panel, review_rows,
};
use self::{
    draw_highlight::draw_highlight,
    popup_layout::{PopupLayout, approach},
    render_lint_card::{
        FIRST_ROW_OFFSET, LintCardAction, popup_layout_for_lint, render_lint_card, row_count,
        sidebar_target,
    },
};
use super::{AddToDictionary, DisableRule, IgnoreLint, OpenSettings};
use crate::{
    lint_kind_color::lint_kind_color,
    rect::{ActionableLint, Rect, Selection},
};

/// A lint marker, popup, or empty overlay region found during cursor hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    Lint(usize),
    Popup,
    /// The one-click fix offered under the lint at this index.
    QuickFix(usize),
    /// The list of everything outstanding, opened from the selection.
    ReviewPanel,
    /// The button offered at the start of a selection, which opens that list.
    ReviewButton,
    None,
}

/// Stores highlighter-specific drawing state and renders it into an egui frame.
///
/// `Window` owns the native window and GPU plumbing; `RenderState` owns the content we want to draw
/// into that plumbing. This keeps future highlight rectangles, styling, and animation state out of
/// the platform/rendering infrastructure.
pub struct RenderState {
    /// Last successful accessibility read, retained when a later read fails.
    last_lints: Option<Vec<ActionableLint>>,

    /// Index of the lint whose suggestion popup is currently visible.
    highlighted_lint: Option<usize>,

    /// Index of the suggestion row that the user hovered last in the visible popup.
    shown_suggestion: Option<usize>,

    /// Index of the lint the pointer is on, which is offered a one-click fix.
    hovered_lint: Option<usize>,

    /// When the set of lints last changed, which is when the user last typed something that
    /// mattered. The review panel waits for this to go quiet.
    lints_changed_at: Instant,

    /// Set when the user closes the panel, so it stays closed until the writing changes again.
    review_dismissed: bool,

    /// What the user has selected in the field being written in.
    selection: Option<Selection>,

    /// Set when the user clicks the button by their selection, and the only thing that opens the
    /// review panel. The panel used to let itself in after a pause in typing, which put it over
    /// the sentence being written and made it something to dismiss rather than something to use.
    review_requested: bool,

    /// Every monitor the overlay covers, in screen coordinates, used to keep the review panel on
    /// the one the writing is on.
    ///
    /// There is an overlay window per monitor and they share this state, so remembering only the
    /// screen drawn into last would be whichever window happened to render last — which is how the
    /// panel ended up on the main monitor while the typing was on another. Empty until the first
    /// frame, which is long before the panel can appear.
    screen_rects: Vec<egui::Rect>,

    /// Set when the user asks the panel to take every suggestion, for the event loop to pick up.
    ///
    /// Applying them needs the OS broker, which lives a layer above this one, so the request is
    /// left here rather than acted on during drawing.
    apply_all_requested: bool,

    /// The flyout card's drawn offset from the list card's top, in points.
    current_sidebar_position: f32,

    /// When `render` last ran, used to move the sidebar at a speed that follows real time.
    last_render: Option<Instant>,

    /// How far the user has dragged the popup from where the layout placed it.
    ///
    /// Drawing and hit-testing both add this, so the cards and their clickable area stay together.
    /// It resets whenever a popup opens or closes, so each lint starts at its own anchor.
    popup_drag: egui::Vec2,

    /// Cache for markdown-rendered lint messages so popup redraws do not repeatedly rebuild markdown
    /// resources from scratch.
    markdown_cache: CommonMarkCache,

    /// Called when the user dismisses a lint so app-level ignored-lint state can be updated.
    ignore_lint: IgnoreLint,

    /// Called when the user adds a spelling lint's source text to the local dictionary.
    add_to_dictionary: AddToDictionary,

    /// Called when the user disables the rule that produced the selected lint.
    disable_rule: DisableRule,

    open_settings: OpenSettings,
}

impl RenderState {
    /// Creates render state through `set_lints` so initial lint data gets the same stale-popup guard
    /// used by later accessibility refreshes.
    pub fn new(
        rects: Vec<ActionableLint>,
        ignore_lint: IgnoreLint,
        add_to_dictionary: AddToDictionary,
        disable_rule: DisableRule,
        open_settings: OpenSettings,
    ) -> Self {
        let mut state = Self {
            last_lints: None,
            highlighted_lint: None,
            shown_suggestion: None,
            hovered_lint: None,
            lints_changed_at: Instant::now(),
            review_dismissed: false,
            selection: None,
            review_requested: false,
            screen_rects: Vec::new(),
            apply_all_requested: false,
            current_sidebar_position: FIRST_ROW_OFFSET,
            last_render: None,
            popup_drag: egui::Vec2::ZERO,
            markdown_cache: CommonMarkCache::default(),
            ignore_lint,
            add_to_dictionary,
            disable_rule,
            open_settings,
        };
        state.set_lints(rects);
        state
    }

    /// Saves successful accessibility reads while retaining the previous lints after a failed read.
    ///
    /// Closes the popup when its lint disappeared, and forgets the shown suggestion when the
    /// refreshed lint has fewer rows.
    pub fn set_lints(&mut self, lints: Vec<ActionableLint>) {
        // Compare before storing: an identical read means the user has not written anything since
        // the last one, which is what the review panel is waiting for.
        if !self.lints_match(&lints) {
            self.lints_changed_at = Instant::now();
            self.review_dismissed = false;
        }

        self.last_lints = Some(lints);

        if self
            .highlighted_lint
            .is_some_and(|index| index >= self.lints().len())
        {
            self.close_popup();
        }

        let row_count = self
            .highlighted_lint
            .and_then(|index| self.lints().get(index))
            .map(|positioned_lint| row_count(&positioned_lint.lint));
        self.shown_suggestion = self
            .shown_suggestion
            .filter(|index| row_count.is_some_and(|row_count| *index < row_count));
    }

    /// Carries everything drawn along with a window that has moved.
    ///
    /// This is a stand-in until the next read of the app, not a substitute for one: it assumes the
    /// text sits still inside the window, which is true of a move and false of anything that
    /// reflows. The positions it produces are never stored anywhere that outlives the next read.
    pub fn translate_lints(&mut self, dx: f64, dy: f64) {
        if let Some(lints) = self.last_lints.as_mut() {
            for actionable_lint in lints {
                actionable_lint.rect = actionable_lint.rect.translated(dx, dy);
            }
        }

        if let Some(selection) = self.selection.as_mut() {
            selection.anchor = selection.anchor.translated(dx, dy);
        }
    }

    /// The lints currently drawn, so the window manager can act on all of them at once.
    pub fn all_lints(&self) -> &[ActionableLint] {
        self.lints()
    }

    fn lints(&self) -> &[ActionableLint] {
        self.last_lints.as_deref().unwrap_or_default()
    }

    /// Updates which lint owns the suggestion popup without exposing render-state internals.
    ///
    /// Window/input code decides what the user interacted with, while `RenderState` owns the popup
    /// selection. Filtering invalid indexes here keeps stale cursor state from selecting a lint that
    /// disappeared after the latest accessibility read.
    ///
    /// Like the Chrome extension, each new selection starts again from the first suggestion row.
    pub fn set_highlighted_lint(&mut self, highlighted_lint: Option<usize>) {
        self.highlighted_lint = highlighted_lint.filter(|index| *index < self.lints().len());
        self.shown_suggestion = None;
        self.current_sidebar_position = FIRST_ROW_OFFSET;
        self.popup_drag = egui::Vec2::ZERO;
    }

    /// Applies a suggestion and drops the lint from the overlay at once.
    ///
    /// The edit itself happens in the background, and the next read of the app is a moment away,
    /// so leaving the highlight up until then makes a fix that already happened look like it is
    /// still pending.
    fn apply_and_forget(&mut self, index: usize, suggestion: Suggestion) {
        if let Some(actionable_lint) = self
            .last_lints
            .as_mut()
            .and_then(|lints| lints.get_mut(index))
        {
            actionable_lint.apply_suggestion(suggestion);
        }

        if let Some(lints) = self.last_lints.as_mut()
            && index < lints.len()
        {
            lints.remove(index);
        }

        self.hovered_lint = None;
        self.close_popup();
    }

    /// Centralizes the close behavior so the popup close button only has to clear the selected lint.
    pub fn close_popup(&mut self) {
        self.highlighted_lint = None;
        self.shown_suggestion = None;
        self.current_sidebar_position = FIRST_ROW_OFFSET;
        self.popup_drag = egui::Vec2::ZERO;
    }

    /// Whether `lints` says the same as what is already on screen.
    fn lints_match(&self, lints: &[ActionableLint]) -> bool {
        let current = self.lints();

        current.len() == lints.len()
            && current.iter().zip(lints).all(|(current, new)| {
                // The written text has to be compared too, not just the lints drawn from it. The
                // panel waits for this to stop changing as its sign that the writing has stopped,
                // and a whole sentence can be typed without the set of mistakes in it changing at
                // all — so on the lints alone the wait ran out mid-sentence and the panel opened
                // over the words still being written.
                current.source_text == new.source_text
                    && current.lint.span == new.lint.span
                    && current.lint.message == new.lint.message
            })
    }

    /// The review panel's rows and where it sits, when it should be on screen.
    ///
    /// It stays away while a popup is open or the user is still typing, and it needs something to
    /// offer: a list with nothing actionable in it is just clutter.
    pub fn review_panel_layout(&self) -> Option<(egui::Rect, Vec<ReviewRow>)> {
        if self.review_dismissed || self.highlighted_lint.is_some() || !self.review_requested {
            return None;
        }

        let lints = self.selected_lints();
        let rows = review_rows(&lints);

        if rows.is_empty() {
            return None;
        }

        Some((panel_bounds(&lints, rows.len(), &self.screen_rects), rows))
    }

    /// Notes what the user has selected, closing the panel when the selection moves on.
    ///
    /// Leaving it open over a selection it no longer describes would let a click apply a fix to
    /// text the user is no longer looking at.
    pub fn set_selection(&mut self, selection: Option<Selection>) {
        if self.selection == selection {
            return;
        }

        self.selection = selection;
        self.review_requested = false;
        self.review_dismissed = false;
    }

    /// The lints inside the current selection.
    fn selected_lints(&self) -> Vec<(usize, &ActionableLint)> {
        let Some(selection) = &self.selection else {
            return Vec::new();
        };

        self.lints()
            .iter()
            .enumerate()
            .filter(|(_, actionable_lint)| selection.contains(actionable_lint.lint.span))
            .collect()
    }

    /// Where the button that opens the review panel sits, when there is one to offer.
    ///
    /// It hangs off the start of the selection, which is where the eye already is after dragging
    /// across a sentence, and it is only offered while there is something inside the selection to
    /// fix — a button that opens an empty list is just something else to look at.
    pub fn review_button_bounds(&self) -> Option<egui::Rect> {
        if self.review_requested || self.highlighted_lint.is_some() {
            return None;
        }

        let selection = self.selection.as_ref()?;

        if review_rows(&self.selected_lints()).is_empty() {
            return None;
        }

        let anchor = &selection.anchor;
        let middle = anchor.y as f32 + anchor.height as f32 / 2.0;

        Some(egui::Rect::from_center_size(
            egui::pos2(
                anchor.x as f32 - REVIEW_BUTTON_GAP - REVIEW_BUTTON_SIZE / 2.0,
                middle,
            ),
            egui::vec2(REVIEW_BUTTON_SIZE, REVIEW_BUTTON_SIZE),
        ))
    }

    /// Opens the review panel, as asked for by the button.
    pub fn request_review(&mut self) {
        self.review_requested = true;
    }

    /// Notes the screen one of the overlay windows covers.
    ///
    /// Each window reports the same rectangle every frame, so a monitor already known is updated
    /// in place rather than collected again.
    fn remember_screen(&mut self, screen: egui::Rect) {
        match self
            .screen_rects
            .iter_mut()
            .find(|known| known.min == screen.min)
        {
            Some(known) => *known = screen,
            None => self.screen_rects.push(screen),
        }
    }

    /// Takes the pending "fix everything" request, if the user made one.
    pub fn take_apply_all_request(&mut self) -> bool {
        std::mem::take(&mut self.apply_all_requested)
    }

    /// Follows the pointer so the lint under it can offer its top suggestion.
    pub fn set_hovered_lint(&mut self, hovered_lint: Option<usize>) {
        self.hovered_lint = hovered_lint.filter(|index| *index < self.lints().len());
    }

    /// The chip offering a one-click fix, when one is on screen.
    ///
    /// There is none while the full popup is open, so the two never overlap.
    pub fn quick_fix_layout(&self) -> Option<(usize, egui::Rect)> {
        if self.highlighted_lint.is_some() {
            return None;
        }

        let index = self.hovered_lint?;
        let actionable_lint = self.lints().get(index)?;
        let suggestion = quick_fix_suggestion(&actionable_lint.lint)?;

        Some((
            index,
            quick_fix_bounds(&actionable_lint.rect, &quick_fix_label(suggestion)),
        ))
    }

    /// Checks if a given position touches a hit target.
    pub fn hit_target_at_pos(&self, pos: egui::Pos2) -> HitTarget {
        if self
            .popup_layout()
            .is_some_and(|layout| layout.contains(pos))
        {
            return HitTarget::Popup;
        }

        if let Some((bounds, _)) = self.review_panel_layout()
            && bounds.contains(pos)
        {
            return HitTarget::ReviewPanel;
        }

        if let Some((index, bounds)) = self.quick_fix_layout()
            && bounds.contains(pos)
        {
            return HitTarget::QuickFix(index);
        }

        if self
            .review_button_bounds()
            .is_some_and(|bounds| bounds.contains(pos))
        {
            return HitTarget::ReviewButton;
        }

        self.lints()
            .iter()
            .position(|positioned_lint| {
                rect_bounds(&positioned_lint.rect.with_friendly_padding()).contains(pos)
            })
            .map_or(HitTarget::None, HitTarget::Lint)
    }

    /// Computes popup hit-test bounds from our layout contract instead of waiting for egui to report
    /// rendered bounds, which keeps hit-testing available before the next render pass completes.
    pub fn popup_layout(&self) -> Option<PopupLayout> {
        self.highlighted_lint
            .and_then(|index| self.lints().get(index))
            .map(|positioned_lint| {
                popup_layout_for_lint(
                    &positioned_lint.rect,
                    &positioned_lint.lint,
                    self.current_sidebar_position,
                )
                .translate(self.popup_drag)
            })
    }

    /// Draws highlights and the active popup from the same state used by hit-testing so visible
    /// regions and clickable regions do not drift apart.
    ///
    /// Each call also moves the sidebar toward its target by the time since the previous call.
    pub fn render(&mut self, ui: &mut egui::Ui, origin: egui::Vec2) {
        self.remember_screen(ui.ctx().screen_rect().translate(origin));

        let now = Instant::now();
        let time_since_last_render = self
            .last_render
            .map_or(0.0, |last_render| (now - last_render).as_secs_f32());
        self.last_render = Some(now);

        if let Some(positioned_lint) = self
            .highlighted_lint
            .and_then(|index| self.lints().get(index))
        {
            let target = sidebar_target(&positioned_lint.lint, self.shown_suggestion);
            self.current_sidebar_position = approach(
                self.current_sidebar_position,
                target,
                time_since_last_render,
            );
        }

        for positioned_lint in self.lints() {
            draw_highlight(ui, &positioned_lint.rect, &positioned_lint.lint, origin);
        }

        if let Some((index, bounds)) = self.quick_fix_layout()
            && let Some(actionable_lint) = self.lints().get(index)
            && let Some(suggestion) = quick_fix_suggestion(&actionable_lint.lint)
        {
            let lint = actionable_lint.lint.clone();
            let label = quick_fix_label(suggestion);
            let suggestion = suggestion.clone();

            if render_quick_fix(ui, bounds.translate(-origin), &lint, &label) {
                self.apply_and_forget(index, suggestion);
            }
        }

        if let Some(bounds) = self.review_button_bounds()
            && render_review_button(ui, bounds.translate(-origin))
        {
            self.request_review();
        }

        if let Some((bounds, rows)) = self.review_panel_layout() {
            let total = self.selected_lints().len();

            match render_review_panel(ui, bounds.translate(-origin), &rows, total) {
                Some(ReviewAction::ApplyOne(index, suggestion)) => {
                    self.apply_and_forget(index, suggestion);
                }
                Some(ReviewAction::ApplyAll) => {
                    self.apply_all_requested = true;
                    self.review_dismissed = true;
                }
                Some(ReviewAction::Dismiss) => self.review_dismissed = true,
                None => {}
            }
        }

        if let Some(index) = self.highlighted_lint
            && let Some(positioned_lint) = self.lints().get(index)
        {
            let rect = positioned_lint.rect;
            let lint = positioned_lint.lint.clone();
            let source_text = positioned_lint.source_text.clone();

            let mut drag = self.popup_drag;
            let action = render_lint_card(
                ui,
                &rect,
                &lint,
                &source_text,
                self.shown_suggestion,
                self.current_sidebar_position,
                &mut self.markdown_cache,
                origin,
                &mut drag,
            );
            self.popup_drag = drag;

            match action {
                Some(LintCardAction::Close) => self.close_popup(),
                Some(LintCardAction::OpenSettings) => {
                    (self.open_settings)();
                    self.close_popup();
                }
                Some(LintCardAction::HoverSuggestion(suggestion)) => {
                    self.shown_suggestion = Some(suggestion);
                }
                Some(LintCardAction::ApplySuggestion(suggestion)) => {
                    self.apply_and_forget(index, suggestion);
                }
                Some(LintCardAction::IgnoreLint) => {
                    if let Some((lint, source_text)) =
                        self.lints().get(index).map(|actionable_lint| {
                            (
                                actionable_lint.lint.clone(),
                                actionable_lint.source_text.clone(),
                            )
                        })
                    {
                        let document = Document::new_markdown_default_curated(&source_text);
                        (self.ignore_lint)(&lint, &document);
                    }

                    self.close_popup();
                }
                Some(LintCardAction::AddToDictionary) => {
                    if let Some((lint, source_text)) =
                        self.lints().get(index).map(|actionable_lint| {
                            (
                                actionable_lint.lint.clone(),
                                actionable_lint.source_text.clone(),
                            )
                        })
                    {
                        let document = Document::new_markdown_default_curated(&source_text);
                        let word = lint.get_str(document.get_source());
                        (self.add_to_dictionary)(&word);
                    }

                    self.close_popup();
                }
                Some(LintCardAction::DisableRule) => {
                    if let Some(rule_name) = self
                        .lints()
                        .get(index)
                        .map(|actionable_lint| actionable_lint.rule_name.clone())
                    {
                        (self.disable_rule)(&rule_name);
                    }

                    self.close_popup();
                }
                None => {}
            }
        }
    }
}

/// Converts accessibility rectangles into egui rectangles so rendering and hit-testing use the same
/// screen-space geometry.
fn rect_bounds(rect: &Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(rect.x as f32, rect.y as f32),
        egui::vec2(rect.width as f32, rect.height as f32),
    )
}

fn lint_kind_color32(lint_kind: LintKind) -> egui::Color32 {
    let color = lint_kind_color(lint_kind);

    egui::Color32::from_rgb(color.r, color.g, color.b)
}
