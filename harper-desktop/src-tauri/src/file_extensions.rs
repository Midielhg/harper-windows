//! Leaves the tail of a filename alone.
//!
//! `.exe`, `.jpg` and `.rs` are not words anyone misspelled — they are how the file is named, and
//! nothing the writer can act on. Harper's dictionary does not hold them, so every one that goes
//! past is offered a correction that is always wrong, which is how a page of filenames turns into a
//! page of highlights worth nothing.

use harper_core::linting::Lint;

/// The longest run of characters after a dot still taken for an extension.
///
/// Real extensions are short. Allowing more would start swallowing ordinary words that follow a
/// full stop someone forgot to put a space after.
const MAX_EXTENSION: usize = 5;

/// Drops the lints that sit on what follows a dot in a filename.
pub fn remove_file_extension_lints(lints: &mut Vec<Lint>, source: &[char]) {
    lints.retain(|lint| !is_file_extension(lint.span.start, lint.span.end, source));
}

fn is_file_extension(start: usize, end: usize, source: &[char]) -> bool {
    if start == 0 || end > source.len() || end <= start || source[start - 1] != '.' {
        return false;
    }

    let extension = &source[start..end];

    if extension.len() > MAX_EXTENSION || !extension.iter().all(|c| c.is_alphanumeric()) {
        return false;
    }

    // A word that goes on past the span is not the whole extension, so this is something else
    // sitting next to a dot.
    if source.get(end).is_some_and(|c| c.is_alphanumeric()) {
        return false;
    }

    // `home.Then` is a missing space between two sentences, not a file: the capital says a word
    // was meant. Extensions are written in one case throughout, so only those are passed over.
    !is_capitalized_word(extension)
}

fn is_capitalized_word(extension: &[char]) -> bool {
    extension[0].is_uppercase() && extension[1..].iter().all(|c| c.is_lowercase())
}

#[cfg(test)]
mod tests {
    use super::is_file_extension;

    /// Whether the run of characters marked off by `[` and `]` in `text` is taken for an extension.
    fn marked(text: &str) -> bool {
        let start = text.find('[').unwrap();
        let end = text.find(']').unwrap() - 1;
        let source: Vec<char> = text.chars().filter(|c| !matches!(c, '[' | ']')).collect();

        is_file_extension(start, end, &source)
    }

    #[test]
    fn skips_the_tail_of_a_filename() {
        assert!(marked("open harper.[exe] now"));
        assert!(marked("a photo.[jpg]"));
        assert!(marked("the file is main.[rs]."));
        assert!(marked("shouting about README.[MD]"));
    }

    #[test]
    fn keeps_words_that_merely_follow_a_dot() {
        assert!(!marked("I went home.[Then] we left"));
        assert!(!marked("done. [Exe] is a word here"));
        assert!(!marked("a very long.[extension] is not one"));
    }

    #[test]
    fn keeps_a_word_that_opens_a_document() {
        assert!(!marked("[Hello] there"));
    }
}
