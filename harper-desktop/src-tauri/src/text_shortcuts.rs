//! User-defined abbreviations that stand in for longer text.
//!
//! People build up their own shorthand while typing — `wo` for `without`, `ok` for `okay`. Harper
//! already knows how to show a correction and apply it in one click, so a shortcut is expressed as
//! a lint rather than as a keystroke that rewrites itself. That keeps the expansion visible and
//! refusable, and it means shortcuts are swept up by "fix everything" along with the rest.

use std::collections::BTreeMap;

use harper_core::linting::{Lint, LintKind, Linter, Suggestion};
use harper_core::{Document, TokenStringExt};

/// Expands the abbreviations a user has defined in settings.
#[derive(Debug, Clone, Default)]
pub struct TextShortcuts {
    /// Abbreviation to expansion, with the abbreviations stored lowercase so lookup ignores case.
    shortcuts: BTreeMap<String, String>,
}

impl TextShortcuts {
    pub fn new(shortcuts: &BTreeMap<String, String>) -> Self {
        Self {
            shortcuts: shortcuts
                .iter()
                .filter(|(abbreviation, expansion)| {
                    !abbreviation.trim().is_empty() && !expansion.trim().is_empty()
                })
                .map(|(abbreviation, expansion)| {
                    (
                        abbreviation.trim().to_lowercase(),
                        expansion.trim().to_string(),
                    )
                })
                .collect(),
        }
    }
}

impl Linter for TextShortcuts {
    fn lint(&mut self, document: &Document) -> Vec<Lint> {
        if self.shortcuts.is_empty() {
            return Vec::new();
        }

        document
            .iter_words()
            .filter_map(|word| {
                let span = word.span;
                let written = document.get_span_content_str(&span);
                let expansion = self.shortcuts.get(&written.to_lowercase())?;

                // A shortcut typed with a capital opens a sentence, so the expansion has to as
                // well, or expanding it would introduce a mistake of its own.
                let expansion = match written.chars().next() {
                    Some(first) if first.is_uppercase() => capitalized(expansion),
                    _ => expansion.clone(),
                };

                if expansion == written {
                    return None;
                }

                Some(Lint {
                    span,
                    lint_kind: LintKind::Style,
                    message: format!("`{written}` is your shortcut for `{expansion}`."),
                    suggestions: vec![Suggestion::ReplaceWith(expansion.chars().collect())],
                    ..Default::default()
                })
            })
            .collect()
    }

    fn description(&self) -> &'static str {
        "Expands the shortcuts you defined in settings into the text they stand for."
    }
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}
