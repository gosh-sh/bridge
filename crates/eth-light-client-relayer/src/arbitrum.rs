//! Arbitrum block anchors (`eth-lc-relayer arbitrum-anchors`): gets the
//! Arbitrum blocks that hold deposits into the bridge's anchor set through
//! `ArbitrumBlockVerifier.proveBlock` on Acki Nacki, without the bridge owner.
//!
//! An Arbitrum chain posts its batches to the `SequencerInbox` on its parent
//! chain, and about every 30 min (Arbitrum Sepolia) or hour (Arbitrum One) a
//! state assertion, `AssertionCreated` on its rollup. The assertion names the
//! hash of block M, the last block of a batch, and M's state holds the hashes
//! of the 393 168 blocks before M in the EIP-2935 history contract.
//! `proveBlock` takes the assertion's receipt in a parent-chain block the
//! bridge already accepts, M's header and an `eth_getProof` of the history
//! slot of deposit block N; the bridge then accepts N for the Arbitrum chain.
//!
//! When the assertion lands, M is already older than the state an ordinary
//! node keeps (M is 33 min old on Arbitrum Sepolia, 62 min on Arbitrum One).
//! Its batch lands within a minute of M, so [`tick`] rolls forward: on every
//! `SequencerBatchDelivered` it takes the batch's last block and the slot
//! proofs of the deposit blocks still waiting, while that state is seconds
//! old, and keeps them in the state file until an assertion ends at that
//! block. No archive node is needed. A batch missed while the relayer was down
//! costs a delay, not a deposit: a later batch covers the same blocks.
//!
//! The parent-chain block with the assertion has to be accepted by the bridge
//! first. The parent chain's light client proves one block per epoch, the
//! checkpoint; the assertion's block is almost never one. Once the light
//! client's head has passed the epoch the block sits in, [`tick`] sends
//! `submitAncestry` for the span between the two checkpoints around it, and
//! the light client admits every block between them. Until the bridge
//! accepts the block, the proof waits in the state file.
//!
//! The live Acki Nacki side (`TvmAnchorTarget`) needs the `live-submit`
//! feature, as every other call this crate sends to Acki Nacki.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "live-submit")]
use acki_nacki_interface::{
    ContractCallRequest, ExtendedAddress, IAckiNacki, TvmAckiNacki, TvmClientConfig,
};
use alloy::{
    consensus::ReceiptEnvelope,
    eips::Encodable2718,
    primitives::{address, b256, Address, Bytes, B256, U256},
    providers::Provider,
    rpc::types::{Filter, TransactionReceipt, TransactionRequest},
    sol,
    sol_types::SolCall,
    trie::{proof::ProofRetainer, HashBuilder, Nibbles},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
#[cfg(feature = "live-submit")]
use tvm_client::crypto::KeyPair;

use crate::{
    error::RelayerError,
    header_rlp::{encode_header_rlp, link_headers},
    source::get_logs_with_retry,
};

/// `Deposit(uint256 indexed depositId, address indexed sender, uint256 amount,
/// bytes32 anAccount, uint256 timestamp, address token)` of `AckiNackiBridge`.
/// Only the blocks that hold one are read, not the logs themselves.
pub const DEPOSIT: B256 = b256!("935bca73ba823197b8e5564da08c2c294de3811fc4a4cb482e6af4fd08623c0e");

/// The EIP-2935 history contract as ArbOS 40 deploys it, and its window.
pub const HISTORY_ADDRESS: Address = address!("0000F90827F1C53a10cb7A02335B175320002935");
pub const HISTORY_WINDOW: u64 = 393_168;

/// `AssertionCreated` (nitro-contracts `IRollupCore`, BoLD), and the data word
/// that holds the hash of the block the assertion ends at
/// (`assertion.afterState.globalState.bytes32Vals[0]`).
pub const ASSERTION_CREATED: B256 =
    b256!("901c3aee23cf4478825462caaab375c606ab83516060388344f0650340753630");
pub const ASSERTION_BLOCK_HASH_WORD: usize = 13;

/// `SequencerBatchDelivered(uint256 indexed batchSequenceNumber, bytes32
/// indexed beforeAcc, bytes32 indexed afterAcc, bytes32 delayedAcc, uint256
/// afterDelayedMessagesRead, (uint64,uint64,uint64,uint64) timeBounds, uint8
/// dataLocation)` on the `SequencerInbox`.
pub const SEQUENCER_BATCH_DELIVERED: B256 =
    b256!("7394f4a19a13c7b92b5bb71033245305946ef78452f7b4986ac1390b5df4ebd7");

/// Arbitrum's NodeInterface precompile: maps a block to the batch holding it.
pub const NODE_INTERFACE: Address = address!("00000000000000000000000000000000000000C8");

sol! {
    #[allow(missing_docs)]
    function findBatchContainingBlock(uint64 blockNum) external view returns (uint64 batch);
}

/// An Arbitrum chain the bridge can prove blocks of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArbitrumChain {
    pub name: &'static str,
    pub chain_id: u64,
    pub parent_chain_id: u64,
    /// Emits `AssertionCreated`; read from `SequencerInbox.rollup()`.
    pub rollup: Address,
    pub sequencer_inbox: Address,
    /// The block the chain's Nitro history starts at: block number =
    /// message count - 1 + this.
    pub nitro_genesis_block: u64,
    /// `genesis_time` of the parent chain's beacon chain: where its epochs,
    /// and so its light-client checkpoints, start.
    pub parent_beacon_genesis: u64,
}

pub const ARBITRUM_CHAINS: &[ArbitrumChain] = &[
    ArbitrumChain {
        name: "Arbitrum Sepolia",
        chain_id: deposit_chain_ids::CHAIN_ID_ARBITRUM_SEPOLIA,
        parent_chain_id: deposit_chain_ids::CHAIN_ID_SEPOLIA,
        rollup: address!("042b2e6c5e99d4c521bd49beed5e99651d9b0cf4"),
        sequencer_inbox: address!("6c97864CE4bEf387dE0b3310A44230f7E3F1be0D"),
        nitro_genesis_block: 0,
        parent_beacon_genesis: 1_655_733_600,
    },
    ArbitrumChain {
        name: "Arbitrum One",
        chain_id: deposit_chain_ids::CHAIN_ID_ARBITRUM_ONE,
        parent_chain_id: 1,
        rollup: address!("4dceb440657f21083db8add07665f8ddbe1dcfc0"),
        sequencer_inbox: address!("1c479675ad559DC151F6Ec7ed3FbF8ceE79582B6"),
        nitro_genesis_block: 22_207_817,
        parent_beacon_genesis: 1_606_824_023,
    },
];

/// The `SequencerInbox` calls that post a batch, with the argument that is
/// the message count after it: `(sequenceNumber, data,
/// afterDelayedMessagesRead, gasRefunder, prevMessageCount, newMessageCount,
/// …)` and the blob calls without `data`. The delay-proof variants append a
/// static tuple.
const BATCH_POSTING_CALLS: &[([u8; 4], usize)] = &[
    ([0x8f, 0x11, 0x1f, 0x3c], 5), // addSequencerL2BatchFromOrigin
    ([0xe0, 0xbc, 0x97, 0x29], 5), // addSequencerL2Batch
    ([0x69, 0xca, 0xcd, 0xed], 5), // addSequencerL2BatchFromOriginDelayProof
    ([0x6e, 0x62, 0x00, 0x55], 5), // addSequencerL2BatchDelayProof
    ([0x3e, 0x5a, 0xa0, 0x82], 4), // addSequencerL2BatchFromBlobs
    ([0x91, 0x7c, 0xf8, 0xac], 4), // addSequencerL2BatchFromBlobsDelayProof
];

/// The last block of `batch`, from the parent-chain call that posted it:
/// `newMessageCount - 1`, offset by the chain's Nitro genesis. `None` for a
/// call that is not one of [`BATCH_POSTING_CALLS`] or posts another batch.
/// Exact, unlike asking a node where a batch ends: a load-balanced endpoint
/// answers from backends that have executed different amounts of it.
pub fn batch_end_from_calldata(input: &[u8], batch: u64, nitro_genesis_block: u64) -> Option<u64> {
    let (selector, args) = input.split_at_checked(4)?;
    let &(_, at) = BATCH_POSTING_CALLS.iter().find(|(s, _)| s == selector)?;
    let word = |i: usize| args.get(32 * i..32 * (i + 1)).map(U256::from_be_slice);
    if word(0)? != U256::from(batch) {
        return None;
    }
    let count = u64::try_from(word(at)?).ok()?;
    count.checked_sub(1)?.checked_add(nitro_genesis_block)
}

pub fn arbitrum_chain(chain_id: u64) -> Option<&'static ArbitrumChain> {
    ARBITRUM_CHAINS.iter().find(|c| c.chain_id == chain_id)
}

// ─────────────────────────────────────────────────────────────────────
// Encoding what `proveBlock` takes
// ─────────────────────────────────────────────────────────────────────

/// The receipt trie of a block: its root, and the nodes from the root to the
/// receipt of transaction `index`.
pub fn receipt_proof(receipts: &[ReceiptEnvelope], index: usize) -> (B256, Vec<Bytes>) {
    let mut leaves: Vec<(Nibbles, Vec<u8>)> = receipts
        .iter()
        .enumerate()
        .map(|(i, r)| (Nibbles::unpack(alloy::rlp::encode(i)), r.encoded_2718()))
        .collect();
    leaves.sort_by_key(|leaf| leaf.0);
    let target = Nibbles::unpack(alloy::rlp::encode(index));
    let mut builder = HashBuilder::default().with_proof_retainer(ProofRetainer::new(vec![target]));
    for (key, value) in &leaves {
        builder.add_leaf(*key, value);
    }
    let root = builder.root();
    let nodes = builder.take_proof_nodes().matching_nodes_sorted(&target);
    (root, nodes.into_iter().map(|(_, node)| node).collect())
}

/// The hash of the block an `AssertionCreated` ends at, from the log's data.
pub fn assertion_end_hash(data: &[u8]) -> Option<B256> {
    let at = ASSERTION_BLOCK_HASH_WORD * 32;
    data.get(at..at + 32).map(B256::from_slice)
}

// ─────────────────────────────────────────────────────────────────────
// State
// ─────────────────────────────────────────────────────────────────────

/// `eth_getProof(HISTORY_ADDRESS, [N % HISTORY_WINDOW], M)`, both halves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotProof {
    pub account_proof: Vec<Bytes>,
    pub storage_proof: Vec<Bytes>,
}

/// The last block of a posted batch, with the slot proofs taken when the
/// batch landed, for the deposit blocks that were waiting then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchEnd {
    pub number: u64,
    pub hash: B256,
    pub header: Bytes,
    /// Deposit block number → its slot proof at this block.
    pub slots: BTreeMap<u64, SlotProof>,
}

/// A batch end an assertion has named, with the assertion's receipt: all
/// `proveBlock` needs once the bridge accepts the parent-chain block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyProof {
    pub parent_block: u64,
    pub parent_hash: B256,
    pub parent_header: Bytes,
    pub tx_index: u32,
    pub receipt_proof: Vec<Bytes>,
    pub end: BatchEnd,
    /// Deposit block → unix time `proveBlock` was last sent for it.
    #[serde(default)]
    pub sent: BTreeMap<u64, u64>,
    /// Unix time a `submitAncestry` span for `parent_block` was last tried.
    #[serde(default)]
    pub span_tried: Option<u64>,
    /// `submitAncestry` calls sent for `parent_block`, at most
    /// [`MAX_SPAN_SENDS`].
    #[serde(default)]
    pub spans_sent: u32,
}

/// What `arbitrum-anchors` keeps between runs. The slot proofs cannot be
/// fetched again once their state has aged out of the nodes, so they live
/// here until the assertion that uses them lands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorState {
    pub chain_id: u64,
    /// `AckiNackiBridge` on the Arbitrum chain whose deposits this state
    /// tracks: a state file of another bridge would skip the new one's first
    /// deposits.
    #[serde(default)]
    pub evm_bridge: Option<Address>,
    /// Last parent-chain block scanned for batches and assertions.
    pub parent_cursor: u64,
    /// Last Arbitrum block scanned for deposits.
    pub deposit_cursor: u64,
    /// Deposit blocks the bridge does not accept yet: number → hash.
    pub pending: BTreeMap<u64, B256>,
    /// Batch ends with proofs, by block number, waiting for an assertion.
    pub batch_ends: BTreeMap<u64, BatchEnd>,
    /// Proofs whose assertion has landed.
    pub ready: Vec<ReadyProof>,
    /// Batches that landed on the parent chain before the Arbitrum node could
    /// place their last block: batch → tries left. Each tick tries them again.
    #[serde(default)]
    pub unplaced_batches: BTreeMap<u64, u32>,
    /// The parent-chain transaction that posted each recent batch
    /// ([`RpcArbitrumView::batch_txs`]), so that after a restart an unplaced
    /// batch's last block still comes from its posting call.
    #[serde(default)]
    pub batch_txs: BTreeMap<u64, B256>,
}

/// How many more ticks a batch is tried when the Arbitrum node cannot place
/// its last block yet, or failed to answer. A public node keeps the state the
/// slot proofs are read from for about 128 blocks, half a minute on Arbitrum,
/// so a later try rarely helps; the next batch covers the same deposits.
pub const BATCH_RETRIES: u32 = 2;

/// Posting transactions kept for the most recent batches: enough for every
/// batch one tick sees after a long stop, and the ticks that try it again.
const BATCH_TXS_KEPT: usize = 400;

impl AnchorState {
    /// Ties the state to `bridge`, the EVM bridge it is run for: a state
    /// recorded for another bridge is refused, one with none takes `bridge`.
    pub fn ensure_bridge(&mut self, bridge: Address) -> Result<(), RelayerError> {
        match self.evm_bridge {
            Some(b) if b != bridge => Err(RelayerError::other(format!(
                "the anchor state is for bridge {b}, not {bridge}: move it away to start the new \
                 bridge from its --from-block"
            ))),
            _ => {
                self.evm_bridge = Some(bridge);
                Ok(())
            },
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>, RelayerError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Write-temp-then-rename with fsync, like the deposit relayer's state.
    pub fn save(&self, path: &Path) -> Result<(), RelayerError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut tmp = PathBuf::from(path);
        tmp.as_mut_os_string().push(".tmp");
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────
// The two chains and Acki Nacki, behind traits
// ─────────────────────────────────────────────────────────────────────

/// An event on the parent chain, in chain order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentEvent {
    Batch {
        block: u64,
        log_index: u64,
        batch: u64,
    },
    Assertion {
        block: u64,
        log_index: u64,
        tx_index: u32,
        end_hash: B256,
    },
}

impl ParentEvent {
    fn position(&self) -> (u64, u64) {
        match self {
            ParentEvent::Batch {
                block,
                log_index,
                ..
            }
            | ParentEvent::Assertion {
                block,
                log_index,
                ..
            } => (*block, *log_index),
        }
    }
}

/// Reads of the Arbitrum chain and its parent chain.
#[async_trait]
pub trait ArbitrumView: Send + Sync {
    async fn arbitrum_head(&self) -> Result<u64, RelayerError>;
    /// Blocks in `from..=to` with a `Deposit` from the bridge: number, hash.
    async fn deposit_blocks(&self, from: u64, to: u64) -> Result<Vec<(u64, B256)>, RelayerError>;
    async fn parent_head(&self) -> Result<u64, RelayerError>;
    /// Batches and assertions in `from..=to`, in chain order.
    async fn parent_events(&self, from: u64, to: u64) -> Result<Vec<ParentEvent>, RelayerError>;
    /// The last block of `batch`, once the Arbitrum node knows the batch.
    async fn batch_end(&self, batch: u64) -> Result<Option<u64>, RelayerError>;
    /// Hash and header RLP of an Arbitrum block.
    async fn arbitrum_header(&self, number: u64) -> Result<(B256, Bytes), RelayerError>;
    async fn arbitrum_number(&self, hash: B256) -> Result<Option<u64>, RelayerError>;
    /// The history slots of blocks `ns` in block `m`'s state, read in one
    /// `eth_getProof`: each block with its slot's value and proof.
    async fn history_slots(
        &self,
        ns: &[u64],
        m: u64,
    ) -> Result<Vec<(u64, B256, SlotProof)>, RelayerError>;
    /// Hash, header RLP and receipt proof of transaction `tx_index` of a
    /// parent-chain block.
    async fn parent_receipt(
        &self,
        block: u64,
        tx_index: u32,
    ) -> Result<(B256, Bytes, Vec<Bytes>), RelayerError>;
    /// A parent-chain block's header; `None` past the head.
    async fn parent_header(&self, number: u64) -> Result<Option<ParentHeader>, RelayerError>;
}

/// A parent-chain block header: what a `submitAncestry` span is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParentHeader {
    pub number: u64,
    pub hash: B256,
    pub rlp: Bytes,
    pub timestamp: u64,
}

/// Arguments of `ArbitrumBlockVerifier.proveBlock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProveBlock {
    pub l1_header: Bytes,
    pub tx_index: u32,
    pub receipt_proof: Vec<Bytes>,
    pub l2_header: Bytes,
    pub account_proof: Vec<Bytes>,
    pub block_number: u64,
    pub storage_proof: Vec<Bytes>,
}

impl ProveBlock {
    /// The call's parameters as the TVM ABI JSON encoder takes them.
    pub fn abi_params(&self) -> serde_json::Value {
        let hex = |b: &Bytes| hex::encode(b);
        let list = |v: &[Bytes]| v.iter().map(hex).collect::<Vec<_>>();
        serde_json::json!({
            "l1Header": hex(&self.l1_header),
            "txIndex": self.tx_index,
            "receiptProof": list(&self.receipt_proof),
            "l2Header": hex(&self.l2_header),
            "accountProof": list(&self.account_proof),
            "blockNumber": self.block_number.to_string(),
            "storageProof": list(&self.storage_proof),
        })
    }
}

/// The bridge, the chain's verifier and the parent chain's light client on
/// Acki Nacki.
#[async_trait]
pub trait AnchorTarget: Send + Sync {
    async fn is_accepted(&self, chain_id: u64, block_hash: B256) -> Result<bool, RelayerError>;
    async fn prove_block(&self, call: &ProveBlock) -> Result<(), RelayerError>;
    /// The beacon slot of the light client's head (`getHead`); `None` when
    /// the bridge has no light client for the parent chain.
    async fn light_client_head_slot(&self) -> Result<Option<u64>, RelayerError>;
    /// `submitAncestry(headerRlps)` on the light client, oldest header first.
    async fn submit_ancestry(&self, headers: &[Bytes]) -> Result<(), RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// One round
// ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TickConfig {
    /// Parent-chain blocks to stay behind its head: a load-balanced RPC can
    /// answer `eth_blockNumber` from a node ahead of the one serving logs.
    pub parent_lag: u64,
    /// Arbitrum blocks to stay behind its head when scanning for deposits, for
    /// the same reason: a deposit a lagging backend leaves out would be behind
    /// the cursor for good.
    pub arbitrum_lag: u64,
    /// Arbitrum blocks scanned for deposits in one tick. The cursor moves
    /// after each window, so a failed read repeats one window, not the whole
    /// range since the last run.
    pub deposit_scan_window: u64,
    /// Seconds before `proveBlock` is sent again for a block the bridge has
    /// not accepted after the first send.
    pub resend_after_secs: u64,
}

impl Default for TickConfig {
    fn default() -> Self {
        Self {
            parent_lag: 1,
            // About a minute at Arbitrum's 0.25 s blocks.
            arbitrum_lag: 240,
            // About seven hours of Arbitrum blocks.
            deposit_scan_window: 100_000,
            resend_after_secs: 300,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub new_deposit_blocks: usize,
    pub batches_taken: usize,
    pub assertions_matched: usize,
    pub assertions_missed: usize,
    pub proofs_sent: usize,
    /// `proveBlock` calls Acki Nacki refused this tick; each is tried again
    /// after `resend_after_secs`, and the other proofs go on.
    pub proofs_failed: usize,
    pub accepted: usize,
    pub waiting_for_parent: usize,
    /// `submitAncestry` spans sent for assertion blocks, and the ones Acki
    /// Nacki refused.
    pub spans_sent: usize,
    pub spans_failed: usize,
}

/// One round: new deposits, new batches and assertions, then every proof
/// whose parent-chain block the bridge accepts. The caller saves `state`.
pub async fn tick<V: ArbitrumView + ?Sized, T: AnchorTarget + ?Sized>(
    chain: &ArbitrumChain,
    view: &V,
    an: &T,
    state: &mut AnchorState,
    cfg: &TickConfig,
) -> Result<TickReport, RelayerError> {
    let mut report = TickReport::default();

    // Deposits, up to `arbitrum_lag` blocks behind the head. A batch seen below
    // may end past that point; a deposit there is waiting by the next batch.
    let head = view.arbitrum_head().await?.saturating_sub(cfg.arbitrum_lag);
    if head > state.deposit_cursor {
        let head = head.min(
            state
                .deposit_cursor
                .saturating_add(cfg.deposit_scan_window.max(1)),
        );
        for (n, hash) in view.deposit_blocks(state.deposit_cursor + 1, head).await? {
            if state.pending.contains_key(&n) || an.is_accepted(chain.chain_id, hash).await? {
                continue;
            }
            info!(block = n, %hash, "deposit block waits for an Arbitrum anchor");
            state.pending.insert(n, hash);
            report.new_deposit_blocks += 1;
        }
        state.deposit_cursor = head;
    }

    // Batches an earlier tick could not place, oldest first, before the new
    // events: an assertion can only end at a batch already taken. A batch
    // leaves the list once it is taken or given up, so a failed read keeps
    // the ones after it for the next tick.
    let unplaced: Vec<(u64, u32)> = state
        .unplaced_batches
        .iter()
        .map(|(&batch, &left)| (batch, left))
        .collect();
    for (batch, left) in unplaced {
        match take_batch(view, state, batch).await? {
            Taken::Proofs => {
                state.unplaced_batches.remove(&batch);
                report.batches_taken += 1;
            },
            Taken::Nothing | Taken::Gone => {
                state.unplaced_batches.remove(&batch);
            },
            Taken::Unplaced if left > 1 => {
                state.unplaced_batches.insert(batch, left - 1);
            },
            Taken::Unplaced => {
                state.unplaced_batches.remove(&batch);
                warn!(
                    batch,
                    "the Arbitrum node still cannot place this batch; given up, the next batch \
                     covers the same deposits"
                );
            },
        }
    }

    // Batches and assertions.
    let parent_head = view.parent_head().await?.saturating_sub(cfg.parent_lag);
    if parent_head > state.parent_cursor {
        let mut events = view
            .parent_events(state.parent_cursor + 1, parent_head)
            .await?;
        events.sort_by_key(ParentEvent::position);
        for event in events {
            // The events of every earlier block are handled: a tick that fails
            // from here on starts again at this block, not at events whose
            // proofs it has already taken.
            state.parent_cursor = state
                .parent_cursor
                .max(event.position().0.saturating_sub(1));
            match event {
                ParentEvent::Batch {
                    batch, ..
                } => match take_batch(view, state, batch).await? {
                    Taken::Proofs => report.batches_taken += 1,
                    Taken::Nothing | Taken::Gone => {},
                    Taken::Unplaced => {
                        state.unplaced_batches.insert(batch, BATCH_RETRIES);
                    },
                },
                ParentEvent::Assertion {
                    block,
                    tx_index,
                    end_hash,
                    ..
                } => {
                    let found = state
                        .batch_ends
                        .values()
                        .find(|e| e.hash == end_hash)
                        .cloned();
                    let end_number = match &found {
                        Some(end) => Some(end.number),
                        None => view.arbitrum_number(end_hash).await?,
                    };
                    match found {
                        Some(end) => {
                            // Read before anything is dropped: if it fails, the
                            // next tick sees this assertion with its proofs.
                            let (parent_hash, parent_header, receipt_proof) =
                                view.parent_receipt(block, tx_index).await?;
                            // Assertions end further on each time: nothing taken
                            // at or before this one can be named again.
                            state.batch_ends.retain(|&n, _| n > end.number);
                            info!(
                                parent_block = block,
                                m = end.number,
                                deposits = end.slots.len(),
                                "assertion ends at a batch with proofs; waiting for the bridge to \
                                 accept its parent-chain block"
                            );
                            state.ready.push(ReadyProof {
                                parent_block: block,
                                parent_hash,
                                parent_header,
                                tx_index,
                                receipt_proof,
                                end,
                                sent: BTreeMap::new(),
                                span_tried: None,
                                spans_sent: 0,
                            });
                            report.assertions_matched += 1;
                        },
                        None => {
                            if let Some(m) = end_number {
                                state.batch_ends.retain(|&n, _| n > m);
                            }
                            let waiting = state.pending.keys().filter(|&&n| Some(n) < end_number);
                            if waiting.count() > 0 {
                                warn!(
                                    parent_block = block,
                                    m = ?end_number,
                                    "assertion ends at a batch this relayer has no proofs for (it \
                                     was down when the batch landed); the next batch covers the \
                                     same deposits"
                                );
                            }
                            report.assertions_missed += 1;
                        },
                    }
                },
            }
        }
        state.parent_cursor = parent_head;
    }

    // Proofs. Each stays in `state.ready` until the bridge accepts all its
    // blocks, so a failed call leaves it for the next tick.
    let now = unix_now();
    for proof in &mut state.ready {
        if !an
            .is_accepted(chain.parent_chain_id, proof.parent_hash)
            .await?
        {
            report.waiting_for_parent += 1;
            if proof
                .span_tried
                .is_none_or(|at| now >= at + cfg.resend_after_secs)
            {
                proof.span_tried = Some(now);
                if let Err(e) = cover_parent_block(chain, view, an, proof, &mut report).await {
                    warn!(
                        parent_block = proof.parent_block,
                        error = %e,
                        retry_in_secs = cfg.resend_after_secs,
                        "could not build the span for the assertion's parent-chain block; \
                         trying again later"
                    );
                }
            }
            continue;
        }
        let blocks: Vec<u64> = proof.end.slots.keys().copied().collect();
        for n in blocks {
            let Some(&hash) = state.pending.get(&n) else {
                proof.end.slots.remove(&n);
                continue;
            };
            if an.is_accepted(chain.chain_id, hash).await? {
                info!(block = n, %hash, "the bridge accepts the deposit block");
                state.pending.remove(&n);
                proof.end.slots.remove(&n);
                report.accepted += 1;
                continue;
            }
            if proof
                .sent
                .get(&n)
                .is_some_and(|&at| now < at + cfg.resend_after_secs)
            {
                continue;
            }
            let slot = &proof.end.slots[&n];
            let call = ProveBlock {
                l1_header: proof.parent_header.clone(),
                tx_index: proof.tx_index,
                receipt_proof: proof.receipt_proof.clone(),
                l2_header: proof.end.header.clone(),
                account_proof: slot.account_proof.clone(),
                block_number: n,
                storage_proof: slot.storage_proof.clone(),
            };
            // Sent or refused, it waits `resend_after_secs` before the next
            // try, and the proofs after it go on: one proof Acki Nacki keeps
            // refusing must not hold up every other deposit.
            match an.prove_block(&call).await {
                Ok(()) => {
                    info!(block = n, m = proof.end.number, "proveBlock sent");
                    report.proofs_sent += 1;
                },
                Err(e) => {
                    warn!(
                        block = n,
                        m = proof.end.number,
                        error = %e,
                        retry_in_secs = cfg.resend_after_secs,
                        "proveBlock refused; trying again later, the other proofs go on"
                    );
                    report.proofs_failed += 1;
                },
            }
            proof.sent.insert(n, now);
        }
    }
    state.ready.retain(|p| !p.end.slots.is_empty());
    Ok(report)
}

/// Most `submitAncestry` calls sent for one assertion block. One is sent only
/// once the bridge accepts both ends of the span, so a refusal is a node or
/// contract fault a resend does not fix, and each costs the light client gas.
pub const MAX_SPAN_SENDS: u32 = 3;

/// Beacon slot length and epoch length, in seconds and slots.
const SECONDS_PER_SLOT: u64 = 12;
const SLOTS_PER_EPOCH: u64 = 32;

/// Blocks tried as each end of a span: the last ones at or before an epoch
/// boundary. Before Gloas the light client's checkpoint is the block of the
/// boundary slot; from Gloas on, the payload bid's parent, the block of the
/// slot before it. The third covers a slot with no block.
const ANCHOR_CANDIDATES: usize = 3;

/// Sends `submitAncestry` for the epoch of `proof.parent_block`, between the
/// light client's checkpoints on either side of it. Waits, sending nothing,
/// until the light client's head has passed the epoch and the bridge accepts
/// both checkpoints.
async fn cover_parent_block<V: ArbitrumView + ?Sized, T: AnchorTarget + ?Sized>(
    chain: &ArbitrumChain,
    view: &V,
    an: &T,
    proof: &mut ReadyProof,
    report: &mut TickReport,
) -> Result<(), RelayerError> {
    if proof.spans_sent >= MAX_SPAN_SENDS {
        return Ok(());
    }
    let b = proof.parent_block;
    let Some(block) = view.parent_header(b).await? else {
        return Ok(());
    };
    if block.hash != proof.parent_hash {
        return Err(RelayerError::other(format!(
            "parent block {b} is {} now, not the {} the assertion was read from",
            block.hash, proof.parent_hash
        )));
    }
    let genesis = chain.parent_beacon_genesis;
    let epoch_secs = SECONDS_PER_SLOT * SLOTS_PER_EPOCH;
    let since = block.timestamp.checked_sub(genesis).ok_or_else(|| {
        RelayerError::other(format!("parent block {b} is older than the beacon genesis"))
    })?;
    let t0 = genesis + since / epoch_secs * epoch_secs;
    let t1 = t0 + epoch_secs;
    // The checkpoint that closes the epoch is proven no earlier than the head
    // reaches the epoch's end: reading the head first spares the walk below
    // while the light client is still behind.
    let Some(head) = an.light_client_head_slot().await? else {
        return Ok(());
    };
    if head < (t1 - genesis) / SECONDS_PER_SLOT {
        return Ok(());
    }
    // The blocks after `b` up to the end of its epoch, oldest first.
    let mut after: Vec<ParentHeader> = Vec::new();
    while after.len() < SLOTS_PER_EPOCH as usize {
        let Some(h) = view.parent_header(b + 1 + after.len() as u64).await? else {
            break;
        };
        if h.timestamp > t1 {
            break;
        }
        after.push(h);
    }
    // The blocks before `b`, newest first, until enough of them sit at or
    // before the epoch's start.
    let mut before: Vec<ParentHeader> = Vec::new();
    while before.len() <= SLOTS_PER_EPOCH as usize
        && before.iter().filter(|h| h.timestamp <= t0).count() < ANCHOR_CANDIDATES
        && (before.len() as u64) < b
    {
        let Some(h) = view.parent_header(b - 1 - before.len() as u64).await? else {
            break;
        };
        before.push(h);
    }
    let mut newer = None;
    for (i, h) in after.iter().enumerate().rev().take(ANCHOR_CANDIDATES) {
        if an.is_accepted(chain.parent_chain_id, h.hash).await? {
            newer = Some(i);
            break;
        }
    }
    let mut older = None;
    for (i, h) in before
        .iter()
        .enumerate()
        .filter(|(_, h)| h.timestamp <= t0)
        .take(ANCHOR_CANDIDATES)
    {
        if an.is_accepted(chain.parent_chain_id, h.hash).await? {
            older = Some(i);
            break;
        }
    }
    let (Some(older), Some(newer)) = (older, newer) else {
        info!(
            parent_block = b,
            newer_found = newer.is_some(),
            older_found = older.is_some(),
            "the bridge does not accept the checkpoints around the assertion's parent-chain block \
             yet; waiting for the light client"
        );
        return Ok(());
    };
    let headers: Vec<Bytes> = before[..=older]
        .iter()
        .rev()
        .chain(std::iter::once(&block))
        .chain(&after[..=newer])
        .map(|h| h.rlp.clone())
        .collect();
    let rlps: Vec<Vec<u8>> = headers.iter().map(|h| h.to_vec()).collect();
    link_headers(&rlps)?;
    match an.submit_ancestry(&headers).await {
        Ok(()) => {
            info!(
                parent_block = b,
                from = before[older].number,
                to = after[newer].number,
                "submitAncestry sent for the assertion's parent-chain block"
            );
            report.spans_sent += 1;
        },
        Err(e) => {
            warn!(
                parent_block = b,
                error = %e,
                "submitAncestry refused"
            );
            report.spans_failed += 1;
        },
    }
    proof.spans_sent += 1;
    if proof.spans_sent >= MAX_SPAN_SENDS {
        warn!(
            parent_block = b,
            sent = proof.spans_sent,
            "no more submitAncestry for this block: the bridge still does not accept it; it needs \
             the owner's setAcceptedBlockHash"
        );
    }
    Ok(())
}

/// What [`take_batch`] did with a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Taken {
    /// Slot proofs were taken at its last block.
    Proofs,
    /// No waiting deposit block is covered by it.
    Nothing,
    /// The Arbitrum node cannot place its last block yet, or failed to
    /// answer for it.
    Unplaced,
    /// The node no longer has the state of its last block: asking again
    /// cannot help.
    Gone,
}

/// The node answered that it no longer keeps that block's state.
fn state_gone(e: &RelayerError) -> bool {
    let t = e.to_string();
    (t.contains("historical state") && t.contains("not available"))
        || t.contains("missing trie node")
}

/// Takes the slot proofs of the waiting deposit blocks at the last block of
/// `batch`. With no deposit block waiting, the batch is not looked up.
async fn take_batch<V: ArbitrumView + ?Sized>(
    view: &V,
    state: &mut AnchorState,
    batch: u64,
) -> Result<Taken, RelayerError> {
    if state.pending.is_empty() {
        return Ok(Taken::Nothing);
    }
    let Some(m) = view.batch_end(batch).await? else {
        warn!(
            batch,
            "the Arbitrum node does not know where this batch ends yet; trying again on the next \
             ticks"
        );
        return Ok(Taken::Unplaced);
    };
    // M's state holds the hashes of blocks M - HISTORY_WINDOW ..= M - 1. A
    // deposit block older than that is out of every later batch's reach too.
    let expired: Vec<(u64, B256)> = state
        .pending
        .iter()
        .filter(|&(&n, _)| n < m && m - n > HISTORY_WINDOW && !proof_held(state, n))
        .map(|(&n, &h)| (n, h))
        .collect();
    for (n, hash) in expired {
        error!(
            block = n,
            %hash,
            m,
            "deposit block is older than the {HISTORY_WINDOW} blocks an Arbitrum block's state \
             remembers: arbitrum-anchors can no longer prove it and stops trying; the deposit now \
             needs the bridge owner"
        );
        state.pending.remove(&n);
    }
    let wanted: Vec<(u64, B256)> = state
        .pending
        .iter()
        .filter(|&(&n, _)| n < m && m - n <= HISTORY_WINDOW)
        .map(|(&n, &h)| (n, h))
        .collect();
    if wanted.is_empty() {
        return Ok(Taken::Nothing);
    }
    // A read that fails here leaves this batch for a later tick, never the
    // whole tick: the tick's other events, and its cursor, go on.
    let unprovable = |e: RelayerError| {
        if state_gone(&e) {
            warn!(
                batch,
                m,
                error = %e,
                "the Arbitrum node no longer has the state of this batch's last block; the next \
                 batch covers the same deposits"
            );
            return Taken::Gone;
        }
        warn!(
            batch,
            m,
            error = %e,
            "the Arbitrum node cannot prove this batch's last block now; trying again on the next \
             ticks"
        );
        Taken::Unplaced
    };
    let (hash, header) = match view.arbitrum_header(m).await {
        Ok(h) => h,
        Err(e) => return Ok(unprovable(e)),
    };
    // One read for every waiting slot: the node keeps M's state for seconds,
    // and reads one after another could straddle its end.
    let ns: Vec<u64> = wanted.iter().map(|&(n, _)| n).collect();
    let read = match view.history_slots(&ns, m).await {
        Ok(v) => v,
        Err(e) => return Ok(unprovable(e)),
    };
    let wanted: BTreeMap<u64, B256> = wanted.into_iter().collect();
    let mut slots = BTreeMap::new();
    for (n, value, proof) in read {
        let Some(&want) = wanted.get(&n) else {
            continue;
        };
        if value != want {
            warn!(block = n, m, %value, %want, "history slot does not hold the deposit block's hash");
            continue;
        }
        slots.insert(n, proof);
    }
    info!(
        batch,
        m,
        deposits = slots.len(),
        "batch landed; proofs taken at its last block"
    );
    state.batch_ends.insert(m, BatchEnd {
        number: m,
        hash,
        header,
        slots,
    });
    Ok(Taken::Proofs)
}

/// A slot proof for block `n` is already taken: past the history window it
/// can no longer be taken again, but the one held stays valid.
fn proof_held(state: &AnchorState, n: u64) -> bool {
    state.batch_ends.values().any(|e| e.slots.contains_key(&n))
        || state.ready.iter().any(|p| p.end.slots.contains_key(&n))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────
// Live view over two JSON-RPC endpoints
// ─────────────────────────────────────────────────────────────────────

pub struct RpcArbitrumView<P, A> {
    pub parent: P,
    pub arbitrum: A,
    pub chain: ArbitrumChain,
    /// `AckiNackiBridge` on the Arbitrum chain, whose `Deposit`s are anchored.
    pub evm_bridge: Address,
    /// Blocks per `eth_getLogs` on each chain.
    pub parent_log_span: u64,
    pub arbitrum_log_span: u64,
    /// The parent-chain transaction that posted each batch seen: where
    /// [`batch_end_from_calldata`] reads the batch's last block from.
    pub batch_txs: std::sync::Mutex<BTreeMap<u64, B256>>,
}

fn eth<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> RelayerError + '_ {
    move |e| RelayerError::Eth(format!("{what}: {e}"))
}

impl<P: Provider, A: Provider> RpcArbitrumView<P, A> {
    async fn logs<Q: Provider>(
        provider: &Q,
        filter: Filter,
        from: u64,
        to: u64,
        span: u64,
    ) -> Result<Vec<alloy::rpc::types::Log>, RelayerError> {
        let mut out = Vec::new();
        let mut start = from;
        while start <= to {
            let end = to.min(start + span.max(1) - 1);
            let chunk = filter.clone().from_block(start).to_block(end);
            out.extend(get_logs_with_retry(provider, &chunk).await?);
            start = end + 1;
        }
        Ok(out)
    }

    /// The batch that holds block `number`; `None` when no posted batch holds
    /// it yet. Any other failure is an error: read as "not yet", it would
    /// settle the search below the batch's last block.
    async fn batch_of(&self, number: u64) -> Result<Option<u64>, RelayerError> {
        let call = findBatchContainingBlockCall {
            blockNum: number,
        };
        let tx = TransactionRequest::default()
            .to(NODE_INTERFACE)
            .input(call.abi_encode().into());
        match self.arbitrum.call(tx).await {
            Ok(out) => findBatchContainingBlockCall::abi_decode_returns(&out)
                .map(Some)
                .map_err(eth("NodeInterface findBatchContainingBlock")),
            Err(e) if not_in_a_batch_yet(&e) => Ok(None),
            Err(e) => Err(eth("NodeInterface findBatchContainingBlock")(e)),
        }
    }
}

/// The node refused `findBatchContainingBlock` because no posted batch
/// holds the block yet: the call reverts. A rate limit, a timeout or a
/// dropped connection is the RPC's failure, not an answer.
fn not_in_a_batch_yet(e: &alloy::transports::TransportError) -> bool {
    e.as_error_resp()
        .is_some_and(|p| p.message.contains("revert"))
}

#[async_trait]
impl<P: Provider, A: Provider> ArbitrumView for RpcArbitrumView<P, A> {
    async fn arbitrum_head(&self) -> Result<u64, RelayerError> {
        self.arbitrum
            .get_block_number()
            .await
            .map_err(eth("Arbitrum eth_blockNumber"))
    }

    async fn deposit_blocks(&self, from: u64, to: u64) -> Result<Vec<(u64, B256)>, RelayerError> {
        let filter = Filter::new()
            .address(self.evm_bridge)
            .event_signature(DEPOSIT);
        let logs = Self::logs(&self.arbitrum, filter, from, to, self.arbitrum_log_span).await?;
        let mut blocks: Vec<(u64, B256)> = logs
            .iter()
            .filter_map(|l| Some((l.block_number?, l.block_hash?)))
            .collect();
        blocks.dedup();
        Ok(blocks)
    }

    async fn parent_head(&self) -> Result<u64, RelayerError> {
        self.parent
            .get_block_number()
            .await
            .map_err(eth("parent eth_blockNumber"))
    }

    async fn parent_events(&self, from: u64, to: u64) -> Result<Vec<ParentEvent>, RelayerError> {
        let span = self.parent_log_span;
        let batches = Filter::new()
            .address(self.chain.sequencer_inbox)
            .event_signature(SEQUENCER_BATCH_DELIVERED);
        let assertions = Filter::new()
            .address(self.chain.rollup)
            .event_signature(ASSERTION_CREATED);
        let mut events = Vec::new();
        for log in Self::logs(&self.parent, batches, from, to, span).await? {
            let (Some(block), Some(log_index), Some(topic)) =
                (log.block_number, log.log_index, log.topics().get(1))
            else {
                continue;
            };
            let batch: u64 = U256::from_be_bytes(topic.0).saturating_to();
            if let Some(tx) = log.transaction_hash {
                let mut txs = self.batch_txs.lock().unwrap_or_else(|p| p.into_inner());
                txs.insert(batch, tx);
                // Kept long enough for the retries of an unplaced batch.
                while txs.len() > BATCH_TXS_KEPT {
                    txs.pop_first();
                }
            }
            events.push(ParentEvent::Batch {
                block,
                log_index,
                batch,
            });
        }
        for log in Self::logs(&self.parent, assertions, from, to, span).await? {
            let (Some(block), Some(log_index), Some(tx_index), Some(end_hash)) = (
                log.block_number,
                log.log_index,
                log.transaction_index,
                assertion_end_hash(&log.data().data),
            ) else {
                continue;
            };
            events.push(ParentEvent::Assertion {
                block,
                log_index,
                tx_index: u32::try_from(tx_index).map_err(eth("transaction index"))?,
                end_hash,
            });
        }
        Ok(events)
    }

    async fn batch_end(&self, batch: u64) -> Result<Option<u64>, RelayerError> {
        // From the call that posted the batch, when it is one this relayer
        // knows; otherwise from the Arbitrum node, below.
        let tx = self
            .batch_txs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&batch)
            .copied();
        if let Some(tx) = tx {
            let posted = self
                .parent
                .get_transaction_by_hash(tx)
                .await
                .map_err(eth("parent eth_getTransactionByHash"))?;
            if let Some(m) = posted.and_then(|t| {
                use alloy::consensus::Transaction as _;
                batch_end_from_calldata(t.input(), batch, self.chain.nitro_genesis_block)
            }) {
                return Ok(Some(m));
            }
        }
        // The last block whose batch is at most `batch`; blocks not in a
        // posted batch yet make the NodeInterface call fail.
        let head = self.arbitrum_head().await?;
        let (mut lo, mut hi) = (head.saturating_sub(50_000), head);
        if self.batch_of(lo).await?.is_none_or(|b| b > batch) {
            return Ok(None);
        }
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            match self.batch_of(mid).await? {
                Some(b) if b <= batch => lo = mid,
                _ => hi = mid - 1,
            }
        }
        Ok((self.batch_of(lo).await? == Some(batch)).then_some(lo))
    }

    async fn arbitrum_header(&self, number: u64) -> Result<(B256, Bytes), RelayerError> {
        let block = self
            .arbitrum
            .get_block_by_number(number.into())
            .await
            .map_err(eth("Arbitrum eth_getBlockByNumber"))?
            .ok_or_else(|| RelayerError::Eth(format!("Arbitrum block {number} not found")))?;
        Ok((block.header.hash, encode_header_rlp(&block.header)?))
    }

    async fn arbitrum_number(&self, hash: B256) -> Result<Option<u64>, RelayerError> {
        let block = self
            .arbitrum
            .get_block_by_hash(hash)
            .await
            .map_err(eth("Arbitrum eth_getBlockByHash"))?;
        Ok(block.map(|b| b.header.inner.number))
    }

    async fn history_slots(
        &self,
        ns: &[u64],
        m: u64,
    ) -> Result<Vec<(u64, B256, SlotProof)>, RelayerError> {
        let keys: Vec<B256> = ns
            .iter()
            .map(|&n| B256::from(U256::from(n % HISTORY_WINDOW)))
            .collect();
        let proof = self
            .arbitrum
            .get_proof(HISTORY_ADDRESS, keys)
            .block_id(m.into())
            .await
            .map_err(eth("Arbitrum eth_getProof"))?;
        if proof.storage_proof.len() != ns.len() {
            return Err(RelayerError::Eth(format!(
                "eth_getProof returned {} storage proofs for {} slots",
                proof.storage_proof.len(),
                ns.len()
            )));
        }
        Ok(ns
            .iter()
            .zip(proof.storage_proof)
            .map(|(&n, storage)| {
                (n, B256::from(storage.value), SlotProof {
                    account_proof: proof.account_proof.clone(),
                    storage_proof: storage.proof,
                })
            })
            .collect())
    }

    async fn parent_receipt(
        &self,
        block: u64,
        tx_index: u32,
    ) -> Result<(B256, Bytes, Vec<Bytes>), RelayerError> {
        let header = self
            .parent
            .get_block_by_number(block.into())
            .await
            .map_err(eth("parent eth_getBlockByNumber"))?
            .ok_or_else(|| RelayerError::Eth(format!("parent block {block} not found")))?
            .header;
        let receipts: Vec<TransactionReceipt> = self
            .parent
            .get_block_receipts(block.into())
            .await
            .map_err(eth("parent eth_getBlockReceipts"))?
            .ok_or_else(|| RelayerError::Eth(format!("no receipts for parent block {block}")))?;
        let envelopes: Vec<ReceiptEnvelope> = receipts
            .into_iter()
            .map(|r| r.inner.into_primitives_receipt())
            .collect();
        let (root, proof) = receipt_proof(&envelopes, tx_index as usize);
        if root != header.inner.receipts_root {
            return Err(RelayerError::Eth(format!(
                "the receipts of parent block {block} do not build its receiptsRoot"
            )));
        }
        Ok((header.hash, encode_header_rlp(&header)?, proof))
    }

    async fn parent_header(&self, number: u64) -> Result<Option<ParentHeader>, RelayerError> {
        let Some(block) = self
            .parent
            .get_block_by_number(number.into())
            .await
            .map_err(eth("parent eth_getBlockByNumber"))?
        else {
            return Ok(None);
        };
        Ok(Some(ParentHeader {
            number,
            hash: block.header.hash,
            rlp: encode_header_rlp(&block.header)?,
            timestamp: block.header.inner.timestamp,
        }))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Live Acki Nacki target
// ─────────────────────────────────────────────────────────────────────

/// The bridge's getters, the chain's verifier and the parent chain's light
/// client over tvm_client.
#[cfg(feature = "live-submit")]
pub struct TvmAnchorTarget {
    bridge: TvmAckiNacki,
    verifier: TvmAckiNacki,
    bridge_address: ExtendedAddress,
    verifier_address: ExtendedAddress,
    light_client: TvmAckiNacki,
    parent_chain_id: u64,
    /// The parent chain's light client, once the bridge names one
    /// (`getLightClient`). Asked again until it does: the relayer may start
    /// before `deployLightClient`.
    light_client_address: std::sync::Mutex<Option<ExtendedAddress>>,
}

/// A uint160 address getter value, hex or decimal.
#[cfg(feature = "live-submit")]
fn abi_address(v: &serde_json::Value) -> Option<Address> {
    let s = v.as_str()?;
    let n = match s.strip_prefix("0x") {
        Some(hex) => U256::from_str_radix(hex, 16).ok()?,
        None => U256::from_str_radix(s, 10).ok()?,
    };
    (n >> 160usize)
        .is_zero()
        .then(|| Address::from_word(n.to_be_bytes::<32>().into()))
}

/// A uint the TVM ABI decoder rendered, decimal or `0x` hex.
#[cfg(feature = "live-submit")]
fn abi_uint(v: &serde_json::Value) -> Option<u128> {
    let s = v.as_str()?;
    match s.strip_prefix("0x") {
        Some(hex) => u128::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

#[cfg(feature = "live-submit")]
impl TvmAnchorTarget {
    /// Connects, and finds the verifier the bridge recorded for `chain`
    /// (`getL2Verifier`). Refuses a chain without one, or one recorded for
    /// another parent chain.
    pub async fn connect(
        graphql_url: &str,
        keys: KeyPair,
        bridge_address: ExtendedAddress,
        chain: &ArbitrumChain,
    ) -> Result<Self, RelayerError> {
        // The ABIs are built in (`crate::abi`): the bridge's for the
        // getters and `isAcceptedBlockHash`, the verifier's for `proveBlock`.
        let client = |abi: tvm_client::abi::Abi| -> Result<TvmAckiNacki, RelayerError> {
            Ok(TvmAckiNacki::connect(TvmClientConfig {
                graphql_endpoints: vec![graphql_url.to_string()],
                keys: keys.clone(),
                bridge_abi: abi,
            })?)
        };
        let bridge = client(crate::abi::bridge_abi())?;
        let verifier = client(crate::abi::arbitrum_verifier_abi())?;
        let got = bridge
            .run_getter(
                &bridge_address.to_string(),
                "getL2Verifier",
                serde_json::json!({ "l2ChainId": chain.chain_id.to_string() }),
            )
            .await?;
        let parent = got.get("parentChainId").and_then(abi_uint);
        let account = got
            .get("verifier")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if parent != Some(u128::from(chain.parent_chain_id)) || !account.starts_with("0:") {
            return Err(RelayerError::other(format!(
                "the bridge has no verifier for {} ({}) on parent chain {}: getL2Verifier \
                 answered {got}; run deployL2Verifier first",
                chain.name, chain.chain_id, chain.parent_chain_id
            )));
        }
        let verifier_address =
            ExtendedAddress::from_legacy(account, Some(bridge_address.dapp_id()))?;
        // A verifier initialized for another rollup or parent chain refuses
        // every proveBlock (307): say so now, not once per tick.
        let cfg = verifier
            .run_getter(
                &verifier_address.to_string(),
                "getConfig",
                serde_json::json!({}),
            )
            .await?;
        let checks = (
            cfg.get("l2ChainId").and_then(abi_uint),
            cfg.get("l1ChainId").and_then(abi_uint),
            cfg.get("rollup").and_then(abi_address),
        );
        if checks
            != (
                Some(u128::from(chain.chain_id)),
                Some(u128::from(chain.parent_chain_id)),
                Some(chain.rollup),
            )
        {
            return Err(RelayerError::other(format!(
                "the verifier {verifier_address} of {} has getConfig {cfg}, not chain {} on \
                 parent chain {} with rollup {}: every proveBlock would be refused. Deploy a \
                 verifier for the rollup {} posts to (SequencerInbox.rollup())",
                chain.name, chain.chain_id, chain.parent_chain_id, chain.rollup, chain.name
            )));
        }
        info!(verifier = %verifier_address, "ArbitrumBlockVerifier of {}", chain.name);
        let target = Self {
            bridge,
            verifier,
            bridge_address,
            verifier_address,
            light_client: client(crate::abi::light_client_abi())?,
            parent_chain_id: chain.parent_chain_id,
            light_client_address: std::sync::Mutex::new(None),
        };
        if target.light_client_address().await?.is_none() {
            warn!(
                "the bridge has no light client for parent chain {} yet: until it has, no \
                 submitAncestry is sent and the assertions' blocks need the owner's \
                 setAcceptedBlockHash",
                chain.parent_chain_id
            );
        }
        Ok(target)
    }

    /// The parent chain's light client, which the spans for the assertions'
    /// blocks go to; `None` while the bridge has none.
    async fn light_client_address(&self) -> Result<Option<ExtendedAddress>, RelayerError> {
        if let Some(address) = self
            .light_client_address
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return Ok(Some(address));
        }
        let got = self
            .bridge
            .run_getter(
                &self.bridge_address.to_string(),
                "getLightClient",
                serde_json::json!({ "l1ChainId": self.parent_chain_id.to_string() }),
            )
            .await?;
        let account = got
            .get("value0")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !account.starts_with("0:") {
            return Ok(None);
        }
        let address = ExtendedAddress::from_legacy(account, Some(self.bridge_address.dapp_id()))?;
        info!(light_client = %address, "EthBeaconLightClient of parent chain {}", self.parent_chain_id);
        *self
            .light_client_address
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(address.clone());
        Ok(Some(address))
    }
}

#[cfg(feature = "live-submit")]
#[async_trait]
impl AnchorTarget for TvmAnchorTarget {
    async fn is_accepted(&self, chain_id: u64, block_hash: B256) -> Result<bool, RelayerError> {
        let out = self
            .bridge
            .run_getter(
                &self.bridge_address.to_string(),
                "isAcceptedBlockHash",
                serde_json::json!({ "chainId": chain_id.to_string(), "blockHash": block_hash.to_string() }),
            )
            .await?;
        match out.get("value0") {
            Some(serde_json::Value::Bool(b)) => Ok(*b),
            Some(serde_json::Value::String(s)) => Ok(s == "true"),
            _ => Err(RelayerError::AckiNacki(format!(
                "isAcceptedBlockHash answered {out}"
            ))),
        }
    }

    async fn prove_block(&self, call: &ProveBlock) -> Result<(), RelayerError> {
        // External and permissionless: the hashes are the authorization, and
        // the verifier pays its own gas. The keys only sign the message.
        self.verifier
            .call_contract(ContractCallRequest {
                from: self.verifier_address.clone(),
                to: self.verifier_address.clone(),
                function: "proveBlock".to_string(),
                params: call.abi_params(),
            })
            .await?;
        Ok(())
    }

    async fn light_client_head_slot(&self) -> Result<Option<u64>, RelayerError> {
        let Some(address) = self.light_client_address().await? else {
            return Ok(None);
        };
        let out = self
            .light_client
            .run_getter(&address.to_string(), "getHead", serde_json::json!({}))
            .await?;
        let slot = out
            .get("finalizedSlot")
            .and_then(abi_uint)
            .and_then(|s| u64::try_from(s).ok())
            .ok_or_else(|| RelayerError::AckiNacki(format!("getHead answered {out}")))?;
        Ok(Some(slot))
    }

    async fn submit_ancestry(&self, headers: &[Bytes]) -> Result<(), RelayerError> {
        let Some(address) = self.light_client_address().await? else {
            return Err(RelayerError::other(
                "the bridge has no light client for the parent chain",
            ));
        };
        // External and permissionless, like `proveBlock`: the parent-hash
        // links are the authorization, and the light client pays its gas.
        let header_rlps: Vec<String> = headers.iter().map(hex::encode).collect();
        self.light_client
            .call_contract(ContractCallRequest {
                from: address.clone(),
                to: address.clone(),
                function: "submitAncestry".to_string(),
                params: serde_json::json!({ "headerRlps": header_rlps }),
            })
            .await?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────
// Test vectors (`eth-lc-relayer arbitrum-vector`)
// ─────────────────────────────────────────────────────────────────────

/// Stands in for the bridge when writing a test vector: no block is accepted,
/// so `tick` takes proofs and matches assertions but never sends.
pub struct NoBridge;

#[async_trait]
impl AnchorTarget for NoBridge {
    async fn is_accepted(&self, _: u64, _: B256) -> Result<bool, RelayerError> {
        Ok(false)
    }
    async fn prove_block(&self, _: &ProveBlock) -> Result<(), RelayerError> {
        Err(RelayerError::other("arbitrum-vector sends nothing"))
    }
    async fn light_client_head_slot(&self) -> Result<Option<u64>, RelayerError> {
        Ok(None)
    }
    async fn submit_ancestry(&self, _: &[Bytes]) -> Result<(), RelayerError> {
        Err(RelayerError::other("arbitrum-vector sends nothing"))
    }
}

/// Wraps a view so the one "deposit" is block `head - behind` at the first
/// scan, for a vector without a real deposit.
pub struct OneBlockBehind<V> {
    pub inner: V,
    pub behind: u64,
    pub taken: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl<V: ArbitrumView> ArbitrumView for OneBlockBehind<V> {
    async fn arbitrum_head(&self) -> Result<u64, RelayerError> {
        self.inner.arbitrum_head().await
    }
    async fn deposit_blocks(&self, _from: u64, to: u64) -> Result<Vec<(u64, B256)>, RelayerError> {
        if self.taken.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Ok(vec![]);
        }
        let n = to.saturating_sub(self.behind);
        let (hash, _) = self.inner.arbitrum_header(n).await?;
        Ok(vec![(n, hash)])
    }
    async fn parent_head(&self) -> Result<u64, RelayerError> {
        self.inner.parent_head().await
    }
    async fn parent_events(&self, from: u64, to: u64) -> Result<Vec<ParentEvent>, RelayerError> {
        self.inner.parent_events(from, to).await
    }
    async fn batch_end(&self, batch: u64) -> Result<Option<u64>, RelayerError> {
        self.inner.batch_end(batch).await
    }
    async fn arbitrum_header(&self, number: u64) -> Result<(B256, Bytes), RelayerError> {
        self.inner.arbitrum_header(number).await
    }
    async fn arbitrum_number(&self, hash: B256) -> Result<Option<u64>, RelayerError> {
        self.inner.arbitrum_number(hash).await
    }
    async fn history_slots(
        &self,
        ns: &[u64],
        m: u64,
    ) -> Result<Vec<(u64, B256, SlotProof)>, RelayerError> {
        self.inner.history_slots(ns, m).await
    }
    async fn parent_receipt(
        &self,
        block: u64,
        tx_index: u32,
    ) -> Result<(B256, Bytes, Vec<Bytes>), RelayerError> {
        self.inner.parent_receipt(block, tx_index).await
    }
    async fn parent_header(&self, number: u64) -> Result<Option<ParentHeader>, RelayerError> {
        self.inner.parent_header(number).await
    }
}

/// A ready proof in the format of `scripts/l2-block-proof-bench/fixtures/`,
/// which the bench (`VECTORS=`) and `test_arbitrum_block_proof.py`
/// (`L2_VECTORS=`) take.
pub fn vector_json(
    chain: &ArbitrumChain,
    proof: &ReadyProof,
    n: u64,
    n_hash: B256,
) -> serde_json::Value {
    let slot = &proof.end.slots[&n];
    let call = ProveBlock {
        l1_header: proof.parent_header.clone(),
        tx_index: proof.tx_index,
        receipt_proof: proof.receipt_proof.clone(),
        l2_header: proof.end.header.clone(),
        account_proof: slot.account_proof.clone(),
        block_number: n,
        storage_proof: slot.storage_proof.clone(),
    };
    let mut prove = call.abi_params();
    prove["blockNumber"] = n.into();
    serde_json::json!({
        "l2ChainId": chain.chain_id,
        "l1ChainId": chain.parent_chain_id,
        "rollup": format!("{:#x}", chain.rollup),
        "l1BlockNumber": proof.parent_block,
        "mBlockNumber": proof.end.number,
        "mBlockHash": proof.end.hash,
        "proveBlock": prove,
        "expect": { "parentBlockHash": proof.parent_hash, "blockHash": n_hash },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use alloy::{primitives::keccak256, rpc::types::Header as RpcHeader};

    use super::*;

    fn fixture(rel: &str) -> serde_json::Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        serde_json::from_slice(
            &fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .unwrap()
    }

    fn live_steps() -> serde_json::Value {
        fixture("../../scripts/l2-block-proof-bench/fixtures/live_steps.json")
    }

    fn hex_of(v: &serde_json::Value) -> String {
        v.as_str().unwrap().trim_start_matches("0x").to_string()
    }

    #[test]
    fn the_deposit_topic_is_the_bridge_event() {
        assert_eq!(
            DEPOSIT,
            keccak256("Deposit(uint256,address,uint256,bytes32,uint256,address)")
        );
    }

    #[test]
    fn a_sepolia_header_encodes_to_the_bench_vector() {
        let f = fixture("src/fixtures/arbitrum/sepolia_11991511.json");
        let header: RpcHeader = serde_json::from_value(f["block"].clone()).unwrap();
        let rlp = encode_header_rlp(&header).unwrap();
        assert_eq!(
            hex::encode(rlp),
            hex_of(&live_steps()["assertionBlock"]["args"]["l1Header"])
        );
    }

    #[test]
    fn an_arbitrum_header_encodes_to_the_bench_vector() {
        let f = fixture("src/fixtures/arbitrum/arbitrum_one_518174193.json");
        let header: RpcHeader = serde_json::from_value(f["block"].clone()).unwrap();
        let rlp = encode_header_rlp(&header).unwrap();
        assert_eq!(
            hex::encode(rlp),
            hex_of(&live_steps()["ancestorHash"]["args"]["l2Header"])
        );
    }

    #[test]
    fn a_header_that_does_not_hash_to_its_block_is_refused() {
        let f = fixture("src/fixtures/arbitrum/sepolia_11991511.json");
        let mut header: RpcHeader = serde_json::from_value(f["block"].clone()).unwrap();
        header.inner.gas_used += 1;
        assert!(encode_header_rlp(&header).is_err());
    }

    #[test]
    fn the_receipt_proof_matches_the_bench_vector_and_the_header() {
        let f = fixture("src/fixtures/arbitrum/sepolia_11991511.json");
        let header: RpcHeader = serde_json::from_value(f["block"].clone()).unwrap();
        let receipts: Vec<TransactionReceipt> =
            serde_json::from_value(f["receipts"].clone()).unwrap();
        let envelopes: Vec<ReceiptEnvelope> = receipts
            .into_iter()
            .map(|r| r.inner.into_primitives_receipt())
            .collect();
        let steps = live_steps();
        let index = steps["assertionBlock"]["args"]["txIndex"].as_u64().unwrap() as usize;
        let (root, proof) = receipt_proof(&envelopes, index);
        assert_eq!(root, header.inner.receipts_root);
        let want: Vec<String> = steps["assertionBlock"]["args"]["receiptProof"]
            .as_array()
            .unwrap()
            .iter()
            .map(hex_of)
            .collect();
        assert_eq!(proof.iter().map(hex::encode).collect::<Vec<_>>(), want);
    }

    #[test]
    fn the_assertion_names_the_block_the_bench_vector_proves() {
        let f = fixture("src/fixtures/arbitrum/sepolia_11991511.json");
        let receipts: Vec<TransactionReceipt> =
            serde_json::from_value(f["receipts"].clone()).unwrap();
        let chain = arbitrum_chain(421_614).unwrap();
        let log = receipts
            .iter()
            .flat_map(|r| r.inner.logs())
            .find(|l| l.address() == chain.rollup && l.topics().first() == Some(&ASSERTION_CREATED))
            .expect("the block holds the assertion");
        let want: B256 = live_steps()["assertionBlock"]["expect"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(assertion_end_hash(&log.data().data), Some(want));
    }

    // ── the state machine, over fakes ──────────────────────────────────

    const ARB: &ArbitrumChain = &ARBITRUM_CHAINS[0];

    fn h(tag: u64) -> B256 {
        keccak256(tag.to_be_bytes())
    }

    #[derive(Default)]
    struct FakeChain {
        arbitrum_head: u64,
        parent_head: u64,
        deposits: Vec<(u64, B256)>,
        events: Vec<ParentEvent>,
        /// batch → its last block
        batch_ends: BTreeMap<u64, u64>,
        /// Blocks whose state the node no longer has.
        missing_state: Vec<u64>,
        /// Blocks whose proof the node fails to answer for once.
        flaky_state: Vec<u64>,
        /// `eth_getProof` calls made.
        slot_reads: u32,
        /// Batches whose end the node fails to answer for.
        failing_batch_ends: Vec<u64>,
        /// How many parent receipt reads fail before one succeeds.
        failing_receipts: u32,
        /// Real, linked parent-chain headers by number, for the spans.
        parent: BTreeMap<u64, ParentHeader>,
    }

    #[async_trait]
    impl ArbitrumView for Mutex<FakeChain> {
        async fn arbitrum_head(&self) -> Result<u64, RelayerError> {
            Ok(self.lock().unwrap().arbitrum_head)
        }
        async fn deposit_blocks(
            &self,
            from: u64,
            to: u64,
        ) -> Result<Vec<(u64, B256)>, RelayerError> {
            let c = self.lock().unwrap();
            Ok(c.deposits
                .iter()
                .copied()
                .filter(|(n, _)| (from..=to).contains(n))
                .collect())
        }
        async fn parent_head(&self) -> Result<u64, RelayerError> {
            Ok(self.lock().unwrap().parent_head)
        }
        async fn parent_events(
            &self,
            from: u64,
            to: u64,
        ) -> Result<Vec<ParentEvent>, RelayerError> {
            let c = self.lock().unwrap();
            Ok(c.events
                .iter()
                .filter(|e| (from..=to).contains(&e.position().0))
                .cloned()
                .collect())
        }
        async fn batch_end(&self, batch: u64) -> Result<Option<u64>, RelayerError> {
            let c = self.lock().unwrap();
            if c.failing_batch_ends.contains(&batch) {
                return Err(RelayerError::Eth(format!(
                    "batch {batch}: connection reset"
                )));
            }
            Ok(c.batch_ends.get(&batch).copied())
        }
        async fn arbitrum_header(&self, number: u64) -> Result<(B256, Bytes), RelayerError> {
            Ok((h(number), Bytes::from(number.to_be_bytes().to_vec())))
        }
        async fn arbitrum_number(&self, hash: B256) -> Result<Option<u64>, RelayerError> {
            let ends = self.lock().unwrap().batch_ends.clone();
            Ok(ends.values().copied().find(|&m| h(m) == hash))
        }
        async fn history_slots(
            &self,
            ns: &[u64],
            m: u64,
        ) -> Result<Vec<(u64, B256, SlotProof)>, RelayerError> {
            let mut c = self.lock().unwrap();
            c.slot_reads += 1;
            if c.missing_state.contains(&m) {
                return Err(RelayerError::Eth(format!(
                    "historical state of {m} is not available"
                )));
            }
            if let Some(i) = c.flaky_state.iter().position(|&b| b == m) {
                c.flaky_state.remove(i);
                return Err(RelayerError::Eth(format!(
                    "block {m}: 429 Too Many Requests"
                )));
            }
            drop(c);
            Ok(ns
                .iter()
                .map(|&n| {
                    (n, h(n), SlotProof {
                        account_proof: vec![Bytes::from(m.to_be_bytes().to_vec())],
                        storage_proof: vec![Bytes::from(n.to_be_bytes().to_vec())],
                    })
                })
                .collect())
        }
        async fn parent_receipt(
            &self,
            block: u64,
            _tx_index: u32,
        ) -> Result<(B256, Bytes, Vec<Bytes>), RelayerError> {
            let mut c = self.lock().unwrap();
            if c.failing_receipts > 0 {
                c.failing_receipts -= 1;
                return Err(RelayerError::Eth(format!(
                    "receipt in {block}: connection reset"
                )));
            }
            if let Some(header) = c.parent.get(&block) {
                return Ok((header.hash, header.rlp.clone(), vec![Bytes::from(vec![
                    0xbb,
                ])]));
            }
            Ok((h(1_000_000 + block), Bytes::from(vec![0xaa]), vec![
                Bytes::from(vec![0xbb]),
            ]))
        }
        async fn parent_header(&self, number: u64) -> Result<Option<ParentHeader>, RelayerError> {
            Ok(self.lock().unwrap().parent.get(&number).cloned())
        }
    }

    #[derive(Default)]
    struct FakeBridge {
        accepted: Mutex<Vec<(u64, B256)>>,
        sent: Mutex<Vec<ProveBlock>>,
        /// How many proveBlock calls fail before one is sent.
        failing_sends: Mutex<u32>,
        /// The light client's head slot; `None` = no light client.
        head_slot: Mutex<Option<u64>>,
        /// `submitAncestry` payloads sent. A sent span is admitted: the blocks
        /// between its ends become accepted, as the light client does.
        spans: Mutex<Vec<Vec<Bytes>>>,
        /// How many submitAncestry calls fail before one is sent.
        failing_spans: Mutex<u32>,
    }

    #[async_trait]
    impl AnchorTarget for FakeBridge {
        async fn is_accepted(&self, chain_id: u64, block_hash: B256) -> Result<bool, RelayerError> {
            Ok(self
                .accepted
                .lock()
                .unwrap()
                .contains(&(chain_id, block_hash)))
        }
        async fn prove_block(&self, call: &ProveBlock) -> Result<(), RelayerError> {
            let mut failing = self.failing_sends.lock().unwrap();
            if *failing > 0 {
                *failing -= 1;
                return Err(RelayerError::Eth("Acki Nacki is not reachable".into()));
            }
            self.sent.lock().unwrap().push(call.clone());
            Ok(())
        }
        async fn light_client_head_slot(&self) -> Result<Option<u64>, RelayerError> {
            Ok(*self.head_slot.lock().unwrap())
        }
        async fn submit_ancestry(&self, headers: &[Bytes]) -> Result<(), RelayerError> {
            let mut failing = self.failing_spans.lock().unwrap();
            if *failing > 0 {
                *failing -= 1;
                return Err(RelayerError::AckiNacki("exit code 250".into()));
            }
            let mut accepted = self.accepted.lock().unwrap();
            for rlp in &headers[1..headers.len() - 1] {
                accepted.push((ARB.parent_chain_id, keccak256(rlp)));
            }
            self.spans.lock().unwrap().push(headers.to_vec());
            Ok(())
        }
    }

    fn accept(an: &FakeBridge, chain_id: u64, hash: B256) {
        an.accepted.lock().unwrap().push((chain_id, hash));
    }

    /// The fakes answer from one consistent node: no lag behind the head.
    fn cfg() -> TickConfig {
        TickConfig {
            arbitrum_lag: 0,
            ..TickConfig::default()
        }
    }

    async fn run(chain: &Mutex<FakeChain>, an: &FakeBridge, state: &mut AnchorState) -> TickReport {
        tick(ARB, chain, an, state, &cfg()).await.unwrap()
    }

    /// A deposit at block 100; batch 7 ends at 150 (parent block 10); the
    /// assertion naming 150 lands in parent block 20.
    fn one_deposit() -> Mutex<FakeChain> {
        Mutex::new(FakeChain {
            arbitrum_head: 200,
            parent_head: 30,
            deposits: vec![(100, h(100))],
            events: vec![
                ParentEvent::Batch {
                    block: 10,
                    log_index: 0,
                    batch: 7,
                },
                ParentEvent::Assertion {
                    block: 20,
                    log_index: 3,
                    tx_index: 5,
                    end_hash: h(150),
                },
            ],
            batch_ends: BTreeMap::from([(7, 150)]),
            ..FakeChain::default()
        })
    }

    #[tokio::test]
    async fn a_deposit_block_is_proven_once_the_parent_block_is_accepted() {
        let chain = one_deposit();
        let an = FakeBridge::default();
        let mut state = AnchorState::default();

        let r = run(&chain, &an, &mut state).await;
        assert_eq!(
            (r.new_deposit_blocks, r.batches_taken, r.assertions_matched),
            (1, 1, 1)
        );
        assert_eq!(
            r.waiting_for_parent, 1,
            "the parent block is not accepted yet"
        );
        assert!(an.sent.lock().unwrap().is_empty());
        assert!(
            state.batch_ends.is_empty(),
            "the batch end moved into the ready proof"
        );

        accept(&an, ARB.parent_chain_id, h(1_000_020));
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.proofs_sent, 1);
        let sent = an.sent.lock().unwrap()[0].clone();
        assert_eq!(sent, ProveBlock {
            l1_header: Bytes::from(vec![0xaa]),
            tx_index: 5,
            receipt_proof: vec![Bytes::from(vec![0xbb])],
            l2_header: Bytes::from(150u64.to_be_bytes().to_vec()),
            account_proof: vec![Bytes::from(150u64.to_be_bytes().to_vec())],
            block_number: 100,
            storage_proof: vec![Bytes::from(100u64.to_be_bytes().to_vec())],
        });

        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.proofs_sent, 0, "not sent again before resend_after_secs");

        accept(&an, ARB.chain_id, h(100));
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.accepted, 1);
        assert!(state.pending.is_empty() && state.ready.is_empty());
    }

    #[tokio::test]
    async fn a_block_already_accepted_is_not_taken() {
        let chain = one_deposit();
        let an = FakeBridge::default();
        accept(&an, ARB.chain_id, h(100));
        let mut state = AnchorState::default();
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(
            (r.new_deposit_blocks, r.batches_taken, r.assertions_matched),
            (0, 0, 0)
        );
        assert_eq!(r.assertions_missed, 1);
    }

    #[tokio::test]
    async fn a_deposit_in_the_last_block_of_a_batch_waits_for_the_next_one() {
        // M's history holds blocks before M, not M itself.
        let chain = one_deposit();
        chain.lock().unwrap().deposits = vec![(150, h(150))];
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(
            (r.batches_taken, r.assertions_matched, r.assertions_missed),
            (0, 0, 1)
        );
        assert_eq!(state.pending.keys().copied().collect::<Vec<_>>(), vec![150]);
    }

    #[tokio::test]
    async fn a_missed_batch_waits_for_the_next_assertion() {
        // The relayer starts after batch 7 landed: the first assertion finds
        // no proofs, batch 8 and the next assertion carry the deposit.
        let chain = one_deposit();
        let an = FakeBridge::default();
        let mut state = AnchorState {
            parent_cursor: 15,
            ..AnchorState::default()
        };
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(
            (r.batches_taken, r.assertions_matched, r.assertions_missed),
            (0, 0, 1)
        );

        {
            let mut c = chain.lock().unwrap();
            c.parent_head = 60;
            c.batch_ends.insert(8, 180);
            c.events.push(ParentEvent::Batch {
                block: 40,
                log_index: 1,
                batch: 8,
            });
            c.events.push(ParentEvent::Assertion {
                block: 50,
                log_index: 0,
                tx_index: 2,
                end_hash: h(180),
            });
        }
        accept(&an, ARB.parent_chain_id, h(1_000_050));
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(
            (r.batches_taken, r.assertions_matched, r.proofs_sent),
            (1, 1, 1)
        );
        assert_eq!(an.sent.lock().unwrap()[0].block_number, 100);
    }

    #[tokio::test]
    async fn a_batch_the_node_cannot_place_yet_is_taken_on_a_later_tick() {
        // Batch 7 lands before the Arbitrum node knows where it ends.
        let chain = one_deposit();
        {
            let mut c = chain.lock().unwrap();
            c.batch_ends.clear();
            c.parent_head = 15;
        }
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.batches_taken, 0);
        assert_eq!(state.unplaced_batches, BTreeMap::from([(7, BATCH_RETRIES)]));

        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.batches_taken, 0);
        assert_eq!(
            state.unplaced_batches,
            BTreeMap::from([(7, BATCH_RETRIES - 1)])
        );

        // The node places it, and the assertion ending there lands.
        {
            let mut c = chain.lock().unwrap();
            c.batch_ends.insert(7, 150);
            c.parent_head = 30;
        }
        let r = run(&chain, &an, &mut state).await;
        assert_eq!((r.batches_taken, r.assertions_matched), (1, 1));
        assert!(state.unplaced_batches.is_empty());
        assert_eq!(state.ready[0].end.number, 150);
    }

    #[tokio::test]
    async fn a_batch_the_node_cannot_prove_does_not_stop_the_tick() {
        // Batch 6 ends at 120, whose state the node lacks; batch 7, later in
        // the same tick, still carries the deposit to its assertion.
        let chain = one_deposit();
        {
            let mut c = chain.lock().unwrap();
            c.batch_ends.insert(6, 120);
            c.missing_state.push(120);
            c.events.insert(0, ParentEvent::Batch {
                block: 5,
                log_index: 0,
                batch: 6,
            });
        }
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let r = run(&chain, &an, &mut state).await;
        assert_eq!((r.batches_taken, r.assertions_matched), (1, 1));
        assert_eq!(
            state.parent_cursor, 29,
            "the tick went through all its events"
        );
        assert!(
            state.unplaced_batches.is_empty(),
            "a batch whose state is gone is not tried again"
        );
    }

    #[tokio::test]
    async fn a_batch_the_node_failed_to_answer_for_is_tried_on_the_next_tick() {
        // Batch 7 ends at 150; the node fails once on its proof, then answers.
        let chain = one_deposit();
        {
            let mut c = chain.lock().unwrap();
            c.flaky_state.push(150);
            c.parent_head = 15;
        }
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.batches_taken, 0);
        assert_eq!(state.unplaced_batches, BTreeMap::from([(7, BATCH_RETRIES)]));

        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.batches_taken, 1);
        assert!(state.unplaced_batches.is_empty());
        assert!(state.batch_ends.contains_key(&150));
    }

    #[tokio::test]
    async fn an_unplaced_batch_is_given_up_after_its_retries() {
        let chain = one_deposit();
        chain.lock().unwrap().batch_ends.clear();
        let an = FakeBridge::default();
        let mut state = AnchorState {
            parent_cursor: 30,
            unplaced_batches: BTreeMap::from([(7, 1)]),
            ..AnchorState::default()
        };
        state.pending.insert(100, h(100));
        state.deposit_cursor = 200;
        run(&chain, &an, &mut state).await;
        assert!(state.unplaced_batches.is_empty());
    }

    #[tokio::test]
    async fn a_failed_receipt_read_keeps_the_batch_end_for_the_assertion() {
        let chain = one_deposit();
        chain.lock().unwrap().failing_receipts = 1;
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        assert!(tick(ARB, &chain, &an, &mut state, &cfg()).await.is_err());
        assert!(
            state.batch_ends.contains_key(&150),
            "the proofs at 150 are kept while the assertion is not taken"
        );
        assert_eq!(
            state.parent_cursor, 19,
            "the next tick starts at the assertion's block"
        );

        let r = run(&chain, &an, &mut state).await;
        assert_eq!((r.assertions_matched, r.assertions_missed), (1, 0));
        assert_eq!(state.ready[0].end.number, 150);
        assert!(state.batch_ends.is_empty());
    }

    #[tokio::test]
    async fn a_refused_prove_block_keeps_the_proof_and_is_sent_again_later() {
        let chain = one_deposit();
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        run(&chain, &an, &mut state).await;
        accept(&an, ARB.parent_chain_id, h(1_000_020));
        *an.failing_sends.lock().unwrap() = 1;
        let r = run(&chain, &an, &mut state).await;
        assert_eq!((r.proofs_sent, r.proofs_failed), (0, 1));
        assert_eq!(state.ready.len(), 1, "the proof is still there");

        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.proofs_sent, 0, "not sent again before resend_after_secs");

        let again = TickConfig {
            resend_after_secs: 0,
            ..cfg()
        };
        let r = tick(ARB, &chain, &an, &mut state, &again).await.unwrap();
        assert_eq!(r.proofs_sent, 1);
        assert_eq!(an.sent.lock().unwrap()[0].block_number, 100);
    }

    #[tokio::test]
    async fn a_long_range_is_scanned_one_window_per_tick() {
        let chain = one_deposit();
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let window = TickConfig {
            deposit_scan_window: 60,
            ..cfg()
        };
        tick(ARB, &chain, &an, &mut state, &window).await.unwrap();
        assert_eq!(state.deposit_cursor, 60);
        assert!(
            state.pending.is_empty(),
            "the deposit at 100 is in the next window"
        );
        tick(ARB, &chain, &an, &mut state, &window).await.unwrap();
        assert_eq!(state.deposit_cursor, 120);
        assert!(state.pending.contains_key(&100));
    }

    #[tokio::test]
    async fn the_deposit_scan_stays_behind_the_arbitrum_head() {
        let chain = one_deposit();
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let behind = TickConfig {
            arbitrum_lag: 150,
            ..cfg()
        };
        tick(ARB, &chain, &an, &mut state, &behind).await.unwrap();
        assert_eq!(state.deposit_cursor, 50, "head 200 less the lag");
        assert!(
            state.pending.is_empty(),
            "the deposit at 100 is not scanned yet"
        );

        run(&chain, &an, &mut state).await;
        assert!(state.pending.contains_key(&100), "the next batch covers it");
    }

    #[tokio::test]
    async fn a_failed_batch_lookup_keeps_every_unplaced_batch() {
        let chain = one_deposit();
        chain.lock().unwrap().failing_batch_ends.push(6);
        let an = FakeBridge::default();
        let mut state = AnchorState {
            parent_cursor: 30,
            deposit_cursor: 200,
            unplaced_batches: BTreeMap::from([(6, 5), (7, 5)]),
            ..AnchorState::default()
        };
        state.pending.insert(100, h(100));
        assert!(tick(ARB, &chain, &an, &mut state, &cfg()).await.is_err());
        assert_eq!(state.unplaced_batches, BTreeMap::from([(6, 5), (7, 5)]));

        chain.lock().unwrap().failing_batch_ends.clear();
        chain.lock().unwrap().batch_ends.insert(6, 120);
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.batches_taken, 2);
        assert!(state.unplaced_batches.is_empty());
    }

    #[tokio::test]
    async fn without_a_waiting_deposit_a_batch_is_not_looked_up() {
        let chain = one_deposit();
        {
            let mut c = chain.lock().unwrap();
            c.deposits.clear();
            c.batch_ends.clear();
        }
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.batches_taken, 0);
        assert!(state.unplaced_batches.is_empty(), "nothing to retry");
    }

    #[tokio::test]
    async fn batch_ends_an_assertion_passed_are_dropped() {
        let chain = one_deposit();
        {
            let mut c = chain.lock().unwrap();
            c.batch_ends.insert(6, 120);
            c.events.insert(0, ParentEvent::Batch {
                block: 5,
                log_index: 0,
                batch: 6,
            });
        }
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        run(&chain, &an, &mut state).await;
        assert!(
            state.batch_ends.is_empty(),
            "120 is before the assertion's 150"
        );
        assert_eq!(state.ready.len(), 1);
        assert_eq!(state.ready[0].end.number, 150);
    }

    /// A word of ABI-encoded call data.
    fn word(v: u64) -> [u8; 32] {
        U256::from(v).to_be_bytes()
    }

    #[test]
    fn a_batch_ends_where_its_posting_call_says() {
        // Arbitrum Sepolia batch 889611, posted with addSequencerL2BatchFromBlobs
        // (sequenceNumber, afterDelayedMessagesRead, gasRefunder, prevMessageCount,
        // newMessageCount): it holds blocks 322874687..=322875126.
        let mut input = vec![0x3e, 0x5a, 0xa0, 0x82];
        for w in [889_611, 2_230_866, 0, 322_874_688, 322_875_127] {
            input.extend_from_slice(&word(w));
        }
        assert_eq!(
            batch_end_from_calldata(&input, 889_611, 0),
            Some(322_875_126)
        );
        assert_eq!(
            batch_end_from_calldata(&input, 889_612, 0),
            None,
            "another batch"
        );
        assert_eq!(
            batch_end_from_calldata(&input, 889_611, 22_207_817),
            Some(322_875_126 + 22_207_817),
            "Arbitrum One counts from its Nitro genesis"
        );
        // The delay-proof variant appends a tuple; the count stays argument 4.
        let mut delayed = vec![0x91, 0x7c, 0xf8, 0xac];
        for w in [889_613, 2_230_870, 0, 322_875_530, 322_875_713, 7, 13] {
            delayed.extend_from_slice(&word(w));
        }
        assert_eq!(
            batch_end_from_calldata(&delayed, 889_613, 0),
            Some(322_875_712)
        );
        let mut unknown = input.clone();
        unknown[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(batch_end_from_calldata(&unknown, 889_611, 0), None);
        assert_eq!(
            batch_end_from_calldata(&input[..40], 889_611, 0),
            None,
            "truncated"
        );
    }

    #[test]
    fn the_batch_posting_selectors_are_the_inbox_functions() {
        let tuple = "(bytes32,(uint8,address,uint64,uint64,uint256,uint256,bytes32))";
        for (sig, at) in [
            (
                "addSequencerL2BatchFromOrigin(uint256,bytes,uint256,address,uint256,uint256)"
                    .to_string(),
                5,
            ),
            (
                "addSequencerL2Batch(uint256,bytes,uint256,address,uint256,uint256)".to_string(),
                5,
            ),
            (
                format!(
                    "addSequencerL2BatchFromOriginDelayProof(uint256,bytes,uint256,address,\
                     uint256,uint256,{tuple})"
                ),
                5,
            ),
            (
                format!(
                    "addSequencerL2BatchDelayProof(uint256,bytes,uint256,address,uint256,uint256,\
                     {tuple})"
                ),
                5,
            ),
            (
                "addSequencerL2BatchFromBlobs(uint256,uint256,address,uint256,uint256)".to_string(),
                4,
            ),
            (
                format!(
                    "addSequencerL2BatchFromBlobsDelayProof(uint256,uint256,address,uint256,\
                     uint256,{tuple})"
                ),
                4,
            ),
        ] {
            let selector: [u8; 4] = keccak256(sig.as_bytes())[..4].try_into().unwrap();
            assert!(BATCH_POSTING_CALLS.contains(&(selector, at)), "{sig}");
        }
    }

    #[test]
    fn only_a_revert_means_the_block_is_in_no_batch_yet() {
        use alloy::transports::{RpcError, TransportErrorKind};
        use alloy_json_rpc::ErrorPayload;

        let reverted = RpcError::ErrorResp(ErrorPayload {
            code: -32000,
            message: "execution reverted: requested block 5 is after latest on-chain block 4"
                .into(),
            data: None,
        });
        assert!(not_in_a_batch_yet(&reverted));
        let limited = RpcError::ErrorResp(ErrorPayload {
            code: 429,
            message: "Too Many Requests".into(),
            data: None,
        });
        assert!(!not_in_a_batch_yet(&limited));
        assert!(!not_in_a_batch_yet(&TransportErrorKind::custom_str(
            "connection reset"
        )));
    }

    #[test]
    fn the_state_belongs_to_one_bridge() {
        let (a, b) = (Address::repeat_byte(0xaa), Address::repeat_byte(0xbb));
        let mut state = AnchorState::default();
        state.ensure_bridge(a).unwrap();
        assert_eq!(
            state.evm_bridge,
            Some(a),
            "a state without one takes the bridge"
        );
        state.ensure_bridge(a).unwrap();
        let e = state.ensure_bridge(b).unwrap_err().to_string();
        assert!(e.contains("move it away"), "{e}");
    }

    #[tokio::test]
    async fn every_waiting_slot_is_read_in_one_call() {
        let chain = one_deposit();
        chain.lock().unwrap().deposits.push((120, h(120)));
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        run(&chain, &an, &mut state).await;
        assert_eq!(chain.lock().unwrap().slot_reads, 1);
        let held: Vec<u64> = state.ready[0].end.slots.keys().copied().collect();
        assert_eq!(held, vec![100, 120]);
    }

    // ── spans for the assertion's parent-chain block ───────────────────

    /// The epoch the assertion's parent block sits in, and its first slot.
    const EPOCH: u64 = 360_000;
    const E0: u64 = EPOCH * 32;

    /// Linked parent-chain headers numbered from 1 000, one per slot in
    /// `E0 - 40 ..= E0 + 72` except the `missing` ones.
    fn parent_chain(missing: &[u64]) -> BTreeMap<u64, ParentHeader> {
        let mut out = BTreeMap::new();
        let mut parent = B256::ZERO;
        let mut number = 1_000;
        for slot in E0 - 40..=E0 + 72 {
            if missing.contains(&slot) {
                continue;
            }
            let header = alloy::consensus::Header {
                parent_hash: parent,
                number,
                timestamp: ARB.parent_beacon_genesis + slot * 12,
                ..Default::default()
            };
            let rlp = Bytes::from(alloy::rlp::encode(&header));
            let hash = keccak256(&rlp);
            out.insert(number, ParentHeader {
                number,
                hash,
                rlp,
                timestamp: header.timestamp,
            });
            parent = hash;
            number += 1;
        }
        out
    }

    fn at_slot(parent: &BTreeMap<u64, ParentHeader>, slot: u64) -> ParentHeader {
        let ts = ARB.parent_beacon_genesis + slot * 12;
        parent
            .values()
            .find(|h| h.timestamp == ts)
            .unwrap_or_else(|| panic!("no block at slot {slot}"))
            .clone()
    }

    /// `one_deposit`, with the assertion in the parent block of slot
    /// `E0 + 10` and the batch two blocks before it.
    fn deposit_with_parent_chain(missing: &[u64]) -> (Mutex<FakeChain>, ParentHeader) {
        let parent = parent_chain(missing);
        let b = at_slot(&parent, E0 + 10);
        let chain = Mutex::new(FakeChain {
            arbitrum_head: 200,
            parent_head: b.number + 40,
            deposits: vec![(100, h(100))],
            events: vec![
                ParentEvent::Batch {
                    block: b.number - 2,
                    log_index: 0,
                    batch: 7,
                },
                ParentEvent::Assertion {
                    block: b.number,
                    log_index: 3,
                    tx_index: 5,
                    end_hash: h(150),
                },
            ],
            batch_ends: BTreeMap::from([(7, 150)]),
            parent,
            ..FakeChain::default()
        });
        (chain, b)
    }

    /// Spans are tried on every tick, to keep the tests short.
    async fn run_now(
        chain: &Mutex<FakeChain>,
        an: &FakeBridge,
        state: &mut AnchorState,
    ) -> TickReport {
        let cfg = TickConfig {
            resend_after_secs: 0,
            ..cfg()
        };
        tick(ARB, chain, an, state, &cfg).await.unwrap()
    }

    fn hash_of(rlp: &Bytes) -> B256 {
        keccak256(rlp)
    }

    #[tokio::test]
    async fn a_span_covers_the_assertion_block_once_the_light_client_passes_its_epoch() {
        // From Gloas on, a checkpoint is the block of the slot before the
        // epoch boundary: the payload bid's parent.
        let (chain, b) = deposit_with_parent_chain(&[]);
        let parent = chain.lock().unwrap().parent.clone();
        let older = at_slot(&parent, E0 - 1);
        let newer = at_slot(&parent, E0 + 31);
        let an = FakeBridge::default();
        accept(&an, ARB.parent_chain_id, older.hash);
        accept(&an, ARB.parent_chain_id, newer.hash);
        *an.head_slot.lock().unwrap() = Some(E0 + 31);
        let mut state = AnchorState::default();

        let r = run_now(&chain, &an, &mut state).await;
        assert_eq!(r.assertions_matched, 1);
        assert_eq!(r.spans_sent, 0, "the head has not passed the epoch yet");
        assert!(an.spans.lock().unwrap().is_empty());

        *an.head_slot.lock().unwrap() = Some(E0 + 32);
        let r = run_now(&chain, &an, &mut state).await;
        assert_eq!(r.spans_sent, 1);
        let span = an.spans.lock().unwrap()[0].clone();
        assert_eq!(span.len(), 33, "two checkpoints and the 31 blocks between");
        assert_eq!(hash_of(&span[0]), older.hash);
        assert_eq!(hash_of(&span[32]), newer.hash);
        assert!(span.contains(&b.rlp));

        let r = run_now(&chain, &an, &mut state).await;
        assert_eq!(r.proofs_sent, 1, "the bridge accepts the parent block now");
        assert_eq!(r.spans_sent, 0);
    }

    #[tokio::test]
    async fn before_gloas_the_span_runs_between_the_boundary_blocks() {
        let (chain, _) = deposit_with_parent_chain(&[]);
        let parent = chain.lock().unwrap().parent.clone();
        let older = at_slot(&parent, E0);
        let newer = at_slot(&parent, E0 + 32);
        let an = FakeBridge::default();
        accept(&an, ARB.parent_chain_id, older.hash);
        accept(&an, ARB.parent_chain_id, newer.hash);
        *an.head_slot.lock().unwrap() = Some(E0 + 32);
        let mut state = AnchorState::default();

        let r = run_now(&chain, &an, &mut state).await;
        assert_eq!(r.spans_sent, 1);
        let span = an.spans.lock().unwrap()[0].clone();
        assert_eq!(span.len(), 33);
        assert_eq!(hash_of(&span[0]), older.hash);
        assert_eq!(hash_of(&span[32]), newer.hash);
    }

    #[tokio::test]
    async fn a_checkpoint_slot_without_a_block_takes_the_block_before_it() {
        // No block in the slot before the boundary: the bid's parent is the
        // block of the slot before that. A slot inside the epoch is empty too.
        let (chain, _) = deposit_with_parent_chain(&[E0 - 1, E0 + 20]);
        let parent = chain.lock().unwrap().parent.clone();
        let older = at_slot(&parent, E0 - 2);
        let newer = at_slot(&parent, E0 + 31);
        let an = FakeBridge::default();
        accept(&an, ARB.parent_chain_id, older.hash);
        accept(&an, ARB.parent_chain_id, newer.hash);
        *an.head_slot.lock().unwrap() = Some(E0 + 40);
        let mut state = AnchorState::default();

        let r = run_now(&chain, &an, &mut state).await;
        assert_eq!(r.spans_sent, 1);
        let span = an.spans.lock().unwrap()[0].clone();
        assert_eq!(
            span.len(),
            32,
            "slots E0 - 2 ..= E0 + 31, two of them empty"
        );
        assert_eq!(hash_of(&span[0]), older.hash);
        assert_eq!(hash_of(&span[31]), newer.hash);
    }

    #[tokio::test]
    async fn no_span_while_a_checkpoint_is_not_accepted() {
        // The light client skipped the checkpoint that closes the epoch.
        let (chain, _) = deposit_with_parent_chain(&[]);
        let parent = chain.lock().unwrap().parent.clone();
        let an = FakeBridge::default();
        accept(&an, ARB.parent_chain_id, at_slot(&parent, E0 - 1).hash);
        *an.head_slot.lock().unwrap() = Some(E0 + 64);
        let mut state = AnchorState::default();

        let r = run_now(&chain, &an, &mut state).await;
        assert_eq!(
            (r.spans_sent, r.spans_failed, r.waiting_for_parent),
            (0, 0, 1)
        );
        assert_eq!(state.ready[0].spans_sent, 0);
    }

    #[tokio::test]
    async fn no_span_without_a_light_client() {
        let (chain, _) = deposit_with_parent_chain(&[]);
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        let r = run_now(&chain, &an, &mut state).await;
        assert_eq!((r.spans_sent, r.waiting_for_parent), (0, 1));
    }

    #[tokio::test]
    async fn a_refused_span_is_sent_again_at_most_max_span_sends_times() {
        let (chain, _) = deposit_with_parent_chain(&[]);
        let parent = chain.lock().unwrap().parent.clone();
        let an = FakeBridge::default();
        accept(&an, ARB.parent_chain_id, at_slot(&parent, E0 - 1).hash);
        accept(&an, ARB.parent_chain_id, at_slot(&parent, E0 + 31).hash);
        *an.head_slot.lock().unwrap() = Some(E0 + 32);
        *an.failing_spans.lock().unwrap() = u32::MAX;
        let mut state = AnchorState::default();

        let mut failed = 0;
        for _ in 0..MAX_SPAN_SENDS + 2 {
            failed += run_now(&chain, &an, &mut state).await.spans_failed;
        }
        assert_eq!(failed, MAX_SPAN_SENDS as usize);
        assert_eq!(state.ready[0].spans_sent, MAX_SPAN_SENDS);
    }

    #[tokio::test]
    async fn a_span_waits_resend_after_secs_between_tries() {
        let (chain, _) = deposit_with_parent_chain(&[]);
        let parent = chain.lock().unwrap().parent.clone();
        let an = FakeBridge::default();
        accept(&an, ARB.parent_chain_id, at_slot(&parent, E0 - 1).hash);
        accept(&an, ARB.parent_chain_id, at_slot(&parent, E0 + 31).hash);
        *an.head_slot.lock().unwrap() = Some(E0 + 32);
        *an.failing_spans.lock().unwrap() = 1;
        let mut state = AnchorState::default();

        assert_eq!(run(&chain, &an, &mut state).await.spans_failed, 1);
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(
            (r.spans_sent, r.spans_failed),
            (0, 0),
            "not tried again before resend_after_secs"
        );
    }

    #[tokio::test]
    async fn a_held_proof_outlives_its_deposits_window() {
        // Batch 7 ends at 150 and its assertion lands, but the parent block
        // is not accepted yet. A batch past the window then lands.
        let chain = one_deposit();
        let an = FakeBridge::default();
        let mut state = AnchorState::default();
        run(&chain, &an, &mut state).await;
        assert!(state.ready[0].end.slots.contains_key(&100));
        let late = 100 + HISTORY_WINDOW + 1;
        {
            let mut c = chain.lock().unwrap();
            c.batch_ends.insert(8, late);
            c.events.push(ParentEvent::Batch {
                block: 35,
                log_index: 0,
                batch: 8,
            });
            c.parent_head = 40;
        }
        run(&chain, &an, &mut state).await;
        assert!(state.pending.contains_key(&100), "its proof is still held");
        assert!(state.ready[0].end.slots.contains_key(&100));

        accept(&an, ARB.parent_chain_id, h(1_000_020));
        let r = run(&chain, &an, &mut state).await;
        assert_eq!(r.proofs_sent, 1);
    }

    #[test]
    fn the_state_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("arbitrum-anchors.json");
        let state = AnchorState {
            chain_id: 421_614,
            parent_cursor: 9,
            deposit_cursor: 8,
            pending: BTreeMap::from([(100, h(100))]),
            batch_ends: BTreeMap::from([(150, BatchEnd {
                number: 150,
                hash: h(150),
                header: Bytes::from(vec![1, 2]),
                slots: BTreeMap::from([(100, SlotProof::default())]),
            })]),
            ready: vec![],
            unplaced_batches: BTreeMap::from([(7, 3)]),
            batch_txs: BTreeMap::from([(7, h(7))]),
            evm_bridge: Some(Address::repeat_byte(0x22)),
        };
        state.save(&path).unwrap();
        assert_eq!(AnchorState::load(&path).unwrap(), Some(state));
        assert_eq!(
            AnchorState::load(&dir.path().join("none.json")).unwrap(),
            None
        );
    }

    #[test]
    fn the_call_parameters_are_what_the_verifier_abi_takes() {
        let call = ProveBlock {
            l1_header: Bytes::from(vec![0xf9, 0x01]),
            tx_index: 64,
            receipt_proof: vec![Bytes::from(vec![0xab])],
            l2_header: Bytes::from(vec![0xcd]),
            account_proof: vec![],
            block_number: 518_171_193,
            storage_proof: vec![Bytes::from(vec![0xef]), Bytes::from(vec![0x01])],
        };
        let abi =
            fixture("../../contracts/an/0.81.1_compiled/exchange/ArbitrumBlockVerifier.abi.json");
        let inputs: Vec<String> = abi["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "proveBlock")
            .unwrap()["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_string())
            .collect();
        let params = call.abi_params();
        let mut keys: Vec<String> = params.as_object().unwrap().keys().cloned().collect();
        let mut want = inputs.clone();
        keys.sort();
        want.sort();
        assert_eq!(keys, want);
        assert_eq!(params["l1Header"], "f901");
        assert_eq!(params["storageProof"], serde_json::json!(["ef", "01"]));
        assert_eq!(params["blockNumber"], "518171193");
    }
}
