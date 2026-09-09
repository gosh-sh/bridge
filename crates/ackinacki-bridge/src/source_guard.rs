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
///
/// WHOLE LINES, both of them, and that is the whole of the matching
/// rule. Two comments among the imports —
///
/// ```text
/// // #[cfg(test)]
/// // mod tests {
/// ```
///
/// — were an anchor when the attribute was looked for as a substring,
/// and cut every guard's view of the file down to its first forty lines.
/// What that costs is silent and one-directional: a guard that REQUIRES a
/// phrase fails loudly, so the file always has one of those to notice it,
/// but a guard that FORBIDS one passes on a scan of nothing. Three of
/// `orchestrator.rs`'s guards are of the forbidding kind.
///
/// The declaration's VISIBILITY is not part of it. `preflight`'s test
/// module is `pub(crate)`, because it owns the only answer set that gets
/// `check_bridge_deploy` to `Ok` and `test_chain` builds a fake world out
/// of it; a second copy of that selector table is a second thing to
/// drift. So the prefix is stripped and the rest of the line has to be
/// the declaration exactly — which is narrower than the "ends with"
/// match that replaced the substring one, and does not ask anyone to
/// spell a module private to keep a guard working.
///
/// ONE anchor per file, and a second is an ambiguity rather than a first
/// match. That is the plausibility check on the cut: every way this has
/// gone wrong so far has been a second thing in the file that looked like
/// the anchor, and taking the first match is what made the wrong one win
/// silently.
pub(crate) fn production_source<'a>(file: &str, src: &'a str) -> &'a str {
    const ATTRIBUTE: &str = concat!("#[cfg", "(test)]");
    const DECLARATION: &str = concat!("mod ", "tests {");

    let mut anchors = Vec::new();
    let mut at = 0;
    for line in src.lines() {
        let start = at;
        at += line.len() + 1;
        if line != ATTRIBUTE {
            continue;
        }
        let next = src[at..].lines().next().unwrap_or("");
        let declaration = next
            .strip_prefix("pub(crate) ")
            .or_else(|| next.strip_prefix("pub "))
            .unwrap_or(next);
        if declaration == DECLARATION {
            anchors.push(start);
        }
    }
    match anchors[..] {
        [cut] => &src[..cut],
        [] => panic!(
            "{file}: no `{ATTRIBUTE}` line followed by a `{DECLARATION}` line to cut at. Every \
             guard that scans this file is now reading text it was never meant to see — fix the \
             anchor rather than the guards"
        ),
        _ => panic!(
            "{file}: {} test-module anchors, and the cut would be at the first. Whichever of them \
             is not the module every guard in this file means, it is deciding how much of the \
             file they read",
            anchors.len(),
        ),
    }
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

/// Every way this tree writes "delete the state record", as the VERB and
/// its object.
///
/// Shared, and that is the whole point of it being here. Two copies of
/// this list existed — one for the shipped documents, one for the
/// refusal messages — and the comment over the second said it was
/// STRICTER than the first. It was true when it was written and false
/// two commits later: the document list grew `prune the record`, `prune
/// the state file` and `delete that state file`, the refusal list did
/// not, and the relation the comment asserted was expressed nowhere, so
/// nothing went red. `Prune the record and re-run.`, planted in the one
/// refusal about a record that cannot say whether a burn is on the wire,
/// passed the gate built to forbid exactly that sentence — measured.
///
/// `prune` is not a hypothetical word here: this repository shipped
/// "records older than N hours with no `an_tx_hash` are safe to prune",
/// and the changelog carries the entry that took it back out.
///
/// This is the FLOOR. A surface may hold itself to more — the refusal
/// gate adds one — but no surface may know fewer orders than the
/// documents are held to.
pub(crate) const ORDERS_A_DELETION: [&str; 8] = [
    "delete the record",
    "delete that record",
    "delete the state file",
    "delete that state file",
    "remove the record",
    "prune the record",
    "prune the state file",
    "safe to delete",
];

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
/// are held to different rules and saying so is the point.
///
/// A procedure may give conditional permission — the runbook's gate
/// does, in a clause naming the verdict. So may a refusal, and the
/// stricter rule this used to state — "a refusal message may not" — is
/// false of this codebase: `ReservationInFlight`'s third step is a
/// refusal whose whole job is to hand out a conditional verdict, and
/// `BurnPermit::issue` names which of three cases it is and what each
/// one permits.
///
/// What the STAGE-1 re-badges are held to is narrower, and for a reason
/// that is about them rather than about refusals in general: their
/// population is precisely the one that cannot be resolved locally — no
/// hash, no liveness verdict, nothing on the machine that settles it —
/// so a condition stated there could not be checked by the operator
/// reading it. That is why `orchestrator`'s list is the verb and its
/// object with prohibitions as the only hedges.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A file, assembled from lines so that no line of this test's own
    /// source is an anchor. `production_source` matches whole lines at
    /// column zero, and these fixtures are indented string literals.
    fn file(lines: &[&str]) -> String {
        lines.join("\n")
    }

    const ATTR: &str = concat!("#[cfg", "(test)]");
    const DECL: &str = concat!("mod ", "tests {");

    #[test]
    fn the_cut_is_the_test_module_and_nothing_above_it() {
        let src = file(&[
            "use std::fs;",
            "",
            "fn ship() {}",
            "",
            ATTR,
            DECL,
            "    fn t() {}",
            "}",
        ]);
        assert_eq!(
            production_source("x.rs", &src),
            "use std::fs;\n\nfn ship() {}\n\n"
        );
    }

    #[test]
    fn a_commented_out_anchor_is_not_an_anchor() {
        // The round-17 finding, and the reason both lines are matched
        // whole. As a substring search this cut the file at line 1 and
        // every forbidding guard in it passed on an empty scan.
        let src = file(&[
            "use std::fs;",
            &format!("// {ATTR}"),
            &format!("// {DECL}"),
            "",
            "fn ship() {}",
            "",
            ATTR,
            DECL,
            "    fn t() {}",
            "}",
        ]);
        let cut = production_source("x.rs", &src);
        assert!(
            cut.contains("fn ship() {}"),
            "the cut stopped at the comments: {cut:?}"
        );
        assert!(!cut.contains("fn t()"), "{cut:?}");
    }

    #[test]
    fn an_attribute_on_an_item_is_walked_past() {
        // Round 11's decoy, and `idempotency.rs`'s real one: an attribute
        // on a method 470 lines above the test module. Indented here, as
        // it is there.
        let src = file(&[
            "impl Status {",
            &format!("    {ATTR}"),
            "    fn only_tests_call_this() {}",
            "}",
            "",
            "fn ship() {}",
            "",
            ATTR,
            DECL,
            "}",
        ]);
        assert!(production_source("x.rs", &src).contains("fn ship() {}"));
    }

    #[test]
    fn a_module_the_crate_can_see_is_still_an_anchor() {
        // `preflight`'s is `pub(crate)`, so `test_chain` can build a fake
        // world out of its answer set.
        for visibility in ["", "pub(crate) ", "pub "] {
            let src = file(&[
                "fn ship() {}",
                "",
                ATTR,
                &format!("{visibility}{DECL}"),
                "}",
            ]);
            assert_eq!(
                production_source("x.rs", &src),
                "fn ship() {}\n\n",
                "visibility `{visibility}` should not move the cut",
            );
        }
    }

    #[test]
    #[should_panic(expected = "somebodys_module.rs")]
    fn a_file_with_no_test_module_names_itself_rather_than_widening() {
        production_source("somebodys_module.rs", "fn ship() {}\n");
    }

    #[test]
    #[should_panic(expected = "2 test-module anchors")]
    fn a_second_anchor_is_an_ambiguity_rather_than_a_first_match() {
        let src = file(&[ATTR, DECL, "}", "", "fn ship() {}", "", ATTR, DECL, "}"]);
        production_source("x.rs", &src);
    }

    #[test]
    #[should_panic(expected = "renamed.rs")]
    fn a_renamed_test_module_is_loud() {
        // The other half of "one anchor": the guards' view of a file is
        // never allowed to widen quietly, so a module called anything
        // else is a failed test naming the file.
        let src = file(&["fn ship() {}", "", ATTR, "mod checks {", "}"]);
        production_source("renamed.rs", &src);
    }

    #[test]
    fn every_file_the_guards_cut_has_exactly_one_anchor() {
        // The check on the real files, rather than on fixtures: each of
        // these is cut by at least one guard, and a cut that lands in the
        // wrong place is silent in exactly the guards that forbid things.
        for (name, src) in [
            ("orchestrator.rs", include_str!("orchestrator.rs")),
            ("idempotency.rs", include_str!("idempotency.rs")),
            ("burn.rs", include_str!("burn.rs")),
            ("preflight.rs", include_str!("preflight.rs")),
        ] {
            let cut = production_source(name, src);
            assert!(
                !cut.is_empty() && cut.len() < src.len(),
                "{name}: the cut is the whole file or none of it",
            );
            // And what is above the cut is code rather than a header: the
            // failure this test exists for cut `orchestrator.rs` at its
            // fortieth line, which is still inside the imports.
            assert!(
                cut.lines()
                    .filter(|l| l.starts_with("fn ") || l.starts_with("pub "))
                    .count()
                    > 1,
                "{name}: the production side has almost nothing in it, which is what a cut at a \
                 decoy anchor looks like",
            );
        }
    }
}
