# BK Set Update Tracking — Lightweight (no Circuit 3) plan

Goal: extend the bridge to track BK-set rotations using only Circuit 1a/1b
(attestation) + an **open** (non-ZK) SHA-256 Merkle proof over the 8-leaf
block-id tree to reveal `L2 = old_bk_set_poseidon_hash` and
`L3 = new_bk_set_poseidon_hash`. Verifier daemon today models what the
Ethereum bridge contract will eventually do.

---

## 0. Recap of the block-id tree

8-leaf SHA-256 tree (see `bridge-prover-lib/src/block_id_tree.rs`):

```
                Root = block_id
           /                       \
         H01                        H23
       /      \                   /      \
     H0        H1               H2        H3
    /  \      /  \             /  \      /  \
   L0  L1   L2  L3           L4  L5   L6  L7
```

with leaves:

| Leaf | Meaning                                            |
|------|----------------------------------------------------|
| L0   | Poseidon(layer_hashes_preimage)                    |
| L1   | SHA256(common_section_bytes)                       |
| **L2** | **old_bk_set_poseidon_hash** (32-byte LE Fr)     |
| **L3** | **new_bk_set_poseidon_hash** (32-byte LE Fr)     |
| L4   | tvm_block_repr_hash                                |
| L5   | SHA256(durable_state)                              |
| L6   | SHA256(tx_cnt u64 BE)                              |
| L7   | Poseidon Merkle root of referenced blocks          |

Inner hashes: `H1 = SHA(L2 ‖ L3)`, `H01 = SHA(H0 ‖ H1)`, `root = SHA(H01 ‖ H23)`.

**Minimum sibling set to reveal both L2 and L3** = **2 siblings: `H0` and `H23`**.
Verifier check:

```
H1   = SHA( L2 ‖ L3 )            // L2, L3 are openly provided
H01  = SHA( H0 ‖ H1 )            // H0  is sibling
root = SHA( H01 ‖ H23 )          // H23 is sibling
require root == block_id_from_Circuit1a/1b
```

The node already exposes the 8 leaves over GraphQL (`block_merkle_tree_leaves`),
so the prover does not have to reconstruct anything from raw block bytes — it
just calls `BlockIdMerkleTree::from_leaves` and reads `tree.h0`, `tree.h23`,
plus `tree.leaves[2]` and `tree.leaves[3]`.

---

## 1. What a bk-set-update event looks like on chain

On Acki Nacki the producer carries the BK set as Poseidon commitments inside
every block; an update is detected structurally by `L2 ≠ L3` in that block's
leaves. The block is signed by **L2's** BK set (the OLD/CURRENT one); the very
next block onward is signed by **L3's** BK set (the NEW one). The off-chain
diff stream `bkSetUpdates` already provides the per-update add/remove records
(see `bridge-prover-lib/src/bk_set_fetcher.rs`).

Important properties:

* **Cannot be skipped.** Thinning (W·P) is fine for layer-hash bundles, but
  any block where `L2 ≠ L3` is a state transition on the bridge: missing it
  desynchronizes our stored commitment and we will reject every subsequent
  Circuit 1a/1b proof (signer-set mismatch).
* **May fall anywhere.** A bk-update block can sit on a thinned key-block
  boundary, between boundaries, or adjacent to one. Worst case: several
  bk-update blocks land inside one W·P window.
* **Cluster-level signal.** Multiple sequential bk-update blocks can happen
  in fast succession during validator churn, but each one is a self-contained
  state transition: prove and apply them in seq_no order.

---

## 2. State extension — **split between contract-mirror state and prover-private state**

The verifier daemon is a 1:1 model of the future Ethereum bridge contract.
The contract will never store the full BK set, only the **commitment** — so
`BridgeState` (the contract mirror, shared by both daemons via `state.json`)
must stay commitment-only.

The pubkey table lives in a **separate prover-only file**, because the
prover needs it to assemble Circuit 1a/1b witnesses (signer_index → BLS
pubkey lookup).

### 2.1 `BridgeState` — contract mirror, schema v4

In `bridge-prover-lib/src/bridge_state.rs`:

```rust
pub struct BridgeState {
    // ... existing fields ...
    pub stored_bk_set_commitment: [u8; 32],          // existing
    pub stored_last_seen_block_seq_no: u64,           // existing — gates W·P thinning

    /// schema v4 — seq_no of the last bk-set-update block whose transition
    /// we have applied. Mirrors what the ETH contract will store.
    #[serde(default)]
    pub stored_last_bk_set_update_seq_no: u64,
}
```

`append_bundle` is **untouched** for layer-hash bundles. Bk-set rotation
moves to a separate method that touches **only the commitment**:

```rust
pub fn apply_bk_set_update(
    &mut self,
    old_commitment: [u8; 32],
    new_commitment: [u8; 32],
    update_block_seq_no: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(old_commitment == self.stored_bk_set_commitment,
        "bk-update old commitment does not match stored state");
    self.stored_bk_set_commitment         = new_commitment;
    self.stored_last_bk_set_update_seq_no = update_block_seq_no;
    Ok(())
}
```

This is exactly what the Solidity `applyBkSetUpdate` will do (§6).

### 2.2 `ProverBkSet` — prover-private pubkey table (new)

New file, e.g. `bridge-prover-lib/src/prover_bk_set.rs`, persisted to
`prover_bk_set.json` next to `state.json`. **Never loaded by the verifier
daemon.**

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProverBkSet {
    /// Commitment this pubkey table hashes to — Poseidon-bound. Must agree
    /// with `BridgeState::stored_bk_set_commitment` at every checkpoint.
    pub commitment: [u8; 32],
    /// signer_index -> 48-byte compressed BLS G1 pubkey.
    pub pubkeys: BTreeMap<u16, Vec<u8>>,
    /// seq_no of the last bk-update applied to this table.
    pub last_applied_update_seq_no: u64,
}

impl ProverBkSet {
    pub fn rotate(&mut self, new_commitment: [u8; 32],
                  new_pubkeys: BTreeMap<u16, Vec<u8>>,
                  update_block_seq_no: u64) -> anyhow::Result<()> { ... }

    /// Sanity at every read site:
    pub fn check_matches(&self, contract_state: &BridgeState) -> anyhow::Result<()> {
        anyhow::ensure!(self.commitment == contract_state.stored_bk_set_commitment,
            "prover bk_set out of sync with contract mirror");
        Ok(())
    }
}
```

At daemon startup the prover loads both files and runs `check_matches`.
If they disagree (e.g. someone deleted only one), the prover re-bootstraps
the pubkey table from `bkSetUpdates` and re-verifies the Poseidon hash
matches the stored commitment.

### 2.3 Bootstrap path

Today the prover materialises the BK set from `bkSetUpdates` at startup
and holds it in memory (`bk_set_fetcher::fetch_bk_set`). With schema v4 the
flow becomes:

1. Load `state.json` (contract mirror) → `stored_bk_set_commitment`.
2. Load `prover_bk_set.json` → in-memory pubkey table, sanity-check
   against (1).
3. If `prover_bk_set.json` is missing or stale (different commitment),
   replay `bkSetUpdates` up to `stored_last_bk_set_update_seq_no` and
   write a fresh `prover_bk_set.json`.
4. Bootstrap-only path (first run): seed the pubkey table from
   `bkSetUpdates` (or `bk_set.json` fallback) and write *both* files.

The verifier daemon only ever reads `state.json` — same as today.

---

## 3. Detection on the prover daemon

Two signals are available and we should use both:

### 3.1 `bkSetUpdates` GraphQL stream (cheap polling)

Already wired (`gql.query_bk_set_updates_light`, `_last`). Each edge carries
a `bk_set_update` blob with adds/removes and (importantly) a block ref. We
add a daemon-side cursor `last_processed_bk_update_seq_no` that is initialised
from `BridgeState::stored_last_bk_set_update_seq_no` at startup and walks
edges forward.

* Poll interval: same cadence as the block-tip poll (already 1 s in the
  main loop).
* For every new edge with seq_no > cursor: enqueue a **bk-update task**
  for that seq_no. The blob is *advisory*; the authoritative data is L2/L3
  read from the block's `block_merkle_tree_leaves`.

### 3.2 Inline check against fetched blocks

When the daemon fetches a key block for the layer-hashes bundle, it already
gets `block_merkle_tree_leaves`. If `leaves[2] != leaves[3]`, that block is
itself a bk-update block. We must catch this **before** the layer-hash bundle
is processed, because the bundle's bk-set commitment is the *old* one (L2),
not the new one — emit the bk-update bundle first, then the layer bundle
(both for the same block).

### 3.3 Plug-in point in `bridge-prover-daemon/src/main.rs`

The current loop pseudo-code (around lines 272–308):

```
loop {
    latest = gql.query_latest_blocks()
    next_thinned = find_next_thinned_key_block(state.stored_last_seen, latest, W, P)
    if next_thinned.is_none() { sleep; continue; }
    process_layer_bundle(next_thinned)
}
```

New shape:

```
loop {
    latest = gql.query_latest_blocks()

    // 1. Drain any bk-set updates strictly older than min(next_thinned, latest).
    while let Some(upd_seqno) = next_pending_bk_update(&state, latest) {
        emit_bk_update_bundle(upd_seqno, &prover_bk_set)?;   // §4
        // After verifier ACK (or self-verify), rotate both stores:
        state.apply_bk_set_update(L2, L3, upd_seqno)?;
        state.save(STATE_FILE)?;                              // contract mirror
        prover_bk_set.rotate(L3, new_pubkeys, upd_seqno)?;
        prover_bk_set.save(PROVER_BK_SET_FILE)?;              // prover-private
    }

    // 2. Existing thinning logic, but: skip a layer bundle whose key block
    //    is itself a bk-update block until that update has been applied.
    next_thinned = find_next_thinned_key_block(state.stored_last_seen,
                                               latest, W, P);
    // If next_thinned == an unseen bk-update block, the previous step
    // already emitted the update bundle for it; the layer bundle now
    // legitimately uses the NEW bk_set.
    process_layer_bundle(next_thinned)?;
}
```

Single linear order: bk-update bundles always precede the layer bundle for
the same key block, and chronologically interleave with other layer bundles.
`next_pending_bk_update` walks the `bkSetUpdates` cursor + filters by
`seq_no > state.stored_last_bk_set_update_seq_no` and
`seq_no <= some upper bound` (latest known tip).

---

## 4. The bk-update bundle (what the prover emits)

A bk-update bundle is a new IPC artefact. It's lighter than a layer bundle —
no Circuit 2 — but it carries a Circuit 1a/1b proof + the open SHA-256
sibling pair.

### 4.1 New IPC variant (schema v4)

```rust
// ipc.rs (PROOF_REQUEST_SCHEMA_VERSION = 4)

pub enum BundleKind { Layer, BkUpdate }   // enum-tagged

pub struct BkUpdateRequest {
    pub schema_version: u32,            // 4
    pub kind: BundleKind,               // BkUpdate
    pub block_seq_no: u32,
    pub block_height: u64,
    pub last_seen_block_seqno: u32,     // = stored_last_bk_set_update_seq_no
    pub block_id_hex: String,

    // Attestation (1a or 1b) — same shape as today
    pub attestation_circuit: AttestationCircuit,
    pub primary_proof_hex: String,

    // OPEN bk-set update payload — all the verifier (and the ETH contract)
    // needs. No pubkey list: the contract only stores the commitment.
    pub old_bk_set_poseidon_hash_hex: String,   // L2
    pub new_bk_set_poseidon_hash_hex: String,   // L3
    pub merkle_sibling_h0_hex: String,           // H0
    pub merkle_sibling_h23_hex: String,          // H23
}
```

The new pubkey **list** is **not** carried in the IPC bundle. It is
prover-only working data (§2.2): the prover computes it locally from
`bkSetUpdates`, sanity-checks `Poseidon(new_pubkeys) == L3`, and uses it
to build the *next* attestation proof. The verifier never sees it.

Backwards compatibility: `ProofRequest` (layer bundle, v3) and
`BkUpdateRequest` (v4) live side-by-side. A discriminator file (or a
top-level enum-tagged JSON) tells the verifier which one to read for a
given seq_no. Concretely:

* `proofs/proof_{:06}.json`   — layer bundles (existing).
* `proofs/bkupd_{:06}.json`   — bk-update bundles (new).

The verifier scans both filename patterns; no in-file ambiguity.

### 4.2 Prover work for one bk-update

For target seq_no `S`:

1. Fetch attestation evidence (`attestation_fetcher::fetch_attestation_evidence`)
   — same path as a layer bundle, classified Primary→1a or Fallback→1b.
2. Fetch block via `gql.query_proof_block_by_seqno(S)` to obtain
   `block_merkle_tree_leaves`.
3. Build `tree = BlockIdMerkleTree::from_leaves(leaves)`. Sanity:
   `tree.block_id() == block_id_from_attestation`.
4. Compute the new pubkey table by replaying `bkSetUpdates` up to and
   including `S` against the current `state.bk_set_pubkeys`. Sanity check:
   `poseidon_hash(new_table) == leaves[3]` (== L3). **This is the trust
   anchor for the verifier**: if Poseidon disagrees, abort the bundle.
5. Generate the Circuit 1a/1b proof against the **OLD** BK set
   (`prover_bk_set.pubkeys`), since the bk-update block is signed by L2.
6. Write `bkupd_{S}.json` with `L2`, `L3`, `H0`, `H23`, the attestation
   proof. No pubkey list in the IPC.
7. On verifier ACK (or self-verify pass), rotate the prover's own table:
   `prover_bk_set.rotate(L3, new_pubkeys, S)` and persist
   `prover_bk_set.json`. Also call `state.apply_bk_set_update(L2, L3, S)`
   on the contract-mirror state.

The 5–7 ordering matters: `prover_bk_set` must NOT rotate until the
verifier accepts the update, otherwise on crash-restart the prover would
be unable to redo the proof against L2.

### 4.3 No Poseidon-on-pubkeys in Circuit 1a/1b changes

Circuit 1a/1b already binds `bk_set_commitment` as a public instance. The
bk-update bundle reuses this **unchanged**: the public instance for the
attestation proof is `[block_id, L2, block_seq_no, last_seen]` — i.e. the
attestation proves "this block_id was finalized by the BK set whose
commitment is L2", exactly as today. We never feed L3 into the circuit.
L3 only appears in the open SHA-256 sibling check.

---

## 5. Verifier daemon flow

`bridge-verifier-daemon/src/main.rs` already branches on the attestation
tag. We add a parallel arm for bk-update bundles:

```
for each new file in proofs/:
    if matches "proof_*.json":
        verify_layer_bundle(...)             // existing — v3 path
    if matches "bkupd_*.json":
        verify_bk_update_bundle(...)         // new — v4 path
```

`verify_bk_update_bundle` performs **three** checks — exactly mirroring
what the ETH contract will do (§6). No Poseidon-on-pubkeys check here:
the verifier doesn't have (and shouldn't have) the pubkey list. The
attestation proof + the SHA-256 Merkle binding are sufficient — they
prove "block_id was finalised by signers committed to L2, and that same
block_id carries L3 as its new commitment".

1. **Attestation.** Pick VK by `attestation_circuit` tag; verify the proof
   against public instances `[block_id, L2, block_seq_no, last_seen]`.
   `L2` must equal `state.stored_bk_set_commitment`. This *authorises*
   the update — the old set has signed off on the block that announces the
   new set.
2. **Merkle.** Recompute
   `root = SHA( SHA(H0 ‖ SHA(L2 ‖ L3)) ‖ H23 )` and require
   `root == block_id`. This binds L3 into the same block_id the
   attestation just verified.
3. **Monotonicity.** `block_seq_no > state.stored_last_bk_set_update_seq_no`.

On success: `state.apply_bk_set_update(L2, L3, S)` + persist. The
contract-mirror state now holds the new commitment; future layer-hash
bundles will arrive with `bk_set_commitment = L3` and verify against it
unchanged. The verifier never learns the pubkeys.

---

## 6. Ethereum-contract sketch (no ZK on this leg)

For the eventual on-chain verifier:

```solidity
function applyBkSetUpdate(
    bytes calldata zkAttestationProof,     // 1a or 1b — verified by precompile/Halo2 verifier
    uint8 attestationCircuitTag,           // 0 = primary, 1 = fallback
    bytes32 blockId,
    uint64 blockSeqNo,
    bytes32 oldCommitmentL2,
    bytes32 newCommitmentL3,
    bytes32 siblingH0,
    bytes32 siblingH23
) external {
    require(oldCommitmentL2 == storedBkSetCommitment, "stale old");
    require(blockSeqNo > storedLastBkUpdateSeq, "replay");

    // (1) ZK attestation, public instances [blockId, L2, seq, lastSeen]
    require(zkVerify(zkAttestationProof, attestationCircuitTag,
                     blockId, oldCommitmentL2, blockSeqNo, storedLastSeen),
            "attestation");

    // (2) open merkle check — pure sha256, cheap on ETH
    bytes32 h1   = sha256(abi.encodePacked(oldCommitmentL2, newCommitmentL3));
    bytes32 h01  = sha256(abi.encodePacked(siblingH0, h1));
    bytes32 root = sha256(abi.encodePacked(h01, siblingH23));
    require(root == blockId, "merkle");

    storedBkSetCommitment = newCommitmentL3;
    storedLastBkUpdateSeq = blockSeqNo;
    emit BkSetUpdated(oldCommitmentL2, newCommitmentL3, blockSeqNo);
}
```

Note that the new pubkey **list** is NOT submitted to Ethereum — the contract
only stores the **commitment**. The off-chain prover keeps the list to build
future Circuit 1a/1b proofs; the contract only ever needs the commitment to
verify those proofs.

---

## 7. Phasing

### Phase 1 — local devnet (no rotation expected)

* Implement schema v4, both daemons, IPC discriminator. On local devnet
  `L2 == L3` for every block; the bk-update queue stays empty, no
  `bkupd_*.json` files are ever produced. Verifier passes unchanged.
* **Acceptance:** existing E2E test in `TECHNICAL_README.md` must still
  show every bundle `verified=true` and zero bk-update bundles. State
  file gains the two new fields (both with their defaults).

### Phase 2 — shellnet cadence study (research, before code lands)

Run a one-off probe against shellnet:

```bash
# Walk last N bk-set updates and report their seq_no spacing.
cargo run -p bridge-prover-daemon --bin probe_bk_updates -- \
    --gql https://shellnet.ackinacki.org/graphql --last 2000
```

(Probe binary to be added; it just calls
`gql.query_bk_set_updates_last(N)` and prints `seq_no` deltas.)

What we want to learn:

* mean / p50 / p99 inter-update spacing in blocks,
* whether updates ever co-locate inside a single W·P window (W=128, P=8 → 1024 blocks),
* whether bk-update blocks tend to land near key-block boundaries.

This informs whether the daemon needs **batching** (multiple bk-update
bundles back-to-back) or if updates are sparse enough that one-at-a-time
is fine. My prior: shellnet rotations are on the order of hours/days
during normal operation, so one bk-update per W·P window is the typical
case and "one at a time" suffices. The architecture in §3.3 handles
arbitrary stacking regardless.

### Phase 3 — shellnet end-to-end

* Bring up shellnet prover+verifier with the v4 stack.
* Watch for the first natural rotation. Verify:
  - `bkupd_{S}.json` appears,
  - verifier accepts it,
  - the NEXT layer bundle uses the rotated `stored_bk_set_commitment`,
  - Circuit 1a/1b still passes (signers map to NEW bk_set).
* If shellnet does not rotate within a reasonable window, force a rotation
  on a side cluster to exercise the path before relying on prod traffic.

---

## 8. Interleaving with thinning — concrete cases

| Case | Bk-update at seq U | Next thinned at seq T | Order daemon emits |
|------|----|----|----|
| A. Update strictly between bundles | U ∈ (last_T, T) | T | bkupd(U) → layer(T) |
| B. Update == key block | U == T | T | bkupd(U) → layer(T) (same block) |
| C. Adjacent updates | U₁, U₂ ∈ (last_T, T] | T | bkupd(U₁) → bkupd(U₂) → layer(T) |
| D. Update after current tip | U > latest | — | wait (no-op) |

Case B is the subtle one: the layer bundle for T must use the **new** BK
set (L3) because, per §1, blocks **after** the update are signed by L3.
But T itself was signed by L2. The classifier needs both:

* attestation for T against L2 (proves block_id) → goes into bkupd bundle,
* layer-hashes proof for T → goes into the layer bundle (Circuit 2 proves
  the layer movement; its `bk_set_commitment` public instance must equal
  the **L2 of T** because that's what Circuit 1a/1b for T bound).

So in case B both bundles use L2. The state's bk-set commitment rotates
to L3 *after* both bundles for T are accepted. The NEXT layer bundle (at
T + W·P) will use L3. This is bookkeeping inside the daemon, no extra
circuit work.

---

## 9. Implementation checklist (when we go to code)

1. `bridge-prover-lib/src/bridge_state.rs`: add `stored_last_bk_set_update_seq_no`
   (commitment-only — NO pubkeys), schema bump to 4, add `apply_bk_set_update(L2, L3, S)`.
2. `bridge-prover-lib/src/prover_bk_set.rs` (new): `ProverBkSet` struct +
   `prover_bk_set.json` persistence. Prover-only; verifier never reads it.
3. `bridge-prover-lib/src/ipc.rs`: schema constant → 4, add `BkUpdateRequest`
   (no pubkey list), new file paths (`bkupd_*.json`).
4. `bridge-prover-lib/src/bk_set_fetcher.rs`: extract a cursor-walk variant
   that takes a starting seq_no and returns "the next bk-update event > cursor".
5. `bridge-prover-daemon/src/main.rs`:
   * load both `state.json` and `prover_bk_set.json` on startup, sanity-check
     `ProverBkSet::check_matches(&BridgeState)`,
   * before each thinning step, drain pending bk-update events,
   * for each pending event: generate Circuit 1a/1b against OLD `prover_bk_set.pubkeys`,
     write `bkupd_*.json` (commitment-only payload),
   * on ACK / self-verify pass: rotate both `state` and `prover_bk_set`,
     persist both.
6. `bridge-verifier-daemon/src/main.rs`: add `verify_bk_update_bundle`
   (3-step: attestation + Merkle + monotonicity) + `state.apply_bk_set_update`
   call. Verifier touches `state.json` only.
7. Probe binary for shellnet cadence study (§7 phase 2).
8. Docs: this file + a section in `TECHNICAL_README.md`.

Risk hot spots:
* Replaying `bkSetUpdates` and matching `poseidon_hash(pubkeys) == L3`
  byte-exactly with the node's encoding. Add a parity test that pulls a
  real shellnet bk-update block and verifies our Poseidon == leaves[3].
* GraphQL race at startup (we have a memory note about this) — fall back
  to a `bk_set.json` config file if `bkSetUpdates` returns null, same as
  today. The state file, once initialised, becomes authoritative.

---

## 10. What we explicitly do NOT do here

* **No Circuit 3.** We are deliberately punting the ZK proof of bk-set
  *transitions*. The open SHA-256 sibling proof is sufficient because the
  Ethereum contract can verify SHA-256 cheaply, and the attestation circuit
  already proves the block_id is canonical under the old BK set. The cost
  of openness: leaves L2 and L3 are revealed publicly, which is fine
  because the BK-set commitment is already a public quantity on chain.
* **No change to Circuit 1a/1b or Circuit 2.** The bk-set commitment they
  bind remains the OLD one (L2) for the bk-update block. Future blocks
  use the new commitment because that's what the chain itself produces.
* **No change to thinning math.** W·P stays. Bk-update bundles are
  *additional*, not a replacement.
