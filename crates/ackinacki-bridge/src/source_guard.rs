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

    // Offsets taken from `split('\n')` rather than `lines()`, and the
    // pair read off a window rather than by slicing back into `src`.
    // Both were bugs of one byte. `lines()` strips a trailing `\r`, so a
    // CRLF file drifted the running offset by a byte per line; and
    // `src[at..]` where the attribute is the last line of a file with no
    // final newline is out of range, which panicked inside the guard
    // instead of naming the file it was cutting.
    let mut numbered: Vec<(usize, &str)> = Vec::new();
    let mut at = 0;
    for raw in src.split('\n') {
        numbered.push((at, raw.strip_suffix('\r').unwrap_or(raw)));
        at += raw.len() + 1;
    }
    let anchors: Vec<usize> = numbered
        .windows(2)
        .filter(|w| w[0].1 == ATTRIBUTE)
        .filter(|w| {
            let next = w[1].1;
            let declaration = next
                .strip_prefix("pub(crate) ")
                .or_else(|| next.strip_prefix("pub "))
                .unwrap_or(next);
            declaration == DECLARATION
        })
        .map(|w| w[0].0)
        .collect();
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

/// The prohibitions that hedge one, and the FLOOR under every hedge
/// list for the same reason the orders are shared.
///
/// The refusal gate's comment said it was stricter than the document
/// gate "in the hedges and only there", and the two hedge sets merely
/// INTERSECTED: `never delete` and `must not delete` exempted a clause
/// in a refusal and not in a document, so on those two spellings the
/// refusal gate was the softer of the pair — the same shape as the
/// verbs, one field over. A refusal may be stricter than a document by
/// having FEWER hedges; it may not have hedges the document does not.
pub(crate) const PROHIBITS_A_DELETION: [&str; 4] = [
    "do not delete",
    "never delete",
    "must not delete",
    "not to delete",
];

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
/// That sentence is not itself caught by this list, and saying it was
/// would be the second spec disagreeing with the first: what is listed
/// is the verb with the RECORD as its object, and "safe to prune" bare
/// is not. It is left out on purpose. `Failed` records carry a hash by
/// construction — the sole writer of `Failed` is the post-burn revert —
/// so pruning one is not this rule's business, and the README says
/// exactly that about them, twice. A list that flagged "safe to prune"
/// would be flagging the one deletion this pipeline does authorise.
///
/// This is the FLOOR, and the documents are held to exactly it. What a
/// refusal adds is [`A_REFUSAL_MAY_NOT_NAME_ANOTHER_FILE`]; neither gate
/// may know fewer orders than this.
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

/// What a DOCUMENT may say instead of a prohibition.
///
/// A named constant for the same reason [`ORDERS_A_DELETION`] is one,
/// and it was written inline for one round: an entry added beside `only
/// if` — `if none` — let "If none landed, prune the record and re-run."
/// back into the shipped runbook with the suite green, because the only
/// thing watching this list was a superset relation against the
/// prohibitions.
///
/// Every entry marks the permission as CONDITIONAL, in the clause that
/// grants it. A refusal has none of these: it has no room for a
/// condition and already carries the prohibition.
const A_DOCUMENT_MAY_CONDITION_ON: [&str; 13] = [
    // References to the three-verdict gate itself.
    "one of three",
    "three-verdict",
    "three verdicts",
    "three liveness",
    "three cases",
    // Conditionals.
    "only if",
    "only then",
    "only in the second case",
    "only sometimes",
    // A prohibition that forbids without naming the verb.
    "delete nothing",
    // Statements that the verdict may not exist.
    "could not be determined",
    "does not always",
    "never does",
];

/// What a REFUSAL may not say and a document may.
///
/// In a refusal the only file in scope is the record it names, so
/// "delete that file" is an order there and ordinary housekeeping in a
/// document, which says it about a key file, a log or a stale artifact.
///
/// A named constant with a declared length, like the three beside it:
/// the length is the second pin, so an entry cannot be added or dropped
/// without editing a number the relation test also reads.
const A_REFUSAL_MAY_NOT_NAME_ANOTHER_FILE: [&str; 1] = ["delete that file"];

/// What exempts a clause in a SHIPPED DOCUMENT: the prohibitions, plus
/// the conditionals a document is allowed to grant permission under.
fn what_exempts_a_clause_in_a_document() -> Vec<&'static str> {
    let mut hedges = PROHIBITS_A_DELETION.to_vec();
    hedges.extend(A_DOCUMENT_MAY_CONDITION_ON);
    hedges
}

/// Ways of ordering a deletion a REFUSAL may not contain: the floor,
/// plus the one a refusal alone is held to.
fn what_a_refusal_may_not_order() -> Vec<&'static str> {
    let mut orders = ORDERS_A_DELETION.to_vec();
    orders.extend(A_REFUSAL_MAY_NOT_NAME_ANOTHER_FILE);
    orders
}

/// Clauses in a SHIPPED DOCUMENT that order a deletion without naming
/// the verdict that permits it.
///
/// PRIVATE, and that is this round's fix rather than a tidy-up. It was
/// `pub(crate)`, and calling it from a REFUSAL gate — one word at
/// `orchestrator.rs`'s call site — handed that gate thirteen document
/// hedges: a live exit-10 refusal with `Only if you have confirmed no
/// burn landed, delete the record.` planted in it stayed green. There
/// was nothing wrong with the function; the hole was that a refusal
/// site could name it. It cannot now, and the gate that uses it lives
/// in this module's own tests.
fn orders_a_document_may_not_give(text: &str) -> Vec<String> {
    sentences_authorising_a_deletion(
        text,
        &ORDERS_A_DELETION,
        &what_exempts_a_clause_in_a_document(),
    )
}

/// Clauses in a REFUSAL THIS CLI RAISES that order a deletion.
///
/// Stricter than the document rule in both directions, and both are
/// checked by `a_refusal_is_stricter_than_a_document_in_both_directions`:
/// one more order, and none of the conditionals — a refusal has no room
/// for a condition and already carries the prohibition.
///
/// The only one of the two that leaves this module. A refusal gate has
/// exactly one function it can call, so there is no surface to pass and
/// no second name to reach for.
pub(crate) fn orders_a_refusal_may_not_give(text: &str) -> Vec<String> {
    sentences_authorising_a_deletion(text, &what_a_refusal_may_not_order(), &PROHIBITS_A_DELETION)
}

/// Whether `text` acknowledges that the verdict permitting a deletion
/// may not exist.
///
/// The documents' other trigger asks this of a whole block: a passage
/// that sends a reader to the exit-3 refusal FOR the liveness answer has
/// to admit the answer can be missing. Same vocabulary as a document's
/// hedges, asked block-wide rather than per clause, and private for the
/// same reason its neighbour is.
fn a_document_admits_the_verdict_may_be_missing(text: &str) -> bool {
    what_exempts_a_clause_in_a_document()
        .iter()
        .any(|h| text.contains(h))
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
fn sentences_authorising_a_deletion(
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

    /// Either gate, as a value, so a case can name which one it is.
    type Gate = fn(&str) -> Vec<String>;

    #[test]
    fn a_refusal_is_stricter_than_a_document_in_both_directions() {
        // In THIS module, because everything it reads is private to it.
        // It lived in `orchestrator` while the vocabularies were
        // arguments, and it could then only compare the two arms with
        // each other — which is why swapping a call site's surface was
        // green, and why a list that shrank for both surfaces was too.
        //
        // The shapes first. Each gate reads two named lists and this is
        // what they are, so a gate handed the other surface's list has
        // to change a name here too.
        assert_eq!(
            what_a_refusal_may_not_order().len(),
            ORDERS_A_DELETION.len() + A_REFUSAL_MAY_NOT_NAME_ANOTHER_FILE.len(),
            "a refusal is held to the whole floor plus what only a refusal may not say",
        );
        assert_eq!(
            what_exempts_a_clause_in_a_document().len(),
            PROHIBITS_A_DELETION.len() + A_DOCUMENT_MAY_CONDITION_ON.len(),
            "a document is exempted by the prohibitions and by the conditionals it is allowed to \
             use, and by nothing else",
        );

        // Then the relation, in both directions.
        for order in ORDERS_A_DELETION {
            assert!(
                what_a_refusal_may_not_order().contains(&order),
                "the documents are held to `{order}` and the refusals are not. A refusal may be \
                 stricter than a document and may not be laxer",
            );
        }
        for hedge in PROHIBITS_A_DELETION {
            assert!(
                what_exempts_a_clause_in_a_document().contains(&hedge),
                "a refusal is exempted by `{hedge}` and a document is not, which makes it the \
                 laxer of the two",
            );
        }
    }

    #[test]
    fn the_sentences_this_gate_exists_for_are_caught_and_the_others_are_not() {
        // The behaviour, on fixed text, so that a vocabulary edit is
        // measured by what it lets through rather than by its shape.
        // Every ORDER here is one that has actually been written into
        // this tree or planted in it during a review.
        for (which, gate, text) in [
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "If none landed, prune the record and re-run.",
            ),
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "Delete the record and re-run.",
            ),
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "The stale files are safe to delete.",
            ),
            (
                "a refusal",
                orders_a_refusal_may_not_give as Gate,
                "If it has clearly died, prune the record and re-run.",
            ),
            (
                "a refusal",
                orders_a_refusal_may_not_give as Gate,
                "Delete that record now and re-run.",
            ),
            // Refusal-only: in a refusal the only file in scope is the
            // record it names.
            (
                "a refusal",
                orders_a_refusal_may_not_give as Gate,
                "Delete that file and try again.",
            ),
            // The measured attack of round 21, and the case that pins
            // the refusal gate's HEDGES behaviourally: a conditional a
            // runbook is allowed to grant permission under, planted in a
            // live exit-10 refusal. Hand this gate the document hedges
            // and this line is the one that goes quiet.
            (
                "a refusal",
                orders_a_refusal_may_not_give as Gate,
                "Only if you have confirmed no burn landed, delete the record.",
            ),
        ] {
            assert!(
                !gate(text).is_empty(),
                "{which} must not be allowed to say: {text}",
            );
        }

        for (which, gate, text) in [
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "Do not delete the record.",
            ),
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "Only if the liveness line says another run has exited, delete the record.",
            ),
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "It says one of three things, and only one of them permits deleting the record.",
            ),
            (
                "a refusal",
                orders_a_refusal_may_not_give as Gate,
                "Do not delete that record on the strength of this refusal.",
            ),
            // A description of a hazard is not an order.
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "Deleting the record destroys the only local trace.",
            ),
            // A document may say this about a file that is not the record.
            (
                "a document",
                orders_a_document_may_not_give as Gate,
                "Delete that file and re-provision it.",
            ),
        ] {
            assert!(
                gate(text).is_empty(),
                "{which} is allowed to say this, and the gate flagged it: {text}",
            );
        }
    }

    #[test]
    fn no_shipped_document_says_the_refusal_always_answers_the_liveness_question() {
        // IN THIS MODULE, with the two functions it calls, and that is
        // the fix for the hole round 21 measured rather than a move for
        // tidiness. Both were `pub(crate)` and the document one could
        // therefore be named from a REFUSAL gate: one word at
        // `orchestrator.rs`'s call site handed that gate thirteen
        // document hedges, and an exit-10 refusal carrying `Only if you
        // have confirmed no burn landed, delete the record.` stayed
        // green. Its caller lives beside it now, so there is no
        // `pub(crate)` document gate left for a refusal site to reach.
        //
        // Four copies of the deletion gate, found one per review round by
        // a human reading. The rule keeps being restated in summary form
        // — a cleanup list, a "safe to prune between demos" heading —
        // and every summary drops the third verdict, because the third
        // verdict is the awkward one.
        //
        // What every wrong copy has in common is not a word count. It is
        // the claim that the exit-3 refusal REPORTS whether another run
        // holds the withdrawal. On a filesystem without `flock` it
        // reports that it could not tell, and an operator who was
        // promised an answer reads the absence of "RIGHT NOW" as one.
        //
        // So: any block that sends a reader to the refusal for the
        // liveness answer has to admit the answer can be missing. The
        // canonical procedures do; the summaries did not.
        const DOCS: [(&str, &str); 3] = [
            ("README.md", include_str!("../README.md")),
            (
                "docs/advanced_user_withdraw_runbook.md",
                include_str!("../docs/advanced_user_withdraw_runbook.md"),
            ),
            // Read by operators deciding what a release changed, and it
            // carried its own copy of the rule. Only the UNRELEASED
            // section is scanned: a released section is frozen by the
            // changelog policy, so guarding it would produce a failure
            // nobody is allowed to fix.
            ("CHANGELOG.md", include_str!("../../../CHANGELOG.md")),
        ];
        // Any of these means the block does not promise an answer it
        // cannot give, or hands the reader to the procedure that does.
        // Ways of acknowledging that the deletion is conditional on a
        // verdict that may not exist. A list, because the documents
        // legitimately say it in several registers — a table, a warning,
        // a pointer to the procedure — and the rule is about the
        // acknowledgement, not the wording.
        //
        // Every entry was checked against the copies this guard exists to
        // catch. Two candidates were REMOVED after measuring: "case 3a",
        // because the defective README pointed at Case 3a in one breath
        // and gave the two-verdict rule in the next; and "no age makes
        // this safe", which the defective RUNBOOK copy says verbatim. A
        // link is not an acknowledgement, and neither is ruling out the
        // wrong criterion.
        // "three" was a BARE NUMERAL and exempted any clause containing
        // it: "three deltas", "three independent checks", "three
        // wrappers". The hedge that matters is a reference to the
        // three-verdict gate, so say that instead. (Markdown emphasis is
        // stripped by `clauses`, which is why "one of three" matches
        // "one of **three** things".)
        // Every entry is a phrase that marks the permission as
        // CONDITIONAL, in the clause that grants it. "three" alone was
        // not: a bare numeral exempted "three deltas", "three
        // independent checks", "three wrappers" — any clause that
        // happened to count something.
        //
        // Markdown emphasis is stripped by `clauses`, which is why "one
        // of three" matches "one of **three** things".
        // Both vocabularies live in `source_guard`, keyed by surface.
        // They were passed in as parameters, and a copy of this list
        // that dropped the shared prohibitions was green under a test
        // that promised to forbid exactly that.

        // Sentences that hand somebody permission. The state-file
        // spelling is here because the documents call the same object two
        // things and only one of them was watched.
        // IMPERATIVE forms only. "Deleting the record destroys the only
        // local trace" is a description of a hazard, not permission to
        // act, and a list that cannot tell the two apart flags every
        // changelog entry that ever explained this defect. `Failed`
        // records are also excluded on purpose: they carry a hash by
        // construction, so pruning one is not this rule's business.
        // The VERB and its object, not seven spellings of one sentence.
        // The old list was a transcript of the four copies that had been
        // found by hand, so every form nobody had written yet was
        // invisible — "delete that record now", "remove the record",
        // "prune the state file", and the two the messages themselves
        // use.
        //
        // The orders live in `source_guard` too, and are not named here
        // at all: `clauses_ordering_a_deletion` reads them. A local
        // rebinding is exactly what let the two surfaces drift.

        // An absent unreleased section is only legitimate in ONE state:
        // straight after a release, where the first heading in the file
        // is the version a human just renamed it to. Anything else — a
        // typo, a re-styled heading, a restructure — leaves a first
        // heading that is neither, and that is a lost anchor rather than
        // an empty scan. Distinguishing the two is what lets this scan
        // nothing without also being unable to notice that it is.
        let changelog = DOCS[2].1;
        let first_heading = changelog
            .lines()
            .find(|l| l.starts_with("## "))
            .expect("CHANGELOG.md has no `## ` heading at all");
        assert!(
            first_heading == "## [Unreleased]" || is_a_release_heading(first_heading),
            "CHANGELOG.md's first heading is `{first_heading}`. This guard scans the unreleased \
             section and nothing else, so it has to be able to tell \"there is nothing unreleased \
             yet\" from \"the heading it looks for was renamed\", and that is the only thing that \
             tells them apart",
        );

        // ANY version, not `0.`. Both places that found the end of the
        // unreleased section spelled it `## [0.`, so the day a human
        // tags 1.0.0 the slice runs to the end of the file and every
        // released section — frozen history this guard may not act on —
        // becomes an offender. A guard that turns red on a release
        // nobody is allowed to fix is a guard that gets deleted.
        fn is_a_release_heading(line: &str) -> bool {
            line.strip_prefix("## [")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        }

        let mut offenders = Vec::new();
        for (name, text) in DOCS {
            // The changelog's released sections are not editable.
            let scanned = if name == "CHANGELOG.md" {
                // `unwrap_or(0)` here was fail-open ON A SCHEDULE, and
                // the schedule is a release. `end` is measured from
                // `start`, so with the heading absent the slice collapses
                // onto the FIRST release heading in the file: 11..2068
                // today, 1..10 the moment a human renames `##
                // [Unreleased]` to a version — which is exactly what
                // AGENTS.md tells them to do. Nine lines of file header,
                // scanned, green, and the changelog silently out of the
                // guard's sight from that commit on.
                //
                // Absent means absent, not "start at zero". After a
                // release there IS no unreleased section — AGENTS.md has
                // the next branch create one — and released sections are
                // frozen by the same policy, so this guard may not act on
                // them anyway. Scanning nothing is the correct answer and
                // is written down here rather than arrived at by
                // accident. An `expect` would be the other candidate and
                // is wrong: it would turn the prescribed release state
                // into a red build.
                match text.find("## [Unreleased]") {
                    Some(start) => {
                        let end = text[start..]
                            .lines()
                            .scan(0usize, |at, l| {
                                let here = *at;
                                *at += l.len() + 1;
                                Some((here, l))
                            })
                            .find(|(_, l)| is_a_release_heading(l))
                            .map_or(text.len(), |(i, _)| start + i);
                        &text[start..end]
                    },
                    None => "",
                }
            } else {
                text
            };
            for (i, block) in scanned.split("\n\n").enumerate() {
                // Whitespace collapsed first. These are hard-wrapped
                // documents, so every phrase this looks for straddles a
                // line break somewhere — the first version of this guard
                // matched nothing at all and looked like a pass.
                // Emphasis stripped here too. Without it "one of
                // **three** things" — the canonical acknowledgement, in
                // both instructional documents — matched no hedge, and
                // the block that carries the gate was reported as
                // promising an answer it cannot give.
                let lower = block
                    .to_ascii_lowercase()
                    .replace(['*', '`', '_'], "")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");

                // (a) The block sends a reader to the refusal FOR THE
                //     ANSWER. Block-scoped, and with no third "does it
                //     sound like an instruction" conjunct: that conjunct
                //     was added to silence two changelog entries and took
                //     the surviving copy of the gate out of scope with
                //     them. Changelog prose is handled where it belongs,
                //     in the hedge vocabulary.
                let promises_an_answer = (lower.contains("exit-3 refusal")
                    || lower.contains("liveness line"))
                    && (lower.contains("holds the withdrawal")
                        || lower.contains("holds this withdrawal")
                        || lower.contains("liveness"));

                // Trigger (a) applies to the INSTRUCTIONAL documents only.
                // Promising a reader an answer is something a procedure
                // does; a changelog entry recounting a past defect
                // mentions the same machinery without directing anybody,
                // and flagging it was what the discarded third conjunct
                // was really for. Trigger (b) applies everywhere, because
                // telling somebody to delete the record is dangerous
                // wherever it is written.
                let instructs = name != "CHANGELOG.md";
                if instructs
                    && promises_an_answer
                    && !a_document_admits_the_verdict_may_be_missing(&lower)
                {
                    offenders.push(format!(
                        "{name} block {i} (promises an answer): {}",
                        block.trim()
                    ));
                }

                // (b) A clause authorises the deletion. Through the
                //     SHARED scanner, which is the other half of this
                //     rule: the refusal messages are held to it too, and
                //     for two rounds only the documents were. Its
                //     splitting is the fix for the copy that lived here —
                //     `split(". ")` handed a dropped full stop or a line
                //     break an exemption, by gluing an order to the
                //     prohibition after it and finding the prohibition.
                for clause in orders_a_document_may_not_give(block) {
                    offenders.push(format!(
                        "{name} block {i} (authorises a deletion): {clause}"
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these send an operator to the exit-3 refusal for a liveness answer it may not have, \
             or authorise deleting the record in a sentence that does not name the verdict \
             permitting it — which is every run on a mount without `flock`, and the reading that \
             deletes a record mid-send: {offenders:#?}",
        );
    }

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
    #[should_panic(expected = "trailing_newline.rs")]
    fn an_attribute_on_the_last_line_names_the_file_rather_than_panicking_inside() {
        // No final newline, and the attribute is the last line. The
        // offset walk ran one past the end and `src[at..]` panicked from
        // inside the guard — a byte-index message about `source_guard`
        // in place of the sentence that names the file being cut.
        production_source("trailing_newline.rs", &format!("fn ship() {{}}\n{ATTR}"));
    }

    #[test]
    fn a_crlf_file_is_cut_in_the_same_place() {
        // `lines()` strips the `\r`, so a running offset built from
        // `line.len()` drifted by a byte per line and the cut landed
        // mid-token further down the file.
        let src =
            file(&["use std::fs;", "", "fn ship() {}", "", ATTR, DECL, "}"]).replace('\n', "\r\n");
        let cut = production_source("crlf.rs", &src);
        assert_eq!(
            cut, "use std::fs;\r\n\r\nfn ship() {}\r\n\r\n",
            "the cut is the same place in the same file with the other line ending",
        );
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
