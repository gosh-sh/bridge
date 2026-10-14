//! Stage 4's check that the event it captured is this withdrawal's.
//!
//! The capture follows `an_tx_hash` to the `WithdrawalInitiated` event that
//! burn emitted. On a fresh run the hash is the one this run broadcast; on a
//! resume it is whatever the record says, and a hash written in by hand — the
//! advanced runbook's Case 3a tells operators to do exactly that — can name
//! another withdrawal's burn from the same multisig. Proving that event pays
//! its recipient its amount and then marks THIS record confirmed. The
//! nullifier stops the same event paying twice; it does not stop this.
//!
//! So everything the record is keyed on is compared with the event before
//! anything is proved from it: the sender multisig, the recipient, the
//! destination chain and the amount, plus the token, which must be USDC.

use std::path::Path;

use crate::{
    args::{FromAddress, ToAddress, UsdcAmount},
    errors::{CliError, CliResult},
};

/// Compare the captured event with the withdrawal this run is executing.
///
/// A mismatch is exit 10: the record names a burn that is not this
/// withdrawal's, so where this withdrawal's own burn is — if it was ever
/// sent — is unknown, and only reconciliation on chain can say. Nothing is
/// proved, submitted or written. An event that does not decode is exit 12:
/// the prover reads the same bytes and would refuse them the same way.
pub(crate) fn check_captured_event(
    event_boc_b64: &str,
    from: &FromAddress,
    to: &ToAddress,
    amount: &UsdcAmount,
    an_tx: &str,
    record: Option<&Path>,
) -> CliResult<()> {
    let event = bridge_event_witness::decode_event_boc_base64(event_boc_b64).map_err(|e| {
        CliError::ProofFailed {
            reason: format!(
                "the event behind AN tx {an_tx} does not decode as WithdrawalInitiated: {e:#}. \
                 The prover reads the same bytes and would refuse them too."
            ),
            source: None,
        }
    })?;

    let mut differ = Vec::new();
    let event_amount = u128::from_str_radix(&event.amount_hex, 16).ok();
    if event_amount != Some(amount.0) {
        differ.push(format!(
            "amount: the event's {}, this withdrawal's {} USDC",
            event_amount.map_or_else(
                || format!("0x{}", event.amount_hex),
                |a| format!("{} USDC", UsdcAmount(a).display())
            ),
            amount.display(),
        ));
    }
    if !event
        .recipient_hex
        .eq_ignore_ascii_case(&hex::encode(to.address.as_slice()))
    {
        differ.push(format!(
            "recipient: the event's 0x{}, this withdrawal's {}",
            event.recipient_hex, to.address
        ));
    }
    let event_chain = hex::decode(&event.dst_chain_id_hex)
        .ok()
        .filter(|b| b.len() == 32)
        .map(|b| alloy::primitives::U256::from_be_slice(&b));
    if event_chain != Some(alloy::primitives::U256::from(to.chain_id)) {
        differ.push(format!(
            "destination chain: the event's {}, this withdrawal's {}",
            event_chain.map_or_else(
                || format!("0x{}", event.dst_chain_id_hex),
                |c| c.to_string()
            ),
            to.chain_id,
        ));
    }
    if event.token_id != crate::burn::USDC_ECC_ID {
        differ.push(format!(
            "token: the event's ECC[{}], this withdrawal's ECC[{}] (USDC)",
            event.token_id,
            crate::burn::USDC_ECC_ID
        ));
    }
    let ours = hex::decode(&from.account_id_hex).ok();
    match sender_of(&event.sender_hex) {
        Some((0, id)) if ours.as_deref() == Some(id.as_slice()) => {},
        Some((wc, id)) => differ.push(format!(
            "sender: the event's {wc}:{}, this withdrawal's 0:{}",
            hex::encode(id),
            from.account_id_hex
        )),
        None => differ.push(format!(
            "sender: the event's sender field 0x{} is not an account address",
            event.sender_hex
        )),
    }

    if differ.is_empty() {
        return Ok(());
    }
    let record = record.map_or_else(
        || "this withdrawal's record".to_string(),
        |p| format!("the record {}", p.display()),
    );
    Err(CliError::BurnOutcomeUnknown {
        reason: format!(
            "the WithdrawalInitiated event behind AN tx {an_tx} is not this withdrawal's — \
             {}.\n\x20 Nothing was proved or submitted, and {record} was not changed. It names a \
             burn that belongs to another withdrawal, most likely a hash written into it by hand. \
             Find this withdrawal's own sendTransaction from 0:{} (the advanced runbook, Case \
             3a), write that hash into the record, and re-run with --allow-retry.",
            differ.join("; "),
            from.account_id_hex,
        ),
        source: None,
    })
}

/// The account a `WithdrawalInitiated` names as its sender, as
/// `(workchain, account id)`.
///
/// The field is the 267-bit `std_addr` the contract stores: tag `10`, no
/// anycast (`0`), an 8-bit workchain and a 256-bit id. It is not
/// byte-aligned — the id starts at bit 11 — so every byte of the id is the
/// tail of one byte and the head of the next.
fn sender_of(sender_hex: &str) -> Option<(i8, [u8; 32])> {
    let b = hex::decode(sender_hex).ok()?;
    if b.len() != 34 || b[0] >> 5 != 0b100 {
        return None;
    }
    let workchain = ((b[0] << 3) | (b[1] >> 5)) as i8;
    let mut id = [0u8; 32];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = (b[1 + i] << 3) | (b[2 + i] >> 5);
    }
    Some((workchain, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `WithdrawalInitiated` of a 1.000000 USDC shellnet withdrawal from the
    /// multisig `0:826403c9…` to `0x400b…46B3` on Sepolia.
    const EVENT_1_000000: &str = "te6ccgEBBAEAyQABn+AA0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAmoAAAAAAuWsPGrGP3VgAQJwPIOJWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAqjanAAAAAAAAAAAAAAAAAA9CQAAAAAMDAgBDgBBMgHk+jlxJjmyW58sP9K4022mCUYFhcswU6JKPCAImUAAoQAu4Jcqcp93fyyXznB7Ac6kSRrM=";
    /// The same multisig and recipient, 0.999995 USDC.
    const EVENT_0_999995: &str = "te6ccgEBBAEAyQABn+AA0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAmoAAAAAAujTl2rGudtgAQJwPIOJWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAqjanAAAAAAAAAAAAAAAAAA9COwAAAAMDAgBDgBBMgHk+jlxJjmyW58sP9K4022mCUYFhcswU6JKPCAImUAAoQAu4Jcqcp93fyyXznB7Ac6kSRrM=";
    /// An older event with token 2 on chain 1, the fixture
    /// `bridge-event-witness` tests its decoder with.
    const EVENT_TOKEN_2: &str = "te6ccgEBBAEAyQABn+AA0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ0NMAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAmoAAAAAAAIZdmoHXvdgAQJwPIOJWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAA9CQAAAAAIDAgBDgAm3yq/MUPIYsiAFU9xmlVK1j7ShCFBTfqlHoaqgQg+0MAAodC01zGY0wFMpJaO4RLxFTkQ49E4=";
    const MULTISIG: &str = "826403c9f472e24c7364b73e587fa571a6db4c128c0b0b9660a7449478401132";
    /// Mixed case on purpose: the comparison is of the address, not of its
    /// spelling.
    const RECIPIENT: &str = "0x400bb825Ca9CA7DdDfCb25F39c1Ec073a91246B3";
    const AN_TX: &str = "0xa8690153d84d2679cf120e29390eced0e0153de42453d87b4f4f070a23c74756";

    fn from() -> FromAddress {
        crate::args::parse_from(&format!("{MULTISIG}::{MULTISIG}")).unwrap()
    }

    fn to() -> ToAddress {
        crate::args::parse_to(RECIPIENT, Some(11_155_111)).unwrap()
    }

    fn amount(s: &str) -> UsdcAmount {
        crate::args::parse_amount(s).unwrap()
    }

    /// The refusal for `event` against this withdrawal, checked for what
    /// every mismatch refusal has to carry.
    fn refusal(event: &str, from: &FromAddress, to: &ToAddress, amt: &str) -> String {
        let record = Path::new("/state/abc.json");
        let err = check_captured_event(event, from, to, &amount(amt), AN_TX, Some(record))
            .expect_err("a mismatch is refused");
        assert_eq!(err.exit_code().as_i32(), 10, "{err}");
        let msg = format!("{err}");
        assert!(msg.contains(AN_TX), "names the burn it followed: {msg}");
        assert!(msg.contains("/state/abc.json"), "names the record: {msg}");
        assert!(msg.contains("Nothing was proved"), "{msg}");
        msg
    }

    #[test]
    fn the_event_of_this_withdrawal_passes() {
        check_captured_event(
            EVENT_1_000000,
            &from(),
            &to(),
            &amount("1.000000"),
            AN_TX,
            None,
        )
        .expect("every field matches");
        check_captured_event(
            EVENT_0_999995,
            &from(),
            &to(),
            &amount("0.999995"),
            AN_TX,
            None,
        )
        .expect("every field matches");
    }

    #[test]
    fn a_hash_from_another_withdrawal_of_the_same_multisig_is_refused_before_the_proof() {
        // The record is for 1.000019; the hash on it is the burn of a
        // 1.000000 withdrawal that already paid out.
        let msg = refusal(EVENT_1_000000, &from(), &to(), "1.000019");
        assert!(
            msg.contains("1.000000") && msg.contains("1.000019"),
            "{msg}"
        );
        assert!(
            msg.contains("Case 3a") && msg.contains("--allow-retry"),
            "{msg}"
        );
    }

    #[test]
    fn another_recipient_is_refused() {
        let other = crate::args::parse_to(
            "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
            Some(11_155_111),
        )
        .unwrap();
        let msg = refusal(EVENT_1_000000, &from(), &other, "1.000000");
        assert!(
            msg.to_lowercase()
                .contains("0x742d35cc6634c0532925a3b844bc454e4438f44e"),
            "{msg}"
        );
    }

    #[test]
    fn another_destination_chain_is_refused() {
        let other = ToAddress {
            address: to().address,
            chain_id: 1,
        };
        let msg = refusal(EVENT_1_000000, &from(), &other, "1.000000");
        assert!(msg.contains("11155111"), "{msg}");
    }

    #[test]
    fn another_sender_is_refused() {
        let other =
            crate::args::parse_from(&format!("{}::{}", "ab".repeat(32), "ab".repeat(32))).unwrap();
        let msg = refusal(EVENT_1_000000, &other, &to(), "1.000000");
        assert!(msg.contains(MULTISIG), "{msg}");
    }

    #[test]
    fn a_token_other_than_usdc_is_refused() {
        let msg = refusal(EVENT_TOKEN_2, &from(), &to(), "1.000000");
        assert!(msg.contains("token"), "{msg}");
    }

    #[test]
    fn the_sender_account_starts_eleven_bits_in() {
        let id: [u8; 32] = hex::decode(MULTISIG).unwrap().try_into().unwrap();
        assert_eq!(
            sender_of("80104c80793e8e5c498e6c96e7cb0ff4ae34db698251816172cc14e8928f08022650"),
            Some((0, id))
        );
        assert_eq!(sender_of("00"), None);
    }

    #[test]
    fn an_event_that_does_not_decode_is_exit_12() {
        let err = check_captured_event(
            "not a boc",
            &from(),
            &to(),
            &amount("1.000000"),
            AN_TX,
            None,
        )
        .expect_err("garbage does not decode");
        assert_eq!(err.exit_code().as_i32(), 12, "{err}");
    }

    #[test]
    fn the_check_runs_before_the_record_says_captured() {
        let src = crate::source_guard::production_source(
            "orchestrator.rs",
            include_str!("orchestrator.rs"),
        );
        let check = src
            .find("event_identity::check_captured_event(")
            .expect("the check is wired");
        let captured = src.find("Status::Captured").expect("the capture write");
        assert!(
            check < captured,
            "the record may say captured only about this withdrawal's event"
        );
    }
}
