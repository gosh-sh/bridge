# Draft — cross-thread reachability guarantees for bridge event proofs

## 1. What the bridge is trying to do

The bridge circuit spec is
[`bridge/crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md`](../crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md).

- A user emits a `WithdrawalInitiated` event on Acki Nacki. The event lives in
  some block **X** in thread **t**. Under sharding, `t` can be any thread —
  including a thread ≠ 0.
- The Ethereum side (`AckiNackiBridge.sol`) can only trust a thread-0 layer-N
  batch root — see spec §1.2 (`_isKnownLayerAnchor` reads
  `layerWindows[anchorLayer]`, populated only from thread-0 blocks). Call the
  batch root recorded on Ethereum **Y_anchor**, and call the thread-0 block
  the cross-thread walk starts from **Y**. Y is proven against Y_anchor by a
  same-thread opening inside the anchor's batch tree; when the batch commits
  to a single block, Y = Y_anchor.
- To pay a withdrawal on Ethereum, the circuit has to cryptographically bind
  event → X → Y → Y_anchor → on-chain root. When `t = 0` this collapses to
  X = Y and no cross-thread walk is needed (Y still opens from Y_anchor).
  When `t ≠ 0` the circuit additionally needs a walk between X and Y through
  the block's `proof_block_refs` list.

The current spec calls this walk "the L7 walk" (spec §4). It uses acki-nacki's
per-block `proof_block_refs` list, opened through the block's L7 Poseidon
dense-Merkle root. **On chain, `proof_block_refs[0]` is `parent_block_id`
(the same-thread parent) and `proof_block_refs[1..]` are cross-thread refs**
— both live inside the same L7 tree
(`acki-nacki/helpers/proof_helper/src/gql_proof.rs:76,109`:
`"proof_block_refs must contain parent block id"`). So slot 0 is a valid
chain-side hop; whether the *circuit* opens it is a spec choice — see §2 and
§3.

## 2. The walk

The walk is a path in the block-ref DAG:

```
 Y_anchor  <-  Y  =  B_0  <-  B_1  <-  ...  <-  B_L  =  X
     ^         ^                                    ^
     on-chain  thread 0 (newer)                     thread t (event, older)
     root
```

`A <- C` reads **"C is a leaf inside A's L7 Poseidon dense-Merkle tree"** —
i.e. `C.block_id` appears in `A.proof_block_refs`, and the L7 opening binds
`C.block_id` to the L7 root committed by `A`. Each hop costs an L7 Poseidon
opening plus the SHA-256 gadgetry that folds `B_i.L7` into `B_i.block_id`
into the next step (§2.1 of the spec: **≈ 8 SHA-256 gadgets per hop**), so
`L` — the *count* of hops — is what drives per-event circuit cost.

The walker is producing a witness, not a canonical chain. Any DAG path from
some thread-0 `Y` (later, committed via a still-current `Y_anchor`) down to
`X` (event block, thread `t`) is valid provided every intermediate block-id
is opened from an already-bound predecessor.

### 2.0 Path topologies

`proof_block_refs` slot semantics are set by acki-nacki
(`helpers/proof_helper/src/gql_proof.rs:76,109`):

- **slot 0** = `parent_block_id` (same-thread, previous seq_no)
- **slots 1..** = cross-thread refs (any thread ≠ own, strictly older)

Both live inside the same L7 tree, so opening slot 0 is as cheap and as sound
as opening any cross-thread slot — the "slot 0 is excluded" constraint in
`bridge-event-witness` today is a **circuit-code choice**, not a chain-side
requirement. The measurement work in §4 characterises paths under the DAG's
actual shape; the circuit-spec update that allows the walker to *use* those
paths is tracked separately.

Some representative topologies:

```
(a) Direct cross-thread ref, single hop:            L = 1
    Y (thread 0) ── slot k ──▶ X (thread t)
    Only possible if thread-0 producer directly referenced X. Rare in bursts
    — the checkpoint stride (§3) skips most of thread t's intermediates.

(b) Same-thread walk-back + one cross-thread hop:   L = 1 + Δ
    Y (thread 0) ── slot k ──▶ X' (thread t, seq_no > X)
                               X' ── slot 0 ──▶ X'−1 ── slot 0 ──▶ … ──▶ X
    Δ = (X'.seq_no − X.seq_no). Fires whenever thread 0 refs a *later*
    thread-t block; we walk the same-thread parent chain back to X.
    Only requires 2 threads to exist.

(c) Multi-thread transitive shortcut:               L = h_0 + h_v
    Y (thread 0) ── slot k ──▶ C (thread v ≠ 0, t) ── slot k' ──▶ X (thread t)
    Requires ≥ 3 threads live in the window. Often *shorter* than (b) when
    thread t is deep in a burst but thread v has a fresher direct ref to X.

(d) Arbitrary mix: any DAG path Y ⇝ X interleaving slot-0 (same-thread) and
    slot-≥1 (cross-thread) edges, respecting the strictly-older-cross-thread
    invariant.
```

`bridge/multithreading/research/multipath_collector.py` builds
the full ref-DAG from live GQL data and enumerates paths in this exact
sense, ranking by hop count (shortest first).

### 2.1 Hop cost and the L target

Circuit cost scales linearly in `L` (spec §6.3–6.4: bundle verification is
`H = 1` per hop). The spec's declared ceiling is **`L_MAX = 300`**,
attributed to the node team's stated cross-thread walk-length upper bound
under the current threading design. That is a *safety* cap for the outer
aggregator; the operational target the walker aims for is much smaller —
**`L ≤ 10`** in the common case, since each hop is ≈ 8 SHA-256 gadgets and
the aggregator threshold binds the acceptable bundle depth.

The purpose of §4 is exactly to check that the empirical `L` distribution
under sustained multi-thread load sits well below `L_MAX` and, ideally,
inside the `L ≤ 10` regime — using multi-path BFS (topology (d)), not just
the linear direct-ref chain.

### 2.2 The reduction chain

For any admissible path `Y = B_0 <- B_1 <- ... <- B_L = X` the reduction chain
is:

```
finalRoot ∈ layerWindows[]  ⊃  Y_anchor  ⊃  block_leaf(Y)  ⊃  Y.block_id
   ⊃  Y.L7  ⊃  B_1.block_id  ⊃  B_1.L7  ⊃  B_2.block_id  ⊃  …
   ⊃  X.block_id  ⊃  X.L8  ⊃  event
                              |
                              └── X is now committed on-chain via Y_anchor
```

Each `B_i.L7 ⊃ B_{i+1}.block_id` step is an L7 Poseidon dense-Merkle opening
into an arbitrary slot of `B_i.proof_block_refs` — slot 0 (`parent_block_id`)
included. Every step is a cryptographic opening from an on-chain-known value;
no free endpoint anywhere.

For the walk to be usable end-to-end, we need: **for every event-carrying
non-thread-0 block X, some later thread-0 block Y is reachable from X in the
ref-DAG (via any interleaving of parent-chain and cross-thread edges) within
`≤ L_MAX` hops, and Y is committed inside some Y_anchor that is still in
`layerWindows[anchorLayer]` on Ethereum when the withdrawal is claimed.**

Soundness is settled by starting at Y. The open questions are **existence**
(does *any* path exist?) and **shortness** (how many hops on the shortest?).

### 2.3 Witness-build latency and the walker strategy

The walker polls thread-0 GQL every **~20 s** (roughly the block cadence),
extends the ref-DAG with each new thread-0 block plus its transitively-fetched
predecessors, and runs BFS on the reverse graph rooted at `X`. It reports the
**shortest** `Y ⇝ X` path found within a **budget of ~5 minutes** since the
event's finalisation.

- If a path with `L ≤ 10` shows up quickly: happy path, hand the witness off.
- If only longer paths exist inside the budget: use the best one, log the `L`.
- If no path exists by budget expiry: park the event; either wait for another
  thread-0 anchor or drop it under the operational retention policy.

Latency is not derivable from the acki-nacki scheduler alone —
`should_include` (`process.rs:530-540`) only fires when other threads
advanced between two consecutive thread-0 productions, the checkpoint stride
(`process.rs:578-628`) permanently skips non-checkpoint intermediates, and
`evaluate_thread_lag` (`cross_thread_ref_enforcement/mod.rs:150`) does not
constrain per-block coverage. What §3 *does* give us is a
**freshness bound**: `MAX_UNREFERENCED_THREAD_LAG = 50` limits how far a
lineage's view of a thread may lag before attestation is withheld — see §3.

The empirical `L` distribution and the fraction of events without a path
inside the budget are exactly what §4 measures.

## 3. What acki-nacki does and does not guarantee for termination

- **Producer refs jump to the *newest* tip and skip intermediates.** The
  `should_include` filter (`process.rs:530-540`) is per-thread monotone: once
  thread-0 references thread-t at seq_no = N, it will next reference thread-t
  at some seq_no > N, whatever the current tip is.

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

- **`MAX_UNREFERENCED_THREAD_LAG = 50` bounds freshness, not coverage.**
  `node/src/protocol/cross_thread_ref_enforcement/mod.rs:56`
  (`pub const MAX_UNREFERENCED_THREAD_LAG: u64 = 50;`) plus the doc-comment
  at `mod.rs:10-45` state: a lineage whose view of some other thread is
  more than 50 blocks behind that thread's finalised tip will have its
  attestation withheld — forcing a rebuild that re-references. This is a
  **freshness ceiling on the referenced-tip pointer per lineage**, not a
  per-block coverage guarantee: it caps the *newest gap* a lineage can
  ignore, but says nothing about which of the intermediate blocks in that
  gap are ever directly referenced. Combined with the checkpoint stride
  (`STEP = 10`), the walker can expect thread-0 to eventually reference
  *some* block in each other thread within a bounded number of thread-0
  productions after that thread advances — but that "some block" is
  typically a checkpoint, not the specific event-block `X`.

- **`helpers/proof_helper/` does not close the gap either.** `proof.rs`,
  `blockchain.rs` and `main.rs` build layer-0 / layer-N proofs *within a
  single thread's history*. There is no cross-thread walk-builder anywhere
  in acki-nacki that we could copy or trust.

**Consequence:** if thread t produces X, X+1, X+2, …, X+15 in a burst before
thread-0's next production, thread-0's next block will reference the
checkpoint at X+10 (not the tip, not X). X, X+1, …, X+9, X+11, …, X+14 are
permanently skipped by *direct* cross-thread refs from thread-0. Whether
they are reachable *transitively* — via a thread-0 block that references
some thread-t' block that in turn references X — depends on the ref-DAG's
shape, and is exactly what the bridge team must measure empirically before
locking `L_MAX` (§5).


## 4. What we would like to measure before landing this

We run the multithread test at
`acki-nacki/tests/mt/cli.py test-multithread-cross-thread` with the
sustained-load recipe from `acki-nacki/MULTITHREAD_TEST_SESSION.md:54-77`,
poll thread-0 tips via GQL, and let
`bridge/multithreading/research/multipath_collector.py` do the
work. The collector implements exactly the algorithm §2.3 sketches:

- polls thread-0 GQL on a fixed cadence (~20 s), fetching `proof_block_refs`
  for each new block plus its transitively-fetched predecessors, and
  maintains a global block DAG in memory;
- for every finalised non-thread-0 candidate `X` in the observation window
  (default 300 s per candidate, matching the §2.3 5-minute budget), runs
  BFS on the reverse graph rooted at `X` over *all* edge kinds (slot 0 and
  slot ≥1), enumerating up to `K` shortest distinct paths;
- emits one JSONL record per candidate with the shortest-path length,
  path bodies, length histogram, and wall-clock timings.

Aggregated over a session this gives us:

- **shortest-`L` distribution** — the primary sizing input. Ideally the
  bulk sits in `L ≤ 10` (§2.1 target); the far tail informs whether
  `L_MAX = 300` is comfortable or tight.
- **time-to-first-path distribution** `T_first_path_wall_s` — the
  witness-build latency §2.3 discusses. This is what the relayer's build
  pipeline has to accommodate per event.
- **time-to-shortest-path distribution** `T_best_path_wall_s` — how much
  extra delay a caller pays if they wait for a *good* path rather than the
  first-any path.
- **no-path-in-budget rate** — fraction of candidates for which the BFS
  found no `Y ⇝ X` path inside the observation window. Any nonzero rate is
  a hard operational problem (retention policy, node-side guarantee, or
  same-thread walk-back — see §4 asks 1/2/3 below).
- **edge-type breakdown of shortest paths** — how often the shortest path
  uses slot-0 (parent-chain) edges vs slot-≥1 (cross-thread). Directly
  motivates the circuit-side spec update to allow slot-0 openings.

If either the shortest-`L` tail or the no-path rate is materially adverse,
we come back to §4 asks (1)–(3).

## 5. Files cited

Acki-nacki, current tree:

- `node/src/multithreading/thread_synchrinization_service.rs:67-77`
- `node/src/block/producer/process.rs:458-633, 530-540, 578-628`
- `node/src/protocol/cross_thread_ref_enforcement/mod.rs:56` — `pub const MAX_UNREFERENCED_THREAD_LAG: u64 = 50;` (per-lineage referenced-tip freshness bound)
- `node/src/protocol/cross_thread_ref_enforcement/mod.rs:10-45, 105-114, 137-186`
- `helpers/proof_helper/src/gql_proof.rs:76,109` — `proof_block_refs[0]` is `parent_block_id`; slots ≥1 are cross-thread refs; all live in the same L7 tree
- `helpers/proof_helper/src/{proof.rs, blockchain.rs, main.rs}` — walks single-thread layer trees only
- `tests/mt/cli.py` — `test-multithread-cross-thread` entry
- `MULTITHREAD_TEST_SESSION.md:54-77` — sustained-load recipe

Bridge research, this tree:

- `bridge/multithreading/research/multipath_collector.py` — polls thread-0 GQL, builds the global block-ref DAG, runs BFS-on-reverse-graph rooted at each candidate `X` to enumerate the shortest `Y ⇝ X` paths (all edge kinds), emits JSONL `direction_b_multipath.v1` with shortest length, path bodies, length histogram, and wall-clock timings.

Bridge, current tree:

- `crates/bridge-circuits/bridge-event-prove-circuit/docs/MULTITHREAD_BRIDGE_EVENT_CIRCUIT_SPECIFICATION.md`
  — §1 (framing), §2.1 (opening cost of L7 and L8 leaves), §2.3 (L7 tree
  shape), §4 (walk), §4.4 (L_MAX = 300), §6.3–6.4 (public inputs and
  bundle-verifier orchestration), §6.7 (event-block reconstruction).
- `crates/bridge-prover-libraries/bridge-event-witness/src/enrich.rs`
  — the walker that produces the multi-hop witness; polls for a future
  thread-0 Y (committed via some Y_anchor) and BFS-walks Y.refs backward to
  reach X.
- `contracts/ethereum/src/AckiNackiBridge.sol` — `withdrawByProofBundle`
  continuity check (spec §6.4).
