use crate::color::Color;
use harper_core::linting::{Lint, Suggestion};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Constructs a copy of the Rect, with the given amount of padding around the original.
    pub fn with_padding(&self, n: f64) -> Self {
        Self {
            x: self.x - n,
            y: self.y - n,
            width: self.width + 2.0 * n,
            height: self.height + 2.0 * n,
        }
    }

    /// Similar to [`Self::with_padding`], but preconfigured to a value that is forgiving to the
    /// user.
    pub fn with_friendly_padding(&self) -> Self {
        self.with_padding(2.0)
    }

    /// The same rectangle shifted by `(dx, dy)`.
    pub fn translated(&self, dx: f64, dy: f64) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColoredRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub color: Color,
}

/// A Harper lint paired with the screen-space rectangle where it should be rendered.
///
/// Harper owns the lint details and accessibility owns the geometry; this type keeps both pieces
/// together without duplicating lint fields into highlighter-specific strings.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionedLint {
    pub rect: Rect,
    pub lint: Lint,
}

/// A Harper lint paired with geometry, source context, and an OS-specific suggestion action.
///
/// The highlighter owns rendering and interaction, but the OS broker is the only layer that knows how
/// to mutate the underlying text element. Storing a one-shot callback here keeps those responsibilities
/// connected without teaching rendering code about Accessibility APIs.
pub struct ActionableLint {
    pub rect: Rect,
    pub rule_name: String,
    pub lint: Lint,
    pub source_text: String,
    apply_suggestion: Option<Box<dyn FnOnce(Suggestion)>>,
}

impl PositionedLint {
    pub fn new(rect: Rect, lint: Lint) -> Self {
        Self { rect, lint }
    }
}

impl ActionableLint {
    pub fn new(
        rect: Rect,
        rule_name: String,
        lint: Lint,
        source_text: String,
        apply_suggestion: impl FnOnce(Suggestion) + 'static,
    ) -> Self {
        Self {
            rect,
            rule_name,
            lint,
            source_text,
            apply_suggestion: Some(Box::new(apply_suggestion)),
        }
    }

    pub fn apply_suggestion(&mut self, suggestion: Suggestion) {
        if let Some(apply_suggestion) = self.apply_suggestion.take() {
            apply_suggestion(suggestion);
        }
    }
}

impl ColoredRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64, color: Color) -> Self {
        Self {
            x,
            y,
            width,
            height,
            color,
        }
    }
}

/// What the user has selected, and where that selection begins on screen.
///
/// The review panel is opened from the selection rather than appearing on its own, so this carries
/// both the part of the text to review and the point to hang the button off.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// Where the selection starts, in characters from the start of the field.
    pub start: usize,
    /// How many characters are selected. Never zero: a caret is not a selection.
    pub len: usize,
    /// The left edge of the selection's first line, with its height.
    pub anchor: Rect,
}

impl Selection {
    /// Whether `span` lies within the selection.
    pub fn contains(&self, span: harper_core::Span<char>) -> bool {
        span.start >= self.start && span.end <= self.start + self.len
    }
}

#[cfg(test)]
mod tests {
    use super::Rect;

    #[test]
    fn moves_a_rect_without_resizing_it() {
        let moved = Rect::new(10.0, 20.0, 30.0, 40.0).translated(-4.0, 7.5);

        assert_eq!(moved, Rect::new(6.0, 27.5, 30.0, 40.0));
    }
}
