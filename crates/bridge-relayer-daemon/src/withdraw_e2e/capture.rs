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
