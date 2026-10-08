//! What an operator reads when `withdrawByProof` reverts: the bridge's error
//! by name, with its arguments, and what to do about the common ones.
//!
//! The node's own text stays on the next line, unedited: a revert this
//! cannot name (a transport failure, a token's error) is still all there.

use bridge_relayer_daemon::bridge::decode_withdraw_revert;

/// The text exit 13 carries for a revert `reason` from the bridge client.
pub(crate) fn explain(reason: &str) -> String {
    let Some(revert) = decode_withdraw_revert(reason) else {
        return reason.to_string();
    };
    let advice = match revert.name {
        "NullifierAlreadyUsed" => Some(
            "this withdrawal's event has already been paid out — by an earlier run, another \
             operator or the relayer daemon — so there is nothing left to submit. Look for \
             WithdrawalByProofExecuted with this nullifier before doing anything with the record",
        ),
        "BridgePaused" => Some(
            "the owner has paused the EVM bridge. Re-run with --allow-retry once paused() is \
             false again; the burn is not repeated",
        ),
        "WithdrawTreasuryShortfall" => Some(
            "the treasury holds less than this withdrawal. Top it up (README Step 3) and re-run \
             with --allow-retry; the burn is not repeated",
        ),
        "UnknownAnchor" => Some(
            "the anchor the proof binds to is not in the bridge's window. Re-run with \
             --allow-retry to prove against the current window; the burn is not repeated",
        ),
        "WithdrawalProofRejected" => Some(
            "the deployed verifier rejected the proof: this host's Circuit-4 keys or aggregator \
             do not match it",
        ),
        _ => None,
    };
    match advice {
        Some(a) => format!("{} — {a}\n\x20 node said: {reason}", revert.rendered),
        None => format!("{}\n\x20 node said: {reason}", revert.rendered),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text a public Sepolia RPC returned for the dry run of a
    /// withdrawal whose nullifier was already spent.
    const NULLIFIER_REVERT: &str =
        "server returned an error response: error code 3: execution reverted: tried to decode \
         error from abi but failed: Error: Failed to slice index abi_decoded_error_data when \
         getting message from revert data, data: \
         \"0xdc215c0a2f01348a22bd5a63a9d567a5f79ac2d0b67b8255981eb11758b080580b9a2fbb\"";

    #[test]
    fn a_spent_nullifier_is_named_first_and_explained() {
        let text = explain(NULLIFIER_REVERT);
        assert!(
            text.starts_with("NullifierAlreadyUsed(nullifier: 0x2f01348a"),
            "the name comes first, not the node's noise: {text}"
        );
        assert!(text.contains("already been paid out"), "{text}");
        assert!(
            text.contains("node said: server returned"),
            "nothing is lost: {text}"
        );
    }

    #[test]
    fn the_resumable_reverts_name_the_flag() {
        // The relayer's `sol!` bindings are private; the selector is the
        // first four bytes of the signature's hash.
        let selector = &alloy::primitives::keccak256("BridgePaused()")[..4];
        let text = explain(&format!("data: \"0x{}\"", hex::encode(selector)));
        assert!(text.starts_with("BridgePaused()"), "{text}");
        assert!(text.contains("--allow-retry"), "{text}");
    }

    #[test]
    fn every_revert_with_advice_gets_it() {
        // The advice is chosen by matching the name as a string: a typo
        // there drops it with nothing failing to compile.
        for (signature, words, advice) in [
            ("NullifierAlreadyUsed(uint256)", 1, "already been paid out"),
            ("BridgePaused()", 0, "paused the EVM bridge"),
            ("WithdrawTreasuryShortfall(uint256,uint256)", 2, "Top it up"),
            ("UnknownAnchor(uint256)", 1, "not in the bridge's window"),
            ("WithdrawalProofRejected()", 0, "rejected the proof"),
        ] {
            let selector = &alloy::primitives::keccak256(signature)[..4];
            let data = format!("0x{}{}", hex::encode(selector), "00".repeat(32 * words));
            let text = explain(&format!("data: \"{data}\""));
            let name = &signature[..signature.find('(').unwrap()];
            assert!(text.starts_with(name), "{text}");
            assert!(text.contains(advice), "{name} lost its advice: {text}");
        }
    }

    #[test]
    fn a_revert_it_cannot_name_is_passed_through_unchanged() {
        let raw = "error sending request: connection refused";
        assert_eq!(explain(raw), raw);
    }

    #[test]
    fn both_submit_refusals_go_through_it() {
        let src = crate::source_guard::production_source(
            "orchestrator.rs",
            include_str!("orchestrator.rs"),
        );
        assert_eq!(
            src.matches("submit_revert::explain(&reason)").count(),
            2,
            "the dry-run revert and the submit revert both name the bridge error"
        );
    }
}
