//! The one place the production/test cut is written down.
//!
//! Ten guards across four modules slice a file at its test module and
//! scan what is above it. Each of them spelled the anchor out and each
//! fell back to `src.len()` — the whole file — when it did not match,
//! which is the wrong direction in both kinds of guard: one that forbids
//! a phrase starts reading the test module's own prose about that phrase,
//! and one that requires a phrase is satisfied by a test that merely
//! mentions it.
//!
//! It has already gone wrong once, and silently. A guard whose anchor was
//! `#[cfg(test)]` alone stopped at an attribute on a method 470 lines
//! above `mod tests`, so every guard in that file scanned its first
//! quarter, passed, and did so for a whole round over a live defect.
//!
//! So: one anchor, one definition, and a miss is a failed test naming the
//! file rather than a wider scan nobody sees.
//!
//! It also holds [`sentences_authorising_a_deletion`], for the same
//! reason: the shipped documents and the refusal messages both have to
//! be held to the deletion gate, and only the documents were.

/// Everything above `#[cfg(test)] mod tests {` — the module's shipped
/// code.
///
/// `file` is only for the panic message; `src` is what gets cut. The
/// anchor is the two lines TOGETHER on purpose: `#[cfg(test)]` on its own
/// matches the first such attribute anywhere in the file, and
/// `idempotency.rs` carries one on a method.
pub(crate) fn production_source<'a>(file: &str, src: &'a str) -> &'a str {
    let cut = src
        .find(concat!("#[cfg(test)]\n", "mod tests {"))
        .unwrap_or_else(|| {
            panic!(
                "{file}: no `#[cfg(test)] mod tests {{` to cut at. Every guard that scans this \
                 file is now reading text it was never meant to see — fix the anchor rather than \
                 the guards"
            )
        });
    &src[..cut]
}

/// Sentences in `text` that hand somebody permission to delete the
/// record, without naming the verdict that permits it.
///
/// SENTENCE-scoped, not block-scoped: a "do not delete" three bullets
/// away in an adjacent branch of the same procedure satisfied a
/// block-wide search while the sentence in front of the reader said the
/// opposite. Whitespace is collapsed first — these are hard-wrapped
/// documents and multi-line format strings, so every phrase worth
/// looking for straddles a line break somewhere, and the first version
/// of this scan matched nothing at all and looked like a pass.
///
/// `authorises` and `hedges` are the CALLER's, because the two surfaces
/// are held to different rules and saying so is the point. A procedure
/// may give conditional permission — the runbook's gate does, in a
/// sentence naming the verdict — so its list is narrow and tuned to the
/// copies that got it wrong. A refusal message may not: it has no room
/// for a condition, and the wrapper around it forbids the deletion two
/// sentences later, so any imperative at all is a contradiction the
/// operator reads as a whole.
pub(crate) fn sentences_authorising_a_deletion(
    text: &str,
    authorises: &[&str],
    hedges: &[&str],
) -> Vec<String> {
    let lower = text
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    lower
        .split(". ")
        .filter(|s| authorises.iter().any(|p| s.contains(p)))
        .filter(|s| !hedges.iter().any(|h| s.contains(h)))
        .map(|s| s.trim().to_string())
        .collect()
}
