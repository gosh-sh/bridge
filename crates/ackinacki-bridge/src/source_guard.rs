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

/// `text` cut into clauses: lowercased, markdown emphasis stripped,
/// whitespace collapsed WITHIN each clause.
///
/// Split before collapsing, and on every terminator rather than `". "`
/// alone. Both of those were wrong and both mattered. A dropped full
/// stop glued an order and the prohibition after it into one "sentence",
/// and a hedge scan then found the prohibition and exempted the order —
/// the check zeroed itself on exactly the edit that makes it necessary.
/// A line break did the same, because these are hard-wrapped documents
/// and multi-line `format!` strings where any phrase worth looking for
/// straddles one somewhere.
///
/// Splitting on `.` also cuts `burn.rs` and `0.1.0` in half. That is the
/// conservative direction: it only makes clauses shorter, and a shorter
/// clause needs its hedge nearer.
///
/// A SOFT WRAP is not a boundary, though, and treating it as one was the
/// first version of this: these documents are hard-wrapped at 72
/// columns, so "said only in the second case\n  delete the record" is
/// one clause that happens to cross a line, and cutting there took the
/// condition off the permission. A newline is a boundary only where the
/// next line starts a new block — a list item, a table row, a heading, a
/// quote, or a blank line.
pub(crate) fn clauses(text: &str) -> Vec<String> {
    let mut joined = String::with_capacity(text.len());
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        joined.push_str(line);
        let next = lines.get(i + 1).map(|l| l.trim_start()).unwrap_or("");
        let starts_a_block = next.is_empty()
            || next.starts_with('-')
            || next.starts_with('*')
            || next.starts_with('|')
            || next.starts_with('#')
            || next.starts_with('>')
            || next.starts_with("```")
            || next.chars().next().is_some_and(|c| c.is_ascii_digit());
        joined.push(if starts_a_block { '\n' } else { ' ' });
    }
    joined
        .split(['.', '!', '?', ';', ':', '\n'])
        .map(|piece: &str| {
            piece
                .to_ascii_lowercase()
                .replace(['*', '`', '_'], "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|c| !c.is_empty())
        .collect()
}

/// Clauses in `text` that hand somebody permission to delete the record,
/// without naming the verdict that permits it.
///
/// CLAUSE-scoped, not sentence- and not block-scoped. A "do not delete"
/// three bullets away in an adjacent branch of the same procedure
/// satisfied a block-wide search while the sentence in front of the
/// reader said the opposite; a lost full stop did the same one level
/// down. A hedge earns an exemption only in the clause it is in.
///
/// `authorises` and `hedges` are the CALLER's, because the two surfaces
/// are held to different rules and saying so is the point. A procedure
/// may give conditional permission — the runbook's gate does, in a
/// clause naming the verdict. A refusal message may not: it has no room
/// for a condition, and the wrapper around it forbids the deletion two
/// sentences later, so any imperative at all is a contradiction the
/// operator reads as a whole.
/// The hedge also has to come BEFORE the order it qualifies. Clause
/// scoping alone was not enough: "Delete the record and re-run do not
/// delete it if another run holds it" — one dropped full stop — is a
/// single clause carrying both, and a hedge scan that only asks
/// "contains" found the prohibition and exempted the order in front of
/// it. A reader acts on the first half.
///
/// "do not delete the record" passes, because there the hedge is where a
/// reader meets it first.
pub(crate) fn sentences_authorising_a_deletion(
    text: &str,
    authorises: &[&str],
    hedges: &[&str],
) -> Vec<String> {
    clauses(text)
        .into_iter()
        .filter(|s| {
            let Some(order) = authorises.iter().filter_map(|p| s.find(p)).min() else {
                return false;
            };
            !hedges
                .iter()
                .filter_map(|h| s.find(h))
                .any(|hedge| hedge <= order)
        })
        .collect()
}
