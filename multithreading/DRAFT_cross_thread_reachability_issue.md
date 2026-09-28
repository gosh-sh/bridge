# Draft — cross-thread reachability guarantees for bridge event proofs

Status: **draft**, prepared by the bridge team for discussion with the acki-nacki
node team. Not landed anywhere yet. All acki-nacki citations are file:line
references into the current `acki-nacki` repo state at the time of writing.

**Update note (2026-09-28).** An earlier revision of this document committed
to walk Direction (a) (X → Y, walk starts at the event) as the pragmatic
near-term choice, on the basis that Direction (a) has a predictable
witness-build latency. That commitment has been **withdrawn**. Direction (a)
is cryptographically unsound in this design: the event block X is not
independently anchored on-chain (see §3.1.1 for the concrete forgery), and
starting the walk from X therefore proves nothing about X's authenticity.
This draft now commits to Direction (b) — walk starts at the anchor Y in
thread 0, which *is* on-chain — as the only sound direction. Direction (a) is
kept below only as a description of what was rejected and why.

## 1. What the bridge is trying to do

The bridge circuit spec is
[`bridge/crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md`](../crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md).

- A user emits a `WithdrawalInitiated` event on Acki Nacki. The event lives in
  some block **X** in thread **t**. Under sharding, `t` can be any thread —
  including a thread ≠ 0.
- The Ethereum side (`AckiNackiBridge.sol`) can only trust a thread-0 layer-N
  batch root — see spec §1.2 (`_isKnownLayerAnchor` reads
  `layerWindows[anchorLayer]`, populated only from thread-0 blocks). Call the
  block that supplies that root **Y**.
- To pay a withdrawal on Ethereum, the circuit has to cryptographically bind
  event → X → Y → on-chain root. When `t = 0` this collapses to X = Y and no
  cross-thread work is needed. When `t ≠ 0` the circuit needs a walk between X
  and Y through cross-thread `refs` edges.

The current spec calls this walk "the L7 walk" (spec §4). It uses acki-nacki's
per-block `refs` list, opened through the block's L7 Poseidon dense-Merkle root.

## 2. What the current spec draft assumes

The spec §4.4 draws the walk as

```
Y  =  B_0  →  B_1  →  ...  →  B_L  =  X
       ^                                ^
       thread 0 (anchor, newer)         thread t (event, older)
```

with `hop_start_block_id = Y.block_id` (the walk begins at the on-chain-known
anchor) and `hop_end_block_id = X.block_id` (the walk terminates at the event
block). Arrow `→` denotes a single hop, i.e. the older block on the right lives
in the left block's `refs`. Each hop opens `B_i.L7` and extracts a slot whose
leaf hashes to `B_{i+1}.block_id`. Slot 0 (`parent_block_id`) is excluded (spec
§2.3, §4.1), so only cross-thread refs are hopped.

The production ceiling declared in the spec is `L_MAX = 300` (spec §4.4, §5.2),
attributed to "the node-team-stated cross-thread walk-length ceiling under the
current threading design".

Two things need to be true before this scheme works end-to-end:

- **(A) the walk direction is sound**: an anchored Y must attest to X's
  existence. This is a cryptographic-binding question and depends on which
  block gets started from.
- **(B) the walk terminates**: for every eligible event, there must exist a
  chain of length ≤ L_MAX from one endpoint to the other over real
  cross-thread `refs` edges.

We think the current spec silently assumed both. Since the pivot to
Direction (b) is the sound one (§3.1.1), (A) is settled — the question that
remains is (B) under (b), and we are asking the node team whether the
acki-nacki protocol actually guarantees it. From reading the code we cannot
find a mechanism that does.

## 3. The two possible walk directions

Regardless of the spec's arrow, only two directions are semantically possible
given that acki-nacki refs point **older, cross-thread**
(`node/src/multithreading/thread_synchrinization_service.rs:67-77` —
`list_blocks_sending_messages_to_thread` returns *last finalized* block of every
*other* thread; `node/src/block/producer/process.rs:530-540` — a candidate
enters the ref list only when its seq_no strictly exceeds the previously
referenced tip of that thread).

### 3.1 Direction (a) — X → Y  *(REJECTED; kept for the record)*

Walk starts at X. Each hop opens X.refs (then B_1.refs, etc.), stepping
backward across threads until landing on a thread-0 block Y that is *older*
than X. Superficially the endpoints are cryptographically linked: X.block_id
commits to X.L7 which commits to X.refs, and every subsequent hop's block_id
also commits to its own L7 which commits to its own refs — so the walk
X → Y1 → Y2 → … → Y is a chain of Poseidon/SHA openings rooted in X.block_id.

The failure mode is that **X.block_id itself is not anchored on-chain**. It
is a public input the circuit is asked to trust, but nothing outside the
circuit ties it to an actual signed acki-nacki block. §3.1.1 spells out the
forgery this enables.

#### 3.1.1 Why Direction (a) is cryptographically unsound

The bridge Ethereum side maintains `layerWindows[anchorLayer]` — the rolling
window of **thread-0** layer-N batch roots (spec §1.2). This is the only
on-chain trust root. Anything that appears in the circuit as a "block" is
trustworthy on-chain **only if it can be reduced, via cryptographic openings,
to a value inside `layerWindows[]`**.

Under Direction (a) the reduction chain runs

```
event → X.L8 → X.block_id → X.L7 → Y1.block_id → … → Y.block_id → block_leaf(Y) → #L1(M_Y) → … → finalRoot ∈ layerWindows[]
```

`X.block_id` is a public input of `BridgeEventFinalProof` (§6.3 `PUB_X_BLOCK_ID`);
`Y.block_id` is a public input too (§6.3 `PUB_Y_BLOCK_ID`), and it is
transitively reduced to `finalRoot`. The reduction chain **does not reduce
X.block_id anywhere**. `X.block_id` enters as a witness the circuit hashes
open, but no step in the chain proves that hashing found any real block.

**The concrete forgery.** An attacker who wants to mint an arbitrary
withdrawal on Ethereum proceeds:

1. **Pick any anchored, thread-0 block Y.** Any block with a `#L<N>(M) ∈
   layerWindows[N]` on Ethereum works. Y is a real block; the attacker only
   observes it.

2. **Compute Y's tagged L7 leaf.** Following spec §2.3,
   `Y_leaf = Poseidon(REFERENCED_REF_BLOCK_TAG ‖ Y.block_id)` — a public
   function of Y's public `block_id`.

3. **Synthesise a fake X.** The attacker constructs a *synthetic* 16-leaf
   block-id tree for a fake block X that no producer ever signed:

   - **X.L7 (position 7 of the block-id tree).** Compute a Poseidon
     dense-Merkle root with two real leaves: `[parent_placeholder_leaf,
     Y_leaf]` (padded to power-of-two width). `parent_placeholder_leaf` is
     any Poseidon-tagged hash the attacker likes — the bridge circuit does
     not open slot 0 (spec §2.3, §4.1). The result is `fake_X.L7`.

   - **X.L8 (position 8 of the block-id tree).** Compute a Poseidon
     dense-Merkle root over a tree whose single real leaf is
     `Poseidon96(chosen_dapp ‖ chosen_acc ‖ chosen_event_hash)`. The
     attacker fabricates a `WithdrawalInitiated` event body (any recipient,
     any amount, any token, up to `type(uint256).max`), computes its BOC
     `repr_hash`, and drops it in position 0. The result is `fake_X.L8`.

   - **X.L0..L6 and L9..L15.** L0..L6 are opaque witnesses to the bridge
     circuit — the circuit reads them only as the aggregate `h0..7` sibling
     value (spec §2.1). The attacker picks any bytes for `h0..7` (equivalently:
     picks any 4 SHA-256 preimages for the left half of the tree). L9..L15
     are the fixed `[0u8; 32]` protocol constants.

   - **fake_X.block_id.** Compose the 16 leaves into the depth-4 SHA-256
     tree and fold. The output is a fresh 32-byte value that no honest node
     ever produced.

4. **Build a Direction-(a) witness.** Feed the prover:
   - X-side: the fabricated event BOC preimages, `x_block_id = fake_X.block_id`,
     `X.L8 = fake_X.L8`, the depth-4 opening's `h0..7` = the picked value.
     The `BridgeEventFinalProof` circuit's X-side constraints (spec §6.7,
     constraints 1–6) are content-oblivious: they only check *hash consistency*
     between BOC preimages and event fields, and between L8 → block_id via
     four SHA-256 compressions. All of that checks out on the attacker's
     synthetic bytes.
   - Hop: one `BridgeMultiHopProof` at H = 1 with
     `hop_start = fake_X.block_id`,
     `hop_end = Y.block_id`,
     `refs_tree_depth = 1`, `ref_index = 1`, `L7_inner_path = [parent_placeholder_leaf]`.
     The circuit's constraints (spec §4.2, §6.6) check that
     `fake_X.L7 == open(Y_leaf, ref_index = 1, path, depth = 1)` and that
     `fake_X.L7` is reachable from `fake_X.block_id` via the outer depth-4
     SHA path. Both hold by construction — the attacker built `fake_X.L7`
     precisely to fit.
   - Y-side: the standard anchor path against `Y.block_id → block_leaf(Y) →
     #L1(M_Y) → … → finalRoot`. This is real chain data.

5. **Submit the bundle to `withdrawByProofBundle`.** Solidity phase 1 (spec
   §6.4) checks nullifier freshness, anchor membership in `layerWindows[]`,
   and continuity `hop_start == x_block_id`, `hop_end == y_block_id`. All
   pass on the attacker's crafted values. Phase 2 runs the SHPLONK
   verifiers; both succeed. Phase 3 settles: `_payoutWithdrawal` sends the
   attacker-chosen amount to the attacker-chosen recipient.

The chain never emitted the event. No producer ever signed `fake_X.block_id`.
No BLS attestation ever covered it. The bridge circuit has no way to know,
because in Direction (a) the walk *starts* at X and X is only ever compared to
itself.

**The general shape of the failure.** For anchoring to be sound, the walk's
starting endpoint must be a block whose identity is already committed to
on-chain. In this design that means a thread-0 block whose `block_leaf`
folds into a `finalRoot ∈ layerWindows[]`. Y is such a block; X is not. A
walk that starts at Y and *ends* at X inherits Y's on-chain commitment and
therefore transitively binds X. A walk that starts at X and ends at Y proves
only that "if X existed, it referenced Y", and *that* is a vacuously true
statement about a block that need not exist.

**Same-thread walk-back does not rescue Direction (a).** A variant of (a)
that walks same-thread (via slot 0 of L7, the excluded `parent_block_id`)
from X to some descendant X' does not help either — X' is still not
anchored unless the walk eventually crosses into thread 0, at which point
it degenerates to Direction (b) with extra steps.

**Why the initial framing missed this.** The intuition was "X.block_id is a
Merkle tree; walking it via refs to Y ties X to Y". True — but Merkle
inclusion is *bidirectional as a hash chain*: an opening that shows Y ∈ X
also shows that X's committed L7 mentions Y. That direction is worthless
unless X is itself on a chain of trust. Direction (b) starts the walk on
the trust chain; Direction (a) does not.

### 3.2 Direction (b) — Y → X  *(the sound direction; committed to)*

Walk starts at a *newer* thread-0 block Y. Each hop opens Y.refs (then the
next block's refs, etc.), stepping backward until reaching X. Y is anchored
on-chain via its batch tree; because Y.block_id commits to Y.refs, Y's
inclusion on-chain implies Y observed everything reachable through Y.refs,
including X.

The reduction chain under (b) is

```
finalRoot ∈ layerWindows[]  ⊃  block_leaf(Y)  ⊃  Y.block_id  ⊃  Y.L7  ⊃  Y1.block_id  ⊃  …  ⊃  X.block_id  ⊃  X.L8  ⊃  event
                                                                                                 |
                                                                                                 └── X is now committed on-chain via Y's anchor
```

Every step is a cryptographic opening from an on-chain-known value. There is
no free endpoint anywhere in the chain — every witnessed `block_id` appears
either as the walk's output (X, bound by all preceding openings) or is
transitively opened from an already-bound predecessor.

For (b) to be usable end-to-end, we need: **for every event-carrying
non-thread-0 block X, some later thread-0 block Y transitively references X
via cross-thread refs, within ≤ L_MAX hops, and that Y's batch is still in
`layerWindows[anchorLayer]` on Ethereum when the withdrawal is claimed.**

This is a purely termination-side question. Soundness is settled: (b) is the
only direction that ties X to `layerWindows[]`.

### 3.3 Witness-latency implication of committing to (b)

The two directions differ sharply in *when* the relayer can build the
private witness for a given event, independent of whether a walk exists at
all.

- **Direction (a).** Would have been predictable and bounded — once the
  event block X is finalized, `X.refs` is fixed on-chain and readable now;
  every subsequent hop just fetches a ref that already exists in chain state.
  Total wait ≈ `time_to_finalize(X) + K × GQL_round_trip`, K ≤ L_MAX. This
  was the operational appeal of (a) in the earlier draft. It does not
  compensate for §3.1.1.

- **Direction (b).** Unpredictable, unbounded from code. The relayer must
  poll for *some* future thread-0 block Y that transitively references X.
  Nothing in the node scheduler binds when or whether that happens:
  `should_include` (`process.rs:530-540`) only fires when other threads have
  advanced between two consecutive thread-0 productions; the checkpoint
  stride (`process.rs:578-628`) permanently skips non-checkpoint
  intermediates; `evaluate_thread_lag` (`cross_thread_ref_enforcement/mod.rs:150`)
  explicitly does not constrain per-block coverage
  ("*Any advance passes, however small and however far behind the result
  still is.*"). Witness-availability latency is therefore not derivable
  from code, and the relayer's build pipeline must accommodate arbitrary
  wait times per event, plus a policy for events that never gain an anchor
  Y within the on-chain retention window.

The waiting problem under (b) is a **first-order operational problem** the
bridge team must solve — with a walker that polls the GQL surface, a
retention policy for un-anchored events, and (if the empirical latency is
unacceptable) either a node-side coverage guarantee (§5 ask 2) or a
same-thread walk-back to a descendant X' that *is* eventually referenced
(§5 ask 3).

## 4. What acki-nacki does and does not guarantee for Direction (b)

### 4.1 Direction (a) — the walk can start with an empty ref set  *(rejected direction; kept as background)*

Even leaving §3.1.1's soundness break aside, Direction (a) has a termination
failure of its own: the walk from X can have no first hop.

- **Postmortem blocks always have `refs = []` by construction.**
  `node/src/block/producer/process.rs:384-385`:
  ```rust
  let refs = if is_postmortem_block {
      Default::default()
  } else { /* ... 250-line pipeline ... */ };
  ```
  A postmortem block that carries a withdrawal event has no starting edge
  for the (a)-walk.

- **The `should_include` filter can empty the ref list for a normal block.**
  `node/src/block/producer/process.rs:530-540`:
  ```rust
  let should_include = match initial_state_clone
      .thread_refs_state.all_thread_refs().get(&thread_id) {
      Some(existing_ref) => seq_no > existing_ref.block_seq_no,
      None => true,
  };
  if !should_include { return None; }
  ```
  A candidate is kept only when *that other thread's* finalized tip has moved
  past what this producer previously referenced. If, between two consecutive
  productions of thread t, no other thread finalized a new block, then every
  candidate is dropped and the produced block carries `refs = []`. Under
  measured devnet rates (~3 blocks/s per thread, see the bridge team's
  `bridge_devnet_source_rate` note) this is not exotic.

- **Consensus-level validation does not require any thread-0 ref.**
  `node/libs/thread-reference-state/src/lib.rs:372-406` (`can_reference`)
  rejects only refs that go *behind* an already-recorded cutoff. Empty ref
  sets and ref sets that contain no thread-0 block pass validation.

- **The `delay-references` feature does not add a thread-0 ref.** It walks
  `⌈√N⌉` ancestors back from candidates already in the buffer
  (`node/src/block/producer/process.rs:402-409, 431-452`) and may return
  `None`, dropping the candidate. It never *introduces* a thread-0 tip.

### 4.2 Direction (b) — later thread-0 refs can skip X permanently

- **Producer refs jump to the *newest* tip and skip intermediates.** The
  `should_include` filter above (`process.rs:530-540`) is per-thread
  monotone: once thread-0 references thread-t at seq_no = N, it will next
  reference thread-t at some seq_no > N, whatever the current tip is.

- **The checkpoint stride limits jumps but does not backfill.**
  `node/src/block/producer/process.rs:578-628`: if the candidate tip is more
  than `CROSS_THREAD_REF_HEIGHT_STEP = 10` heights past the last reference,
  the producer picks the next checkpoint height instead of the tip
  (line 604: `let target = next_cross_thread_ref_height(last_referenced_height);`).
  The skipped non-checkpoint blocks in between are never revisited by any
  later reference from this thread.

- **The consensus lag check enforces the pointer, not per-block coverage.**
  `node/src/protocol/cross_thread_ref_enforcement/mod.rs:137-186`
  (`evaluate_thread_lag`) rules a candidate `Ok` as soon as *any* advance
  happens (line 150: `if advanced(referenced_height, parent_referenced_height) { return Ok }`),
  and otherwise as long as the *distance to the finalized tip* is within
  `max_lag`. Its own doc-comment (line 131-132) is explicit: **"Any advance
  passes, however small and however far behind the result still is."** It
  says nothing about which blocks between the old and new referenced tip are
  covered.

- **`helpers/proof_helper/` does not close the gap either.** `proof.rs`,
  `blockchain.rs` and `main.rs` build layer-0 / layer-N proofs *within a
  single thread's history*. There is no cross-thread walk-builder anywhere
  in acki-nacki that we could copy or trust.

**Consequence for (b):** if thread t produces X, X+1, X+2, …, X+15 in a burst
before thread-0's next production, thread-0's next block will reference the
checkpoint at X+10 (not the tip, not X). X, X+1, …, X+9, X+11, …, X+14 are
permanently skipped by *direct* cross-thread refs from thread-0. Whether
they are reachable *transitively* — via a thread-0 block that references
some thread-t' block that in turn references X — depends on the ref-DAG's
shape, and is exactly what the bridge team must measure empirically before
locking `L_MAX` (§6).

## 5. What we are asking the node team

**Bridge team's current position.** We are committing to Direction (b) for
the implementation — it is the only direction that is cryptographically
sound (§3.1.1). Direction (a)'s witness-latency advantage does not
compensate. The plumbing changes (b) needs — walker rewrite in
`bridge-event-witness`, endpoint swap in `bundle_verifier` and
`AckiNackiBridge.sol`, verifier redeployment — are local to the bridge
side; the circuit gadget itself is direction-agnostic (opens a leaf against
a Merkle root) and needs no algebraic change.

Under (b), events whose block X is never transitively referenced by any
thread-0 block within the retention window are un-provable. The relayer
will surface them as an error rather than attempt a fallback until we have
measured how often the case actually occurs.

We would like the node team's position on one of the following:

1. **~~Direction-(a) guarantee.~~** *(No longer relevant. Direction (a) has
   been ruled out as unsound — see §3.1.1. A producer-side rule making
   every non-thread-0 block's `refs` transitively reach thread 0 would fix
   (a)'s termination problem but would not fix its soundness problem.)*

2. **Direction-(b) coverage guarantee.** Add a rule such that every finalized
   non-thread-0 block is referenced (directly or transitively) by *some*
   later block in another thread within ≤ L_MAX hops and within the
   Ethereum-side `layerWindows` retention window. This eliminates the
   checkpoint-stride skipping problem for events specifically. This is the
   ask most directly compatible with (b).

3. **A same-thread walk-back opt-in.** Currently spec §2.3 / §4.1 forbids
   opening slot 0 of L7 (`parent_block_id`). If the node protocol cannot
   provide (2), the bridge can absorb the gap by walking forward from X to
   some descendant X' in the same thread that *did* get cross-thread
   referenced (from a thread-0 anchor Y). The Y → X' segment stays
   Direction-(b); the same-thread X' → X extension is a linear parent-chain
   walk that binds X to X' through same-thread `parent_block_id` edges,
   which acki-nacki does guarantee. Requires no acki-nacki protocol change
   but does require the node team to confirm the same-thread parent chain
   is stable enough to open in circuit.

4. **Explicit "we don't guarantee this and you have to live with lost
   events"** — in which case the bridge relayer will need a documented
   fallback (skip, retry later, off-chain refund) and users need to be told
   that some withdrawals may be unprovable.

## 6. What we would like to measure before landing this

We plan to run the multithread test at
`acki-nacki/tests/mt/cli.py test-multithread-cross-thread` with the
sustained-load recipe from `acki-nacki/MULTITHREAD_TEST_SESSION.md:54-77`,
query `proof_block_refs` via GQL, and compute:

- **anchor-latency distribution.** For every finalized non-thread-0 block X
  in the observation window, measure the wall-clock delay `T_anchor(X)` from
  X's finalization to the first thread-0 block Y whose ref-DAG transitively
  reaches X within ≤ L_MAX hops. This is the (b)-direction witness-build
  latency.
- **anchor-loss rate.** Fraction of finalized non-thread-0 blocks that
  *never* gain a transitive thread-0 anchor within the retention window of
  `layerWindows[anchorLayer]` on Ethereum. Any nonzero rate means the (b)
  walk simply has no path for some subset of events — see §5 asks 2/3/4.
- **empirical L distribution** for those blocks that *do* have a walk —
  informs `L_MAX` sizing and (since bundle verification cost scales
  linearly in L at H = 1) the outer-aggregator threshold.

If either of the first two quantities is materially adverse, we come back
to §5 asks (2)–(4).

## 7. Files cited

Acki-nacki, current tree:

- `node/src/multithreading/thread_synchrinization_service.rs:67-77`
- `node/src/block/producer/process.rs:384-385, 402-409, 431-452, 458-633, 530-540, 578-628`
- `node/libs/thread-reference-state/src/lib.rs:372-406`
- `node/src/protocol/cross_thread_ref_enforcement/mod.rs:105-114, 137-186`
- `helpers/proof_helper/src/{proof.rs, blockchain.rs, main.rs}` — walks single-thread layer trees only
- `tests/mt/cli.py` — `test-multithread-cross-thread` entry
- `MULTITHREAD_TEST_SESSION.md:54-77` — sustained-load recipe

Bridge, current tree:

- `crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md`
  — §1 (framing), §2.1 (opening cost of L7 and L8 leaves), §2.3 (L7 tree
  shape), §4 (walk), §4.4 (L_MAX = 300), §6.3–6.4 (public inputs and
  bundle-verifier orchestration), §6.7 (event-block reconstruction).
- `crates/bridge-prover-libraries/bridge-event-witness/src/enrich.rs`
  — the walker that produces the multi-hop witness; must be rewritten under
  Direction (b) to poll for a future anchor Y and BFS through Y.refs to
  reach X, rather than walking X.refs.
- `contracts/ethereum/src/AckiNackiBridge.sol` — `withdrawByProofBundle`
  continuity check; endpoint labels swap when (b) lands (see spec §6.4).
