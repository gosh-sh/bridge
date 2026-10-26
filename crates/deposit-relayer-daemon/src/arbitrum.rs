//! Arbitrum block anchors (`deposit-relayer arbitrum-anchors`): gets the
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
//! first (the parent chain's light client, with `submitAncestry` for blocks
//! between its checkpoints); until it is, the proof waits in the state file.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use acki_nacki_interface::{
    ContractCallRequest, ExtendedAddress, IAckiNacki, TvmAckiNacki, TvmClientConfig,
};
use alloy::{
    consensus::ReceiptEnvelope,
    eips::Encodable2718,
    primitives::{address, b256, keccak256, Address, Bytes, B256, U256},
    providers::Provider,
    rpc::types::{Filter, Header as RpcHeader, TransactionReceipt, TransactionRequest},
    sol,
    sol_types::SolCall,
    trie::{proof::ProofRetainer, HashBuilder, Nibbles},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use tvm_client::crypto::KeyPair;

use crate::{error::RelayerError, source::AckiNackiBridge};

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
}

pub const ARBITRUM_CHAINS: &[ArbitrumChain] = &[
    ArbitrumChain {
        name: "Arbitrum Sepolia",
        chain_id: deposit_chain_ids::CHAIN_ID_ARBITRUM_SEPOLIA,
        parent_chain_id: deposit_chain_ids::CHAIN_ID_SEPOLIA,
        rollup: address!("042b2e6c5e99d4c521bd49beed5e99651d9b0cf4"),
        sequencer_inbox: address!("6c97864CE4bEf387dE0b3310A44230f7E3F1be0D"),
        nitro_genesis_block: 0,
    },
    ArbitrumChain {
        name: "Arbitrum One",
        chain_id: deposit_chain_ids::CHAIN_ID_ARBITRUM_ONE,
        parent_chain_id: 1,
        rollup: address!("4dceb440657f21083db8add07665f8ddbe1dcfc0"),
        sequencer_inbox: address!("1c479675ad559DC151F6Ec7ed3FbF8ceE79582B6"),
        nitro_genesis_block: 22_207_817,
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

/// The RLP of a block header, checked against the block's hash.
pub fn header_rlp(header: &RpcHeader) -> Result<Bytes, RelayerError> {
    let rlp = alloy::rlp::encode(&header.inner);
    if keccak256(&rlp) != header.hash {
        return Err(RelayerError::Eth(format!(
            "the header of block {} does not encode to its hash {}: the node returns fields this \
             encoder does not know",
            header.inner.number, header.hash
        )));
    }
    Ok(rlp.into())
}

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
}

/// What `arbitrum-anchors` keeps between runs. The slot proofs cannot be
/// fetched again once their state has aged out of the nodes, so they live
/// here until the assertion that uses them lands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorState {
    pub chain_id: u64,
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
}

/// How many ticks a batch the Arbitrum node cannot place yet is tried again:
/// about 20 minutes at the default poll, within the ~30 minutes a Nitro node
/// keeps the state its slot proofs are read from.
pub const BATCH_RETRIES: u32 = 100;

impl AnchorState {
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
    /// The history slot of block `n` in block `m`'s state: its value and proof.
    async fn history_slot(&self, n: u64, m: u64) -> Result<(B256, SlotProof), RelayerError>;
    /// Hash, header RLP and receipt proof of transaction `tx_index` of a
    /// parent-chain block.
    async fn parent_receipt(
        &self,
        block: u64,
        tx_index: u32,
    ) -> Result<(B256, Bytes, Vec<Bytes>), RelayerError>;
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

/// The bridge and the chain's verifier on Acki Nacki.
#[async_trait]
pub trait AnchorTarget: Send + Sync {
    async fn is_accepted(&self, chain_id: u64, block_hash: B256) -> Result<bool, RelayerError>;
    async fn prove_block(&self, call: &ProveBlock) -> Result<(), RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// One round
// ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TickConfig {
    /// Parent-chain blocks to stay behind its head: a load-balanced RPC can
    /// answer `eth_blockNumber` from a node ahead of the one serving logs.
    pub parent_lag: u64,
    /// Seconds before `proveBlock` is sent again for a block the bridge has
    /// not accepted after the first send.
    pub resend_after_secs: u64,
}

impl Default for TickConfig {
    fn default() -> Self {
        Self {
            parent_lag: 1,
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
    pub accepted: usize,
    pub waiting_for_parent: usize,
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

    // Deposits. The head is read before the parent chain, so every block a
    // batch seen below ends at is already scanned.
    let head = view.arbitrum_head().await?;
    if head > state.deposit_cursor {
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
    // events: an assertion can only end at a batch already taken.
    for (batch, left) in std::mem::take(&mut state.unplaced_batches) {
        match take_batch(view, state, batch).await? {
            Taken::Proofs => report.batches_taken += 1,
            Taken::Nothing => {},
            Taken::Unplaced if left > 1 => {
                state.unplaced_batches.insert(batch, left - 1);
            },
            Taken::Unplaced => warn!(
                batch,
                "the Arbitrum node still cannot place this batch; given up, the next batch covers \
                 the same deposits"
            ),
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
            match event {
                ParentEvent::Batch {
                    batch, ..
                } => match take_batch(view, state, batch).await? {
                    Taken::Proofs => report.batches_taken += 1,
                    Taken::Nothing => {},
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
                    // Assertions end further on each time: nothing taken at or
                    // before this one can be named again.
                    if let Some(m) = end_number {
                        state.batch_ends.retain(|&n, _| n > m);
                    }
                    match found {
                        Some(end) => {
                            let (parent_hash, parent_header, receipt_proof) =
                                view.parent_receipt(block, tx_index).await?;
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
                            });
                            report.assertions_matched += 1;
                        },
                        None => {
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

    // Proofs.
    let now = unix_now();
    let mut ready = std::mem::take(&mut state.ready);
    for proof in &mut ready {
        if !an
            .is_accepted(chain.parent_chain_id, proof.parent_hash)
            .await?
        {
            report.waiting_for_parent += 1;
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
            an.prove_block(&call).await?;
            info!(block = n, m = proof.end.number, "proveBlock sent");
            proof.sent.insert(n, now);
            report.proofs_sent += 1;
        }
    }
    ready.retain(|p| !p.end.slots.is_empty());
    state.ready = ready;
    Ok(report)
}

/// What [`take_batch`] did with a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Taken {
    /// Slot proofs were taken at its last block.
    Proofs,
    /// No waiting deposit block is covered by it.
    Nothing,
    /// The Arbitrum node cannot place its last block yet, or cannot prove
    /// it: a load-balanced endpoint answers from a backend without that state.
    Unplaced,
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
    // M's state holds the hashes of blocks M - HISTORY_WINDOW ..= M - 1.
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
    let mut slots = BTreeMap::new();
    for (n, want) in wanted {
        let (value, proof) = match view.history_slot(n, m).await {
            Ok(v) => v,
            Err(e) => return Ok(unprovable(e)),
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
            out.extend(
                provider
                    .get_logs(&chunk)
                    .await
                    .map_err(eth("eth_getLogs"))?,
            );
            start = end + 1;
        }
        Ok(out)
    }

    async fn batch_of(&self, number: u64) -> Option<u64> {
        let call = findBatchContainingBlockCall {
            blockNum: number,
        };
        let tx = TransactionRequest::default()
            .to(NODE_INTERFACE)
            .input(call.abi_encode().into());
        let out = self.arbitrum.call(tx).await.ok()?;
        findBatchContainingBlockCall::abi_decode_returns(&out).ok()
    }
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
        let filter = Filter::new().address(self.evm_bridge).event_signature(
            <AckiNackiBridge::Deposit as alloy::sol_types::SolEvent>::SIGNATURE_HASH,
        );
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
                while txs.len() > 4 * BATCH_RETRIES as usize {
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
        if self.batch_of(lo).await.is_none_or(|b| b > batch) {
            return Ok(None);
        }
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            match self.batch_of(mid).await {
                Some(b) if b <= batch => lo = mid,
                _ => hi = mid - 1,
            }
        }
        Ok((self.batch_of(lo).await == Some(batch)).then_some(lo))
    }

    async fn arbitrum_header(&self, number: u64) -> Result<(B256, Bytes), RelayerError> {
        let block = self
            .arbitrum
            .get_block_by_number(number.into())
            .await
            .map_err(eth("Arbitrum eth_getBlockByNumber"))?
            .ok_or_else(|| RelayerError::Eth(format!("Arbitrum block {number} not found")))?;
        Ok((block.header.hash, header_rlp(&block.header)?))
    }

    async fn arbitrum_number(&self, hash: B256) -> Result<Option<u64>, RelayerError> {
        let block = self
            .arbitrum
            .get_block_by_hash(hash)
            .await
            .map_err(eth("Arbitrum eth_getBlockByHash"))?;
        Ok(block.map(|b| b.header.inner.number))
    }

    async fn history_slot(&self, n: u64, m: u64) -> Result<(B256, SlotProof), RelayerError> {
        let slot = B256::from(U256::from(n % HISTORY_WINDOW));
        let proof = self
            .arbitrum
            .get_proof(HISTORY_ADDRESS, vec![slot])
            .block_id(m.into())
            .await
            .map_err(eth("Arbitrum eth_getProof"))?;
        let storage =
            proof.storage_proof.into_iter().next().ok_or_else(|| {
                RelayerError::Eth("eth_getProof returned no storage proof".into())
            })?;
        Ok((B256::from(storage.value), SlotProof {
            account_proof: proof.account_proof,
            storage_proof: storage.proof,
        }))
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
        Ok((header.hash, header_rlp(&header)?, proof))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Live Acki Nacki target
// ─────────────────────────────────────────────────────────────────────

/// The bridge's getters and the chain's verifier over tvm_client.
pub struct TvmAnchorTarget {
    bridge: TvmAckiNacki,
    verifier: TvmAckiNacki,
    bridge_address: ExtendedAddress,
    verifier_address: ExtendedAddress,
}

/// A uint the TVM ABI decoder rendered, decimal or `0x` hex.
fn abi_uint(v: &serde_json::Value) -> Option<u128> {
    let s = v.as_str()?;
    match s.strip_prefix("0x") {
        Some(hex) => u128::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

impl TvmAnchorTarget {
    /// Connects, and finds the verifier the bridge recorded for `chain`
    /// (`getL2Verifier`). Refuses a chain without one, or one recorded for
    /// another parent chain.
    pub async fn connect(
        graphql_url: &str,
        keys: KeyPair,
        bridge_abi_path: &str,
        verifier_abi_path: &str,
        bridge_address: ExtendedAddress,
        chain: &ArbitrumChain,
    ) -> Result<Self, RelayerError> {
        let client = |abi: &str| -> Result<TvmAckiNacki, RelayerError> {
            Ok(TvmAckiNacki::connect(TvmClientConfig {
                graphql_endpoints: vec![graphql_url.to_string()],
                keys: keys.clone(),
                bridge_abi: TvmAckiNacki::load_abi(abi)?,
            })?)
        };
        let bridge = client(bridge_abi_path)?;
        let verifier = client(verifier_abi_path)?;
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
        info!(verifier = %verifier_address, "ArbitrumBlockVerifier of {}", chain.name);
        Ok(Self {
            bridge,
            verifier,
            bridge_address,
            verifier_address,
        })
    }
}

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
}

// ─────────────────────────────────────────────────────────────────────
// Test vectors (`deposit-relayer arbitrum-vector`)
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
    async fn history_slot(&self, n: u64, m: u64) -> Result<(B256, SlotProof), RelayerError> {
        self.inner.history_slot(n, m).await
    }
    async fn parent_receipt(
        &self,
        block: u64,
        tx_index: u32,
    ) -> Result<(B256, Bytes, Vec<Bytes>), RelayerError> {
        self.inner.parent_receipt(block, tx_index).await
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
    fn a_sepolia_header_encodes_to_the_bench_vector() {
        let f = fixture("tests/fixtures/arbitrum/sepolia_11991511.json");
        let header: RpcHeader = serde_json::from_value(f["block"].clone()).unwrap();
        let rlp = header_rlp(&header).unwrap();
        assert_eq!(
            hex::encode(rlp),
            hex_of(&live_steps()["assertionBlock"]["args"]["l1Header"])
        );
    }

    #[test]
    fn an_arbitrum_header_encodes_to_the_bench_vector() {
        let f = fixture("tests/fixtures/arbitrum/arbitrum_one_518174193.json");
        let header: RpcHeader = serde_json::from_value(f["block"].clone()).unwrap();
        let rlp = header_rlp(&header).unwrap();
        assert_eq!(
            hex::encode(rlp),
            hex_of(&live_steps()["ancestorHash"]["args"]["l2Header"])
        );
    }

    #[test]
    fn a_header_that_does_not_hash_to_its_block_is_refused() {
        let f = fixture("tests/fixtures/arbitrum/sepolia_11991511.json");
        let mut header: RpcHeader = serde_json::from_value(f["block"].clone()).unwrap();
        header.inner.gas_used += 1;
        assert!(header_rlp(&header).is_err());
    }

    #[test]
    fn the_receipt_proof_matches_the_bench_vector_and_the_header() {
        let f = fixture("tests/fixtures/arbitrum/sepolia_11991511.json");
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
        let f = fixture("tests/fixtures/arbitrum/sepolia_11991511.json");
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
        /// Blocks whose state the node cannot prove.
        missing_state: Vec<u64>,
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
            Ok(self.lock().unwrap().batch_ends.get(&batch).copied())
        }
        async fn arbitrum_header(&self, number: u64) -> Result<(B256, Bytes), RelayerError> {
            Ok((h(number), Bytes::from(number.to_be_bytes().to_vec())))
        }
        async fn arbitrum_number(&self, hash: B256) -> Result<Option<u64>, RelayerError> {
            let ends = self.lock().unwrap().batch_ends.clone();
            Ok(ends.values().copied().find(|&m| h(m) == hash))
        }
        async fn history_slot(&self, n: u64, m: u64) -> Result<(B256, SlotProof), RelayerError> {
            if self.lock().unwrap().missing_state.contains(&m) {
                return Err(RelayerError::Eth(format!(
                    "historical state of {m} is not available"
                )));
            }
            let proof = SlotProof {
                account_proof: vec![Bytes::from(m.to_be_bytes().to_vec())],
                storage_proof: vec![Bytes::from(n.to_be_bytes().to_vec())],
            };
            Ok((h(n), proof))
        }
        async fn parent_receipt(
            &self,
            block: u64,
            _tx_index: u32,
        ) -> Result<(B256, Bytes, Vec<Bytes>), RelayerError> {
            Ok((h(1_000_000 + block), Bytes::from(vec![0xaa]), vec![
                Bytes::from(vec![0xbb]),
            ]))
        }
    }

    #[derive(Default)]
    struct FakeBridge {
        accepted: Mutex<Vec<(u64, B256)>>,
        sent: Mutex<Vec<ProveBlock>>,
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
            self.sent.lock().unwrap().push(call.clone());
            Ok(())
        }
    }

    fn accept(an: &FakeBridge, chain_id: u64, hash: B256) {
        an.accepted.lock().unwrap().push((chain_id, hash));
    }

    async fn run(chain: &Mutex<FakeChain>, an: &FakeBridge, state: &mut AnchorState) -> TickReport {
        tick(ARB, chain, an, state, &TickConfig::default())
            .await
            .unwrap()
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
            missing_state: vec![],
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
        assert_eq!(state.unplaced_batches, BTreeMap::from([(6, BATCH_RETRIES)]));
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
            fixture("../../contracts/an/0.81.0_compiled/exchange/ArbitrumBlockVerifier.abi.json");
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
