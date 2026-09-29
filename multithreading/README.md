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
  cross-thread `refs` edges.

The current spec calls this walk "the L7 walk" (spec §4). It uses acki-nacki's
per-block `refs` list, opened through the block's L7 Poseidon dense-Merkle root.

## 2. The walk

The walk is

```
 Y_anchor  <-  Y  =  B_0  <-  B_1  <-  ...  <-  B_L  =  X
     ^         ^                                    ^
     on-chain  thread 0 (newer)                     thread t (event, older)
     root
```
Here cryptographically ` A <- C` arrow means that C is leaf, A -- Merkle root, containing this leaf.

But speaking about real grabbing private witness : the walk begins at the thread-0
block Y (proven against the on-chain-known batch root Y_anchor), with
`hop_start_block_id = Y.block_id` and `hop_end_block_id = X.block_id` (the
walk terminates at the event block).

Arrow `<-` reads **"left block's `refs` contains right block"**:

- `B_0.refs` contains `B_1`,
- `B_1.refs` contains `B_2`,
- …
- `B_{L-1}.refs` contains `B_L = X`.

Each hop opens `B_i.L7` and extracts a slot whose leaf is `B_{i+1}.block_id`. Slot 0 of L7 (`parent_block_id`) is excluded, so only cross-thread ref slots are hopped. Because acki-nacki refs are
strictly older-cross-thread (§2.1), every hop moves backward in wall-clock
time and never stays in the same thread.

The production ceiling declared in the spec is `L_MAX = 300`,
attributed to "the node-team-stated cross-thread walk-length ceiling under the
current threading design".

### 2.1 The reduction chain

Under the Y-start walk the reduction chain is

```
finalRoot ∈ layerWindows[]  ⊃  Y_anchor  ⊃  block_leaf(Y)  ⊃  Y.block_id  ⊃  Y.L7  ⊃  Y1.block_id  ⊃  …  ⊃  X.block_id  ⊃  X.L8  ⊃  event
                                                                                                              |
                                                                                                              └── X is now committed on-chain via Y_anchor
```

Every step is a cryptographic opening from an on-chain-known value. There is
no free endpoint anywhere in the chain — every witnessed `block_id` appears
either as the walk's output (X, bound by all preceding openings) or is
transitively opened from an already-bound predecessor.

For the walk to be usable end-to-end, we need: **for every event-carrying
non-thread-0 block X, some later thread-0 block Y transitively references X
via cross-thread refs, within ≤ L_MAX hops, and Y is committed inside some
Y_anchor that is still in `layerWindows[anchorLayer]` on Ethereum when the
withdrawal is claimed.**

Soundness is settled by starting at Y. The question that remains is
termination — and from reading the code we cannot find a mechanism that
guarantees it.

### 2.2 Witness-latency implication

The Y-start walk has unpredictable, unbounded witness-build latency. The
relayer must poll for *some* future thread-0 block Y that transitively
references X. Nothing in the node scheduler binds when or whether that
happens:

- `should_include` (`process.rs:530-540`) only fires when other threads have
  advanced between two consecutive thread-0 productions;
- the checkpoint stride (`process.rs:578-628`) permanently skips non-checkpoint
  intermediates;
- `evaluate_thread_lag` (`cross_thread_ref_enforcement/mod.rs:150`) explicitly
  does not constrain per-block coverage ("*Any advance passes, however small
  and however far behind the result still is.*").

Witness-availability latency is therefore not derivable from code, and the
relayer's build pipeline must accommodate arbitrary wait times per event, plus
a policy for events that never gain a Y whose Y_anchor is still in the
on-chain retention window. This is a **first-order operational problem** the
bridge team must solve — with a walker that polls the GQL surface, a
retention policy for un-anchored events, and (if the empirical latency is
unacceptable) either a node-side coverage guarantee (§4 ask 1) or a
same-thread walk-back to a descendant X' that *is* eventually referenced
(§4 ask 2).

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

We plan to run the multithread test at
`acki-nacki/tests/mt/cli.py test-multithread-cross-thread` with the
sustained-load recipe from `acki-nacki/MULTITHREAD_TEST_SESSION.md:54-77`,
query `proof_block_refs` via GQL, and compute:

- **anchor-latency distribution.** For every finalized non-thread-0 block X
  in the observation window, measure the wall-clock delay `T_anchor(X)` from
  X's finalization to the first thread-0 block Y whose ref-DAG transitively
  reaches X within ≤ L_MAX hops. (User-visible latency also includes the
  Y_anchor commit delay, but that lives outside this experiment.) This is
  the witness-build latency.
- **anchor-loss rate.** Fraction of finalized non-thread-0 blocks that
  *never* gain a transitive thread-0 Y within the retention window of
  `layerWindows[anchorLayer]` on Ethereum (i.e. no Y is committed via any
  Y_anchor that is still on-chain). Any nonzero rate means the walk simply
  has no path for some subset of events — see §4 asks 1/2/3.
- **empirical L distribution** for those blocks that *do* have a walk —
  informs `L_MAX` sizing and (since bundle verification cost scales
  linearly in L at H = 1) the outer-aggregator threshold.

If either of the first two quantities is materially adverse, we come back
to §4 asks (1)–(3).

## 5. Files cited

Acki-nacki, current tree:

- `node/src/multithreading/thread_synchrinization_service.rs:67-77`
- `node/src/block/producer/process.rs:458-633, 530-540, 578-628`
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
  — the walker that produces the multi-hop witness; polls for a future
  thread-0 Y (committed via some Y_anchor) and BFS-walks Y.refs backward to
  reach X.
- `contracts/ethereum/src/AckiNackiBridge.sol` — `withdrawByProofBundle`
  continuity check (spec §6.4).
