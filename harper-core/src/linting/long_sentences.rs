use super::{Lint, LintKind, Linter, Suggestion};
use crate::TokenStringExt;
use crate::{Document, Span};

/// Detect and warn that the sentence is too long.
#[derive(Debug, Clone, Copy, Default)]
pub struct LongSentences;

impl Linter for LongSentences {
    fn lint(&mut self, document: &Document) -> Vec<Lint> {
        let mut output = Vec::new();

        for sentence in document.iter_sentences() {
            let word_count = sentence.iter_words().count();

            if word_count > 40 {
                let span = Span::new(
                    sentence.first_word().unwrap().span.start,
                    sentence.last().unwrap().span.end,
                );

                output.push(Lint {
                    span,
                    lint_kind: LintKind::Readability,
                    message: format!("This sentence is {word_count} words long."),
                    suggestions: split_suggestion(&document.get_span_content_str(&span))
                        .into_iter()
                        .collect(),
                    ..Default::default()
                })
            }
        }

        output
    }

    fn description(&self) -> &'static str {
        "This rule looks for run-on sentences, which can make your work harder to grok."
    }
}

/// The conjunctions a run-on can be cut at, and whether the conjunction survives the cut.
///
/// `and` is dropped because the second half stands on its own without it. The others carry a
/// contrast or a consequence that the reader loses if it goes, so they stay on as the opening word.
const SPLIT_POINTS: [(&str, bool); 4] = [
    (", and ", false),
    (", but ", true),
    (", so ", true),
    (", yet ", true),
];

/// The fewest words either half may be left with.
///
/// Cutting near an end trades one long sentence for a long one and a stub, which reads worse than
/// what it replaced.
const MIN_HALF_WORDS: usize = 8;

/// Rewrites a run-on as two sentences, when there is an obvious place to cut it.
///
/// Most of what makes a sentence too long cannot be fixed by rule — it needs the writer. A clause
/// joined on with a comma and a conjunction is the exception: it is already a sentence, and only
/// the join has to go. Where there is no such join this offers nothing rather than guessing.
fn split_suggestion(sentence: &str) -> Option<Suggestion> {
    // The fix replaces the whole sentence, which is applied by typing it back. A line break typed
    // into a chat box or a search field sends what is there instead of starting a new line, and a
    // tab moves to the next control. A sentence carrying either is left alone rather than risking
    // that.
    if sentence.chars().any(char::is_control) {
        return None;
    }

    let (at, separator, keep_conjunction) = SPLIT_POINTS
        .iter()
        .flat_map(|(separator, keep)| {
            sentence
                .match_indices(separator)
                .map(move |(at, _)| (at, *separator, *keep))
        })
        .filter(|(at, separator, _)| {
            words_in(&sentence[..*at]) >= MIN_HALF_WORDS
                && words_in(&sentence[at + separator.len()..]) >= MIN_HALF_WORDS
        })
        // Cutting nearest the middle leaves the two halves closest in length.
        .min_by_key(|(at, _, _)| (*at as isize - sentence.len() as isize / 2).abs())?;

    let before = &sentence[..at];
    let after = &sentence[at + separator.len()..];

    let rewritten = if keep_conjunction {
        // `separator` is ", but " and the like, so this is the conjunction on its own.
        let conjunction = separator.trim_matches([',', ' ']);

        format!("{before}. {} {after}", capitalized(conjunction))
    } else {
        format!("{before}. {}", capitalized(after))
    };

    Some(Suggestion::ReplaceWith(rewritten.chars().collect()))
}

fn words_in(text: &str) -> usize {
    text.split_whitespace().count()
}

/// `text` with its first character in upper case, so the new sentence opens like one.
fn capitalized(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::LongSentences;
    use crate::linting::tests::{assert_lint_count, assert_suggestion_result};

    const BEFORE: &str = "The release went out on Friday afternoon without any real trouble at all from the build servers or from the deployment scripts that the team had rewritten the week before";
    const AFTER: &str =
        "the team spent the rest of the day watching the dashboards for errors that never arrived";

    #[test]
    fn splits_a_run_on_at_its_middle_conjunction() {
        assert_suggestion_result(
            &format!("{BEFORE}, and {AFTER}."),
            LongSentences,
            &format!(
                "{BEFORE}. The team spent the rest of the day watching the dashboards for errors that never arrived."
            ),
        );
    }

    #[test]
    fn keeps_the_conjunction_that_carries_the_contrast() {
        assert_suggestion_result(
            &format!("{BEFORE}, but {AFTER}."),
            LongSentences,
            &format!("{BEFORE}. But {AFTER}."),
        );
    }

    #[test]
    fn offers_nothing_when_there_is_nowhere_obvious_to_cut() {
        assert_lint_count(&format!("{BEFORE} while {AFTER}."), LongSentences, 1);
    }
}
