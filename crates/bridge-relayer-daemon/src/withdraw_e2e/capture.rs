//! Poll GQL for a freshly-emitted `WithdrawalInitiated` ExtOut event and
//! resolve the owning block's canonical metadata.
//!
//! Mirrors `capture_event_metadata` in `python/helper/bridge_e2e.py`:
//!
//! * Baseline snapshot of pre-existing ExtOut message IDs so we ignore
//!   anything that landed before our capture window opened.
//! * Loop `query_bridge_extouts` looking for a message whose `dst`
//!   matches the WithdrawalInitiated `makeAddrExtern(618)` sentinel and
//!   whose id was not in the baseline.
//! * Resolve the containing block via `query_block_by_hash` — the
//!   `Message.src_transaction.block_id` field is the block's `hash`, NOT
//!   its consensus `block_id`; we need the direct lookup to obtain the
//!   real `block_id` and `envelope_hash` that Circuit 4 hashes.
//! * Fetch the account's on-chain `dapp_id` (falls back to a zero-hex
//!   pad, matching the Python exporter's tolerance).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tracing::{debug, info};

use bridge_gql_fetcher::gql_client::{BridgeExtOutMessage, GqlBlockByHash, GqlClient};

/// Normalize a message `dst` string for equality comparison. The GQL
/// server emits both extern-form (`:<hex>`) and workchain-legacy
/// (`0:<hex>`) verbatim; different code paths hand us slightly
/// different casings and prefix conventions. Compare on a canonical
/// lowercase-hex form (workchain segment preserved) so
/// `preflight.usdc_bridge_legacy` (`0:<lower>`) equals whatever the
/// message row carries.
fn dst_matches(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Fully-resolved metadata for one captured `WithdrawalInitiated` event.
/// Fields carry hex strings straight from GQL — the driver decodes them
/// into `[u8; 32]` fixed-size arrays before handing to the exporter.
#[derive(Debug, Clone)]
pub struct CapturedEvent {
    /// Base64 BOC of the ExtOut `Message` cell (the exporter input).
    pub event_boc_b64: String,
    /// Consensus block_id (32-byte hex, from `block(hash:).block_id`).
    pub block_id_hex: String,
    /// Block's `hash` field — the BOC hash used to look the block up.
    pub block_hash: String,
    pub block_seq_no: u64,
    pub block_height: u64,
    /// 32-byte hex envelope hash from the block record.
    pub envelope_hash_hex: String,
    /// On-chain dapp_id of the emitting account (hex, may be `"0"*64`).
    pub account_dapp_id_hex: String,
    /// Account_id echo of the input (hex).
    pub account_id_hex: String,
    /// GQL Message.id — opaque tag we use to dedupe against the baseline.
    pub message_id: String,
    /// Whether the containing block is a key block (informational).
    pub key_block: bool,
}

/// Snapshot the current set of ExtOut message IDs so
/// [`capture_next_withdrawal_event`] can ignore anything that predated
/// the capture window.
pub async fn snapshot_baseline_msg_ids(
    gql: &GqlClient,
    account_id_hex: &str,
    dapp_id_hex: &str,
    limit: u32,
) -> Result<HashSet<String>> {
    let msgs = gql
        .query_bridge_extouts(account_id_hex, dapp_id_hex, limit)
        .await
        .context("baseline query_bridge_extouts failed")?;
    Ok(msgs.into_iter().map(|m| m.id).collect())
}

/// Poll GQL until an ExtOut message matching `dst_filter` and not in
/// `baseline_ids` surfaces, then resolve the owning block and dapp_id.
///
/// Returns the youngest matching message (`created_at` desc) so
/// simultaneous emissions in the same block still yield a deterministic
/// pick.
pub async fn capture_next_withdrawal_event(
    gql: &GqlClient,
    account_id_hex: &str,
    dapp_id_hex: &str,
    dst_filter: &str,
    baseline_ids: &HashSet<String>,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<CapturedEvent> {
    info!(
        %dst_filter,
        ?timeout,
        baseline_size = baseline_ids.len(),
        "waiting for WithdrawalInitiated ExtOut event",
    );
    let deadline = Instant::now() + timeout;
    let mut last_matched_count: i32 = -1;
    let target: BridgeExtOutMessage = loop {
        if Instant::now() >= deadline {
            bail!(
                "did not observe a WithdrawalInitiated event within {:?}",
                timeout
            );
        }
        let msgs = gql
            .query_bridge_extouts(account_id_hex, dapp_id_hex, 500)
            .await
            .context("query_bridge_extouts failed")?;
        let mut matched: Vec<BridgeExtOutMessage> = msgs
            .into_iter()
            .filter(|m| m.dst == dst_filter && !baseline_ids.contains(&m.id))
            .collect();
        if matched.len() as i32 != last_matched_count {
            debug!(
                matched = matched.len(),
                "poll: matched new events (not in baseline)",
            );
            last_matched_count = matched.len() as i32;
        }
        if !matched.is_empty() {
            matched.sort_by_key(|m| m.created_at.unwrap_or(0));
            break matched.pop().unwrap();
        }
        tokio::time::sleep(poll_interval).await;
    };

    let block_hash = target
        .block_id
        .as_ref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "event {} has no block_id nor src_transaction.block_id",
                target.id
            )
        })?
        .clone();
    let block: GqlBlockByHash = gql
        .query_block_by_hash(&block_hash)
        .await
        .with_context(|| format!("query_block_by_hash({block_hash})"))?;

    // Workchain-root accounts return an empty dapp_id; pad to 32 zero
    // bytes hex so downstream `parse_hex32` succeeds — matches the
    // exporter's tolerance in the Python driver.
    let account_dapp_id_hex = gql
        .query_account_dapp_id(account_id_hex, dapp_id_hex)
        .await
        .context("query_account_dapp_id failed")?
        .unwrap_or_else(|| "0".repeat(64));

    info!(
        message_id = %target.id,
        block_seq_no = block.seq_no,
        block_height = block.height,
        key_block = block.key_block,
        block_id = %block.block_id,
        "captured WithdrawalInitiated event",
    );

    Ok(CapturedEvent {
        event_boc_b64: target.boc,
        block_id_hex: block.block_id,
        block_hash: block.hash,
        block_seq_no: block.seq_no,
        block_height: block.height,
        envelope_hash_hex: block.envelope_hash,
        account_dapp_id_hex,
        account_id_hex: account_id_hex.to_string(),
        message_id: target.id,
        key_block: block.key_block,
    })
}

/// Capture the `WithdrawalInitiated` ExtOut emitted by *our* burn by
/// chain-following the multisig transaction hash, avoiding the
/// youngest-pick race that [`capture_next_withdrawal_event`] has under
/// concurrent operators.
///
/// The walk (all identifiers deterministic once the burn tx exists):
///
/// 1. `blockchain.transaction(hash: an_tx_hash) { out_messages { id dst } }`
///    — pick the outbound whose `dst` equals `usdc_bridge_legacy`. This
///    is the internal message the multisig sent to USDCBridge. Retry
///    briefly to absorb GQL propagation lag.
/// 2. `blockchain.message(hash: msig_out_msg_id) { dst_transaction
///    { out_messages { id dst } } }` — retry until USDCBridge has
///    processed the message and its `dst_transaction` (with the ExtOut
///    in its `out_messages`) is visible. Pick the outbound with
///    `dst == ext_out_dst`.
/// 3. `blockchain.message(hash: withdrawal_msg_id) { … }` — fetch the
///    full ExtOut row (boc + block metadata) via
///    [`GqlClient::query_bridge_extout_by_id`], then reuse the same
///    block-by-hash + account-dapp_id resolution as the untargeted path.
///
/// Timing budget: both retry loops share `timeout`. On timeout the
/// error names which stage stalled so the operator can distinguish
/// "burn bounced (no USDCBridge tx)" from "USDCBridge tx exists but no
/// ExtOut (bridge rejected the call)" from "GQL propagation stuck".
///
/// This function is the multi-user-safe alternative used by
/// `bridge-withdraw-e2e-cli`; the daemon's `run_once` still uses the
/// baseline-snapshot path via [`capture_next_withdrawal_event`].
#[allow(clippy::too_many_arguments)]
pub async fn capture_targeted_withdrawal_event(
    gql: &GqlClient,
    an_tx_hash: &str,
    usdc_bridge_legacy: &str,
    ext_out_dst: &str,
    account_id_hex: &str,
    dapp_id_hex: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<CapturedEvent> {
    info!(
        %an_tx_hash,
        %usdc_bridge_legacy,
        %ext_out_dst,
        ?timeout,
        "targeted capture: chain-following multisig tx to WithdrawalInitiated ExtOut",
    );
    let deadline = Instant::now() + timeout;

    // Stage 1: multisig tx → outbound to USDCBridge.
    let msig_out_msg_id = loop {
        if Instant::now() >= deadline {
            bail!(
                "targeted capture stalled at stage 1: transaction(hash: {an_tx_hash}) not \
                 visible via GQL within {:?} — check GQL endpoint reachability and multisig \
                 broadcast",
                timeout
            );
        }
        match gql
            .query_tx_out_messages(an_tx_hash)
            .await
            .context("query_tx_out_messages failed")?
        {
            Some(out_msgs) if !out_msgs.is_empty() => {
                let match_ = out_msgs
                    .iter()
                    .find(|m| dst_matches(&m.dst, usdc_bridge_legacy));
                if let Some(m) = match_ {
                    debug!(msig_out_msg_id = %m.id, "stage 1: found multisig → USDCBridge outbound");
                    break m.id.clone();
                }
                // Tx exists but no outbound to USDCBridge — the multisig
                // must have taken a different path (bounce, self-transfer,
                // wrong dest). Fail fast, don't wait.
                let dsts: Vec<&str> = out_msgs.iter().map(|m| m.dst.as_str()).collect();
                bail!(
                    "transaction {an_tx_hash} produced {} outbound messages but none targeted \
                     USDCBridge ({usdc_bridge_legacy}); saw destinations: {:?}. This means the \
                     multisig sendTransaction did not forward to the bridge — check the tx on-chain.",
                    out_msgs.len(),
                    dsts,
                );
            }
            Some(_) => {
                bail!(
                    "transaction {an_tx_hash} exists but has zero outbound messages — the \
                     multisig call reverted before forwarding. Check tx.aborted / compute.exit_code."
                );
            }
            None => {
                debug!("stage 1: tx not yet visible via GQL, retrying");
                tokio::time::sleep(poll_interval).await;
            }
        }
    };

    // Stage 2: USDCBridge dst_transaction → outbound ExtOut.
    let withdrawal_msg_id = loop {
        if Instant::now() >= deadline {
            bail!(
                "targeted capture stalled at stage 2: message(hash: {msig_out_msg_id}) \
                 dst_transaction never surfaced a `dst = {ext_out_dst}` ExtOut within {:?} — \
                 USDCBridge may have bounced the call (check for a bounce message back to the \
                 multisig)",
                timeout
            );
        }
        match gql
            .query_msg_dst_tx_out_messages(&msig_out_msg_id)
            .await
            .context("query_msg_dst_tx_out_messages failed")?
        {
            Some(out_msgs) => {
                let match_ = out_msgs.iter().find(|m| dst_matches(&m.dst, ext_out_dst));
                if let Some(m) = match_ {
                    debug!(withdrawal_msg_id = %m.id, "stage 2: found USDCBridge → ExtOut(618)");
                    break m.id.clone();
                }
                // dst_transaction exists but no ExtOut yet — could be
                // in-flight in the same tx (unlikely) or the tx never
                // emitted one. Retry briefly then give up at deadline.
                debug!(
                    outbound_count = out_msgs.len(),
                    "stage 2: dst_transaction visible but no matching ExtOut yet, retrying",
                );
                tokio::time::sleep(poll_interval).await;
            }
            None => {
                debug!("stage 2: dst_transaction not yet visible, retrying");
                tokio::time::sleep(poll_interval).await;
            }
        }
    };

    // Stage 3: fetch the ExtOut's full row. Retry only a couple of ticks —
    // if the id showed up via stage 2 it should be immediately readable.
    let ext_out = loop {
        if Instant::now() >= deadline {
            bail!(
                "targeted capture stalled at stage 3: message(hash: {withdrawal_msg_id}) \
                 known-good id but its row (boc / block_id) never became readable within {:?}",
                timeout
            );
        }
        match gql
            .query_bridge_extout_by_id(&withdrawal_msg_id)
            .await
            .context("query_bridge_extout_by_id failed")?
        {
            Some(m) => break m,
            None => {
                debug!("stage 3: ExtOut row not readable yet, retrying");
                tokio::time::sleep(poll_interval).await;
            }
        }
    };

    // Same block + dapp_id resolution as the untargeted path.
    let block_hash = ext_out.block_id.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "captured ExtOut {} has no block_id nor src_transaction.block_id",
            ext_out.id
        )
    })?;
    let block: GqlBlockByHash = gql
        .query_block_by_hash(&block_hash)
        .await
        .with_context(|| format!("query_block_by_hash({block_hash})"))?;
    let account_dapp_id_hex = gql
        .query_account_dapp_id(account_id_hex, dapp_id_hex)
        .await
        .context("query_account_dapp_id failed")?
        .unwrap_or_else(|| "0".repeat(64));

    info!(
        message_id = %ext_out.id,
        block_seq_no = block.seq_no,
        block_height = block.height,
        key_block = block.key_block,
        block_id = %block.block_id,
        "captured WithdrawalInitiated event via targeted chain-follow",
    );

    Ok(CapturedEvent {
        event_boc_b64: ext_out.boc,
        block_id_hex: block.block_id,
        block_hash: block.hash,
        block_seq_no: block.seq_no,
        block_height: block.height,
        envelope_hash_hex: block.envelope_hash,
        account_dapp_id_hex,
        account_id_hex: account_id_hex.to_string(),
        message_id: ext_out.id,
        key_block: block.key_block,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dst_matches_is_case_insensitive_on_hex() {
        // USDCBridge legacy comes out as `0:<lower>`; a nearly-identical
        // uppercase form should still match.
        let a = "0:1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a";
        let b = "0:1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A1A";
        assert!(dst_matches(a, b));
    }

    #[test]
    fn dst_matches_rejects_different_workchain() {
        // `0:<hex>` vs `-1:<hex>` must never collide.
        let a = "0:00";
        let b = "-1:00";
        assert!(!dst_matches(a, b));
    }

    #[test]
    fn dst_matches_extern_form() {
        // The `WithdrawalInitiated` dst is the extern-form
        // `:...:026a`. Case-insensitive equality still applies.
        let a = ":000000000000000000000000000000000000000000000000000000000000026a";
        let b = ":000000000000000000000000000000000000000000000000000000000000026A";
        assert!(dst_matches(a, b));
    }
}
