# Draft — cross-thread reachability guarantees for bridge event proofs

Status: **draft**, prepared by the bridge team for discussion with the acki-nacki
node team. Not landed anywhere yet. All acki-nacki citations are file:line
references into the current `acki-nacki` repo state at the time of writing.

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
X  =  B_0  →  B_1  →  ...  →  B_L  =  Y
       ^                                ^
       thread t (event)                 thread 0 (anchor)
```

with `hop_start_block_id = X.block_id` and `hop_end_block_id = Y.block_id`
(spec §6.4). Each hop opens `B_i.L7` and extracts a slot whose leaf hashes to
`B_{i+1}.block_id`. Slot 0 (`parent_block_id`) is excluded (spec §2.3, §4.1),
so only cross-thread refs are hopped.

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

We think the current spec silently assumes both. We are asking the node team
whether the acki-nacki protocol actually guarantees (B) — because from reading
the code we cannot find a mechanism that does, in either walk direction.

## 3. The two possible walk directions

Regardless of the spec's arrow, only two directions are semantically possible
given that acki-nacki refs point **older, cross-thread**
(`node/src/multithreading/thread_synchrinization_service.rs:67-77` —
`list_blocks_sending_messages_to_thread` returns *last finalized* block of every
*other* thread; `node/src/block/producer/process.rs:530-540` — a candidate
enters the ref list only when its seq_no strictly exceeds the previously
referenced tip of that thread).

### Direction (a) — X → Y

Walk starts at X. Each hop opens X.refs (then B_1.refs, etc.), stepping
backward across threads until landing on a thread-0 block Y that is *older*
than X. X.block_id includes X's L7 tree, which cryptographically commits to
X.refs — so X → Y1 → Y2 → … → Y binds X's identity to Y through a chain of
Poseidon/SHA hashes rooted in X.block_id.

For (a) to be usable end-to-end, we need: **every event-carrying non-thread-0
block X has at least one path via its own refs that eventually terminates in a
thread-0 block within ≤ L_MAX hops.**

### Direction (b) — Y → X

Walk starts at a *newer* thread-0 block Y. Each hop opens Y.refs (then the
next block's refs, etc.), stepping backward until reaching X. Y is anchored
on-chain via its batch tree; because Y.block_id commits to Y.refs, Y's
inclusion on-chain implies Y observed everything reachable through Y.refs,
including X.

For (b) to be usable, we need: **for every event-carrying non-thread-0 block
X, some later thread-0 block Y transitively references X via cross-thread
refs, within ≤ L_MAX hops, and that Y's batch is still in
`layerWindows[anchorLayer]` on Ethereum when the withdrawal is claimed.**

Both directions are cryptographically sound given a single valid walk. The
question is whether such a walk exists.

### 3.3 Witness-latency predictability

The two directions differ sharply in *when* the relayer can build the
private witness for a given event, independent of whether a walk exists at
all.

- **Direction (a).** Predictable and bounded. Once the event block X is
  finalized, `X.refs` is fixed on-chain and readable now; every subsequent
  hop just fetches a ref that already exists in chain state. Total wait ≈
  `time_to_finalize(X) + K × GQL_round_trip`, K ≤ L_MAX.

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
  from code, which is a hard operational problem for a relayer that has to
  ship proofs on a fixed cadence.

This is what makes (a) the pragmatic choice even before the (B)-termination
question is answered — (b)'s waiting problem hits every event, not just the
pathological ones.

## 4. Neither direction is guaranteed by acki-nacki today

### 4.1 Direction (a) — the walk can start with an empty ref set

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

**Consequence for (a):** an event in a postmortem block, or in a block produced
during a quiet cross-thread window, has no first hop and is un-provable no
matter what L_MAX we pick.

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
permanently skipped by every cross-thread walk that starts from thread-0.
Events in those blocks are un-anchorable, no matter what L_MAX we pick.

## 5. What we are asking the node team

**Bridge team's current position.** We are committing to Direction (a) for
the near-term implementation — it is the only direction with a predictable
witness-build latency (§3.3), and the plumbing changes it needs are local
to the bridge circuits. Under (a), events in blocks with `refs = []`
(§4.1) are simply un-provable for now; the relayer will surface them as an
error rather than attempt a fallback. Same-thread walk-back and other
fallbacks (options 3 and 4 below) remain on the table for a later
iteration once we have measured how often the empty-refs case actually
occurs, but we are not selecting between them yet.

The bridge circuit can be built for either direction; the choice does not
matter cryptographically as long as termination is guaranteed. What we
cannot build around is a chain where the walk simply has no path for some
subset of events.

We would like the node team's position on one of the following:

1. **Direction-(a) guarantee.** Add a producer-side rule such that every
   non-thread-0 block emits `refs` containing at least one entry that
   transitively (within ≤ L_MAX hops) reaches thread 0. This eliminates
   `refs = []` production and makes the walk from X well-defined.

2. **Direction-(b) coverage guarantee.** Add a rule such that every finalized
   non-thread-0 block is referenced (directly or transitively) by *some*
   later block in another thread within ≤ L_MAX hops and within the
   Ethereum-side `layerWindows` retention window. This removes the
   checkpoint-stride skipping problem for events specifically.

3. **A same-thread walk-back opt-in.** Currently spec §2.3 / §4.1 forbids
   opening slot 0 of L7 (`parent_block_id`). If the node protocol cannot
   provide (1) or (2), the bridge can absorb the gap by walking forward from
   X to some descendant X' in the same thread that *did* get cross-thread
   referenced. This requires no acki-nacki protocol change but does require
   the node team to confirm the same-thread parent chain is stable enough
   to open in circuit.

4. **Explicit "we don't guarantee this and you have to live with lost
   events"** — in which case the bridge relayer will need a documented
   fallback (skip, retry later, off-chain refund) and users need to be told
   that some withdrawals may be unprovable.

## 6. What we would like to measure before landing this

We plan to run the multithread test at
`acki-nacki/tests/mt/cli.py test-multithread-cross-thread` with the
sustained-load recipe from `acki-nacki/MULTITHREAD_TEST_SESSION.md:54-77`,
query `proof_block_refs` via GQL, and compute:

- **rate(a):** fraction of non-thread-0 finalized blocks with
  `proof_block_refs.len() == 1` (i.e. parent only, no cross-thread refs) —
  bounds the (a)-direction failure rate.
- **rate(b):** fraction of non-thread-0 finalized blocks that are never
  referenced by any later block of any other thread within the observation
  window — bounds the (b)-direction failure rate.
- **empirical L distribution** for those blocks that *do* have a walk, in
  both directions, to inform L_MAX sizing.

If either rate is materially nonzero, the current spec is incomplete no
matter which direction it commits to, and we come back to (1)–(4) above.

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
  — §1 (framing), §2.3 (L7 tree shape), §4 (walk), §4.4 (L_MAX = 300), §6.3–6.4 (public inputs)
