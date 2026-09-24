# Bridge Prover Thinning — Design Spec

**Status:** Design proposal. Not implemented.

**Companion:** [`GLOBAL_HISTORY_DATA_SPEC.md`](GLOBAL_HISTORY_DATA_SPEC.md) defines the
on-chain layer-hash protocol. This spec proposes a *cadence change* on top of it.
Where they disagree, the canonical spec wins until this one is adopted.

**Hard invariant inherited from circuits:** `MAX_CHAIN_LEN = 11`
(`gosh-halo2-crypto-lib/dense-balanced-tree/src/lib.rs:25`).

---

## 1. Motivation

### 1.1 Source rate baseline

Master-thread block rate observed on local devnet
(`local_gossip_nodes-*`) on 2026-05-21:

```
seq_no  2504 → 2596   (Δ = 92 blocks)
gen_utime  1779335822 → 1779335853   (Δ = 31 s)
rate ≈ 2.97 blocks/s  (cumulative since node start: 3.01 b/s over 14 min)
```

Working assumption for the rest of this spec: **~3 source blocks/s**. This is the
*locally-observed* rate; if production targets a lower sustained rate (devnet
configurations often run as fast as possible because they lack real network
latency), the throughput numbers below need to be re-derived from the actual
prod-target rate.

### 1.2 The gap

Today the prover submits a (Circuit 1A + Circuit 2) bundle for **every** key block
(every `W` source blocks). At `W = 128` and 3 b/s, that's one key block every
~43 s; the prover does ~4–5 min per bundle. The prover is ~7× behind the source
chain. At `W = 8` (test) the gap is ~110×. The E2E test never finishes.

Circuit 1A (BLS, K=20) dominates and is invariant w.r.t. `W`: it proves attestation
on a single block regardless of elapsed source blocks. The only prover-side lever
is to make each bundle cover more source blocks — **thinning**: relay one bundle
every `P` key blocks (`P > 1`), targeting heights `K_n = n · P · W`.

Circuit 2 already supports a chain of up to `MAX_CHAIN_LEN = 11` dense Merkle
proofs (`verify_chain_of_dense_proofs`); today the chain is length 1–2 with the
remaining slots inactive padding. Thinning uses more of that budget.

At 3 b/s, thinning alone within the existing `W = 128` and `MAX_CHAIN_LEN = 11`
ceilings cannot reach comfortable slack — see §3.4 and §9. The spec still
documents `P = 8` as the recommended setting, but flags the structural shortfall
and the producer-side levers (larger `W`) that may also be needed.

---

## 2. Constraints

### 2.1 Circuit invariants

| Invariant | Value | Source |
|-----------|-------|--------|
| `MAX_CHAIN_LEN` | 11 | `dense-balanced-tree/src/lib.rs:25` (shared by Circuits 2 & 4) |
| `MAX_LAYERS` | 10 | `LayerNumber ∈ {0..10}`; on-chain layers 1..10 |

Increasing `MAX_CHAIN_LEN` changes circuit shape and invalidates VKs — out of scope.

### 2.2 Layer cadence (canonical, unchanged)

Per `GLOBAL_HISTORY_DATA_SPEC.md` §3.2: layer-`L` hash is emitted at heights
`h ≠ 0 ∧ h % W^L == 0`. This is a **producer** property. Thinning changes only
how often the prover relays, not when the producer emits.

### 2.3 Leaf layout of every `L`-tree

Per `node/src/types/history_proof.rs:298–321` and `block_producer.rs:1331–1404`,
the layer-`L` Poseidon tree built at an `L`-key-block has leaves:

```
[0]      R_{L+1}@h_prev    (most recent higher-layer root before this tree)
[1]      R_L@h_prev        (most recent same-layer root before this tree)
[2..W+1] the W layer-(L-1) hashes from the just-filled L-(L-1)-window
         (padded to next power of two)
```

A dense-merkle proof against `R_L`'s root therefore witnesses inclusion of any
one of those leaves. Three semantic moves, **same primitive**:

| Move | Leaf | Tree root | Position | Effect |
|------|------|-----------|----------|--------|
| **Climb** | `R_{L-1}@h'`, `h' ∈ ((m−1)·W^L, m·W^L]` | `R_L@(m·W^L)` | 2..W+1 | up to `W` layer-(L−1) positions in 1 step |
| **Forward** | `R_L@((m−1)·W^L)` | `R_L@(m·W^L)` | 1 | exactly `W^L` blocks |
| **Descend** | most-recent `R_{L+1}` before `m·W^L` | `R_L@(m·W^L)` | 0 | up to `W^{L+1}` blocks downward |

`verify_chain_of_dense_proofs` doesn't restrict which position is used; the
witness builder freely interleaves all three. This is the core capability
thinning exploits.

### 2.4 Circuit 2 public instances (unchanged)

| Idx | Value |
|-----|-------|
| 0 | `block_id` (SHA-256 root of anchor block) |
| 1 | `bk_set_poseidon_hash` |
| 2 | `num_layers` |
| 3..12 | `layer_hash_frs[0..9]` (inactive = 0) |
| 13 | `prev_max_level_layer_hash` |

The chain proves `prev_max_level_layer_hash → layer_hash_frs[num_layers − 1]`
via ≤ `MAX_CHAIN_LEN` hops. Intermediate `layer_hash_frs[i]` are bound to the
chain by existing circuit constraints (§5.2).

---

## 3. The thinning parameter `P`

### 3.1 Definition

Prover targets heights `K_n = n · P · W` for `n = 1, 2, 3, …`. `P = 1` is today's
protocol; `P > 1` is thinning. The bundle's anchor (Circuit 2 instance 0,
Circuit 1A BLS) is `K_n`; intermediate key blocks `K_{n-1} + iW` enter only via
their layer-1 roots on the dense chain.

### 3.2 Order frequency

`order(K_n) = max L` such that `K_n % W^L = 0`, i.e., `n · P ≡ 0 (mod W^{L−1})`.
For `W = 128`, `P = 8` at 3 b/s (bundle cadence `P·W / rate = 1024 / 3 ≈ 5.7 min`):

| L | Cadence (bundles) | Real time |
|---|------|------|
| 1 | every 1 | ~5.7 min |
| 2 | every 16 | ~91 min (~1.5 h) |
| 3 | every 2 048 | ~8 days |
| 4 | every 262 144 | ~2.8 years |
| 5 | every 33.5 M | ~363 years |

### 3.3 Chain-length analysis

Let `L_new = order(K_n)`, `L_prev = order(K_{n−1})`, gap `G = P · W`.

**Case A (`L_new = 1`, ~94 % of bundles for `P=8`, `W=128`):** no high-layer
compression at `K_n`. Best shape:
- `L_prev = 1` (typical): `P` forward hops at L=1.
- `L_prev ≥ 2`: `L_prev − 1` descend hops, arriving at `R_1@K_n` directly. The
  descend works because `R_1@K_n`'s tree has the most recent `R_2` *before*
  `K_n` at position 0 — which is exactly `R_2@K_{n−1}` when no new `R_2` was
  emitted in between (i.e., whenever `L_new = 1`).

**Case B (`L_new ≥ 2`, ~6 %):** the tree `R_{L_new}@K_n` has the `L_{L_new−1}`
window at positions 2..W+1. If `R_{L_prev}@K_{n−1}` is `L_{L_new−1}`-aligned
(true when `L_prev ≥ L_new − 1`), 1 hop suffices. Otherwise climb the missing
layers at `K_n`: `L_new − L_prev − 1` extra hops. Worst sub-case `L_prev = 1`,
`L_new = L`: `L − 1` hops.

**Combined worst case:**
```
chain_len(K_n) ≤ max(P, L_max − 1)
```
For `P = 8`, `W = 128`: at `L_max = 9` (astronomical horizon), chain ≤ 8;
at `L_max = 10` (theoretical max, unreachable), chain ≤ 9. Always ≥ 2 slots
of margin under `MAX_CHAIN_LEN = 11`.

### 3.4 Choosing `P`

Constraints, hardness-ranked:
1. `P ≤ MAX_CHAIN_LEN = 11` *(hard)*.
2. `P | W` *(soft: preserves `layerWindows[L ≥ 2]` cadence — see §8.2)*.
3. `P` a power of 2 *(nicety)*.

For `W = 128`, at 3 b/s with prover cost ~5 min/bundle:

| `P` | Chain margin | Cadence (`P·W/3` s) | Prover slack | Verdict |
|-----|--------------|---------------------|--------------|---------|
| 2 | 9 | ~85 s | ~0.28× | impossibly behind |
| 4 | 7 | ~2.8 min | ~0.57× | impossibly behind |
| 8 | 3 | ~5.7 min | ~1.14× | breakeven, no margin |
| 11 (drops `P\|W`) | 0 | ~7.8 min | ~1.6× | tight; `L≥2` cadence stretches 11× |

**At 3 b/s the existing `W = 128` window cannot reach comfortable slack within
the soft `P | W` constraint.** `P = 8` is the largest valid choice and gives
only ~1.1× slack — wiped out by any GC pause, GraphQL hiccup, or PK reload.

**Production recommendation (revised): `P = 8`** as the immediate landing target,
**with the explicit understanding that one or more of the following producer-
or prover-side changes is required to reach durable slack:**

1. **Larger `W`** (producer-side). With `W = 256`, `P = 8`: cadence
   ~11.4 min, slack ~2.3×. With `W = 512`, `P = 8`: ~22.7 min, ~4.5×.
2. **Drop `P | W`** to use `P = 11`. Saves another ~37 % cadence but
   stretches `L ≥ 2` `layerWindows` cadence by `P` (§8.2) — hurts
   withdrawal-anchor freshness at higher layers.
3. **Faster prover.** Circuit 1A (BLS K=20) is the dominant cost; any
   reduction there directly translates into slack.
4. **Slower devnet** for the test path. If the local rate is not a hard
   production requirement, throttling the devnet (or running the test
   against a controlled-rate harness) restores the §9 analysis.

For tests with `W = 8`, candidates are `{2, 4}` (`P = 8 = W` is degenerate).
At 3 b/s with `W = 8`, `P = 4`: cadence ~11 s vs ~5 min prover = slack ~0.04×
(prover ~28× behind). **No `(W=8, P≤4)` configuration is viable at 3 b/s
without rate-limiting the devnet.** See §13 for test-path recommendations.

---

## 4. Witness orchestration

No circuit code changes. Only the prover-side witness builder changes.

### 4.1 Per bundle (target `K_n`)

1. Read `prev_max_level_layer_hash` from the previous bundle's
   `layer_hash_frs[num_layers − 1]` (persisted locally).
2. Compute `L_new = order(K_n)`, `L_prev = order(K_{n−1})`.
3. Pick a shape (shortest, derived from §3.3):
   - **Case A:** `P` forward hops at L=1, *or* `L_prev − 1` descend hops if
     shorter.
   - **Case B:** one compression hop into the appropriate window of
     `R_?@K_n`, then climb the remaining `L_new − max(L_prev, L_new−1) − 1`
     layers at `K_n`.
4. Build `DenseChainLink { active: true, siblings, position, leaf_native }` for
   each step; siblings from off-circuit Poseidon-tree reconstruction.
5. Pad to `MAX_CHAIN_LEN = 11` with `DenseChainLink::inactive(...)`; set
   `num_active_steps`, `num_layers = L_new`, and
   `layer_hash_frs[0..L_new]` from `common_section.history_proofs[1..L_new]`.

### 4.2 Source data

All available in the source chain: layer-1 roots at intermediate KBs
(`common_section.history_proofs[1]` per block), higher-layer roots at `K_n`,
and `L_0` block-leaves `Poseidon(block_id ‖ envelope_hash ‖ ext_out_root)`. No
new GraphQL field.

### 4.3 Restart catch-up

Read `prev_max_level_layer_hash` from the verifier-daemon (or local store).
Compute `n_start = ⌈last_K / (P·W)⌉ + 1`, target up to
`K_{n_head} = ⌊H / (P·W)⌋ · P · W`. Bundles issued sequentially — each one's
input is the previous one's output.

---

## 5. Circuit 2 — no functional changes

Same public-instance layout. `num_active_steps` is a witness, range-checked
in-circuit to `[1, MAX_CHAIN_LEN]`. Thinning just sets it to the chosen shape's
length. Same circuit, same VK.

### 5.1 Soundness under direction-agnostic chains

`verify_chain_of_dense_proofs` doesn't constrain which position is used at each
step. A witness using the moves catalogued in §2.3 is sound iff each step's
tree actually contains the claimed leaf at the claimed position. The producer's
tree-construction code (`history_proof.rs:298–321`) places leaves
deterministically as described, so a correct witness builder produces a correct
proof; an incorrect one fails Merkle verification at the first divergence.

### 5.2 Intermediate `layer_hash_frs` constraint

When `L_new ≥ 2`, the chain passes through intermediate roots (e.g., at
`L_new = 3`: `R_? → R_2@K_n → R_3@K_n`). The circuit already constrains
`layer_hash_frs[L_new − 2]` to equal the chain's value just before the last
hop. Unchanged under thinning.

> **Implementation note.** Confirm in
> `historical-layer-hashes-movement-checker-circuit/src/circuit.rs` that the
> binding from `layer_hash_frs[i]` to the chain is positional w.r.t.
> `num_layers`, not `num_active_steps`. If the latter, the witness builder may
> need to pad the chain shape (not just `num_active_steps`) so the deepest
> layer hash always lands at the same chain index.

---

## 6. Circuit 4 (event prove) impact

### 6.1 Anchor strategy under thinning

`layerWindows[1]` now only contains roots at `K_n = n · P · W` heights;
`layerWindows[L ≥ 2]` cadence unchanged (§8.2). So for an event at block `B`:

1. Lift `B`'s block-leaf to `R_1@(⌈B/W⌉ · W)` (1 hop).
2. If that height is `K_n`-aligned: anchor at L=1.
3. Otherwise (common under thinning, ~94 % of events): one more climbing hop
   `L_1 → L_2` and anchor at L=2.

### 6.2 Chain budget

`bridge-event-witness-builder` already builds `AnchorRef.dense_chain` of length
`MAX_CHAIN_LEN` (`main.rs:392–404`). Adding one climbing hop is comfortably
within budget. Deepest realistic climb: `MAX_LAYERS − 1 = 9` hops for events
near the L_10 horizon (same as today).

### 6.3 User-facing latency

Mean withdrawal wait = `P·W / (2 · rate)` source-block-times. At 3 b/s,
`W = 128`:

| `P` | Mean wait |
|-----|-----------|
| 1 (unthinned) | ~21 s |
| 4 | ~85 s |
| 8 | ~2.8 min |

User-latency is comfortable in all cases at 3 b/s — the binding constraint
is prover slack (§3.4), not user wait.

---

## 7. Verifier-daemon changes

1. Last-verified target advances by `P · W` per bundle (not `W`).
2. `appendLayer(L, root, K_n)` dispatch loop unchanged — only cadence shifts.
3. `lastHeight[1]` step size is `P · W`; monotone-height invariant preserved.
4. `layerWindows[1]` covers `P · W²` source blocks of recent history
   (~12 h at `P=8`, `W=128` and 3 b/s); `L ≥ 2` horizons unchanged.

---

## 8. Contract changes

### 8.1 Required

No code change. `appendLayer(layer, hashValue, blockHeight)` signature and
invariants unchanged (`GLOBAL_HISTORY_DATA_SPEC.md` §8.4). Only off-chain docs
update: `lastHeight[1]` step is `P · W`, not `W`.

### 8.2 Why `L ≥ 2` cadence is unchanged when `P | W`

`layerWindows[L]` is updated whenever `K_n % W^L == 0`, i.e., `n · P ≡ 0
(mod W^{L−1})`. Real-time cadence: `W^{L−1} / gcd(P, W^{L−1}) · P · W`. With
`P | W`, `gcd(P, W^{L−1}) = P`, simplifying to `W^L` source blocks — same as
today. If `P ∤ W` the cadence stretches by `P`, hurting freshness — why
`P | W` is strongly recommended.

### 8.3 No storage layout change

`HistoryWindow` struct, mapping, per-layer size all unchanged. ~41 KB per
thread (`GLOBAL_HISTORY_DATA_SPEC.md` §8.3).

---

## 9. Throughput analysis

For `W = 128`, `P = 8`, observed local rate 3 b/s:

| Quantity | Value |
|----------|-------|
| Source blocks / bundle | 1 024 |
| Bundle cadence | ~5.7 min |
| Prover bundle cost (Circuit 1A K=20 + Circuit 2 K=17, chain ~8) | ~5 min |
| Slack | ~1.14× |

That is **not** enough margin. A 30 s prover stall (GC, GraphQL re-fetch, PK
reload) consumes the entire budget for that bundle and the backlog grows.

### 9.1 Sensitivity to source rate

| Rate | Bundle cadence at `P=8`, `W=128` | Slack |
|------|----------------------------------|-------|
| 0.32 b/s (legacy doc baseline) | ~53 min | ~10× |
| 1.0 b/s | ~17 min | ~3.4× |
| 2.0 b/s | ~8.5 min | ~1.7× |
| 3.0 b/s (observed local) | ~5.7 min | ~1.14× |
| 5.0 b/s | ~3.4 min | ~0.68× (cannot keep up) |

Recovery levers if local rate is representative of production target — see
§3.4 list (larger `W`, drop `P | W`, faster prover, throttled devnet).

### 9.2 Cost of Circuit 2 with longer chain

### 9.1 Cost of Circuit 2 with longer chain

Poseidon hashing is roughly linear in `chain_len · tree_depth`. At `W = 128`,
`tree_depth = ⌈log₂(W + 2)⌉ = 8`. Chain length goes from ~1–2 to ~8 — ~4–5×
more Poseidon. Circuit 2's `K = 17` is dominated by SHA-256, not Poseidon, so
column count likely tolerates this.

> **Validate before commit.** If `K = 17` overflows, bump to `K = 18`
> (still well within 10× slack) or trim the Circuit 2 SHA-256 budget.

---

## 10. Open issues

1. **Anonymisation leak.** Anchor layer reveals event-age bucket. Out of
   scope for thinning — property of Circuit 4 / verifier policy regardless of
   `P`. See `GLOBAL_HISTORY_DATA_SPEC.md` §8.2 caveat.
2. **Circuit 2 intermediate-layer-hash constraint indexing** (§5.2 note).
   Verify the binding is positional w.r.t. `num_layers`, not
   `num_active_steps`.
3. **`prev_max_level_layer_hash` continuity across order transitions.** When
   `order(K_{n−1}) ≠ order(K_n)`, chain start/end are at different layers.
   Add regression tests for: 1→1 (Case A, common), 1→2 (every 16th),
   2→1 (immediately after), 1→3 (every 2 048th).
4. **Backwards compatibility.** Existing `P=1` bundles must still verify after
   contract upgrade. Contract code doesn't change (§8), so automatic — but the
   verifier-daemon must handle the tail before switching cadence.

---

## 11. Implementation checklist

- [ ] Add `P` as workspace feature (`p-8`, `p-4`, `p-2`) in
      `acki-nacki-to-eth-bridge-halo2-prover/Cargo.toml`, mirroring the
      existing `w-128` / `w-8` pattern.
- [ ] `bridge-event-witness-builder`: implement shape-chooser from §4.1.
      Unit-test each shape (forward, compress, climb, descend, mixed).
- [ ] `bridge-prover-daemon`: target heights `n · P · W`; update catch-up (§4.3).
- [ ] `bridge-verifier-daemon`: increment last-verified target by `P · W` (§7).
- [ ] **No circuit code edits.** Verify with `git diff` against
      `bridge-event-prove-circuit` and
      `historical-layer-hashes-movement-checker-circuit`.
- [ ] Resolve open issue #2 (intermediate-layer-hash constraint indexing).
- [ ] Add E2E test variant with `P = 2`, `W = 8`.
- [ ] Validate Circuit 2 fits at `K = 17` with longer chains (§9.2).
- [ ] Validate Circuit 4 chain budget for deepest climb (§6.2).
- [ ] Document `P` in repo `README.md`.

---

## 12. Future perspective — chains may combine moves freely

`verify_chain_of_dense_proofs` is direction-agnostic at every slot. The shapes
in §3.3 are only what thinning *needs*, not what the circuit *allows*. The full
catalogue (§2.3 — climb, forward, descend) is interleavable in any of the 11
slots. Capabilities below are **not** part of the initial thinning landing;
they're documented for future redesigns.

- **Intermediate `L_2`-alignment shortcut.** In Case A, if some intermediate
  height in `(K_{n−1}, K_n]` is `L_2`-aligned, the chain could go
  `R_1@K_{n−1} → R_2@(intermediate) → R_1@K_n` (climb + descend = 2 hops)
  instead of `P` forward hops. Useful only if `P` is pushed higher than the
  chain budget allows. Witness builder must detect the alignment.

- **Anonymising up-then-down.** Always declare `prev_max_level_layer_hash` at
  the highest on-chain layer, descend in-circuit to the productive start. Costs
  chain slots; hides the actual `L_prev` from the verifier. Irrelevant for
  current bridge but relevant if future protocols want geometry-hiding.

- **Uniform-depth Circuit 4 anchoring.** Always climb to a fixed terminal layer
  regardless of `B`'s age, padding chain length. Closes the anchor-layer
  age-bucket leak (§10 issue 1). Likely needs `K = 20` for Circuit 4.

- **Multi-event aggregation in Circuit 4.** With slack chain slots, descend
  from a shared `L_max` root to a second event's `L_0` leaf. Needs Circuit 4
  public-instance changes; significantly more complex than single-event
  thinning.

- **`L_1 → L_2 → L_1` escalation.** Anchor a just-now event against an
  already-published `L_1` root, when `R_1@(B's L_1 KB)` rolled out of
  `layerWindows[1]` but the covering `L_2` is on-chain. Niche.

---

## 13. Near-term test configuration

The existing `tests/exchange/generate_withdrawals_with_live_event_proving.py`
E2E currently doesn't finish. **At the observed devnet rate of 3 b/s, prover
slack is negative for every `(W ≤ 128, P ≤ 11)` combination at test scale.**
The test cannot keep up no matter how `P` is tuned.

### 13.1 Time budget at 3 b/s

Single-bundle E2E (one event, one Circuit 1+2 bundle, one Circuit 4):

```
total ≈ setup
      + wait for next K_n      (avg P·W / (2·rate) seconds)
      + Circuit 1+2 proving    (~4–5 min, dominated by Circuit 1A)
      + Circuit 4 proving      (~2–5 min)
      + verify on-chain        (~10 s)
      + slack                  (~1 min)
```

At 3 b/s the *wait* is small in all rows below; the binding constraint is that
the source chain produces another `K_n` every `P·W/3` seconds, so the prover
must finish at least that fast to avoid backlog growth on a *multi-bundle* test:

| `W` | `P` | next-`K_n` interval | Prover slack per bundle | Verdict |
|-----|-----|---------------------|--------------------------|---------|
| 8 | 1 | ~2.7 s | ~0.009× | hopelessly behind |
| 8 | 4 | ~10.7 s | ~0.036× | hopelessly behind |
| 128 | 1 | ~43 s | ~0.14× | ~7× behind |
| 128 | 8 | ~5.7 min | ~1.14× | breakeven; first bundle OK, backlog builds |
| 128 | 11 (no `P\|W`) | ~7.8 min | ~1.6× | tight |

A *single-bundle* sanity test (build one bundle, verify it, stop) can still
finish: pick any row, wait for the first `K_n`, prove, verify. The pipeline is
exercised, even if continuous keep-up is not.

### 13.2 Recommendation: pick the orientation first

There is no `(W, P)` combination that gives durable slack at 3 b/s without one
of the levers in §3.4. Two orientations:

**(a) Single-bundle smoke test.** Drop the "continuous" requirement; assert
that one full E2E iteration completes. Use `W = 8`, `P = 4` (smallest config
that exercises thinning), wait for one `K_n`, prove, verify, exit. Expected
wall-clock: ~10 min (dominated by prover). Reveals correctness bugs but says
nothing about throughput.

**(b) Throttled-rate continuous test.** Slow the local devnet — by config or
by a controlled-rate driver in front of the GraphQL endpoint — to something
like 0.3 b/s. Then the historical `W = 8`, `P = 4` arithmetic from the prior
version of this spec holds: bundle cadence ~80 s, prover ~5 min, the test
keeps up if the rate is throttled to ~0.05 b/s or lower. Investigate whether
the devnet has a slot-time knob (likely in the node config / consensus
parameters).

Either path is acceptable for the immediate goal of "make the E2E finish."
Continuous throughput at 3 b/s requires the producer-side / prover-side
changes in §3.4 and is out of scope for the near-term test.

### 13.3 Handling `L ≥ 2` emissions during the test

With `W = 8`, the producer emits `R_2` every `W² = 64` source blocks. At 3 b/s
that's ~21 s — a multi-minute test crosses many `L_2` boundaries. Circuit 2
already handles `num_layers ∈ {1, 2}` and Circuit 4 can pin to L=1 for the
test, so this is functionally fine; just expect Circuit 2 to emit
`num_layers = 2` bundles frequently.

### 13.4 Steps (single-bundle smoke variant)

> **Status (superseded — production-only `W = 128`).** The two-step `W = 8`
> → `W = 128` ladder originally proposed here is no longer the working plan.
> Circuit 2's tree depth is part of the circuit shape (VK/PK are
> depth-specific), so we want test code to exercise only the shape that
> will ever be deployed. The smoke test now runs directly at production
> `W = 128`; the `W = 8` step (and its `tree_depth = 4`) is dropped.
> The synthetic Circuit 2 test inputs are produced by
> `bridge_test_data_gen::layer_hashes::build_synthetic_layer_hashes_input`,
> which hard-codes `TREE_DEPTH = 8`.

1. Confirm `HISTORY_PROOF_WINDOW_SIZE = 128` in
   `acki-nacki/node/libs/history-proof/src/lib.rs` (this matches the value
   the bridge vendors in `bridge_prover_lib::poseidon_dense`).
2. Set `P = 1` in `bridge-prover-daemon` / `bridge-verifier-daemon` for the
   smoke variant; plumb through. (Layer thinning at `P > 1` lands later.)
3. Modify the E2E to *exit after the first successful bundle verification*
   rather than running continuously.
4. Lower per-phase timeouts (`VERIFIER_STATE_TIMEOUT_S = 1200`,
   `RUST_BIN_TIMEOUT_S = 600`).
5. Run. If green, the pipeline is correct at production shape.

### 13.5 Re-measuring rate

The 3 b/s figure is from the local devnet snapshot in §1.1. If devnet config
changes or production targets a different rate, re-measure with:

```
curl -s -X POST http://127.0.0.1/graphql -H 'Content-Type: application/json' \
  -d '{"query":"{ blockchain { blocks(last: 1, thread_id: \"00000000000000000000000000000000000000000000000000000000000000000000\") { edges { node { seq_no gen_utime } } } } }"}'
```

Take two snapshots, divide `Δseq_no` by `Δgen_utime`. Update §1.1 and the §9
sensitivity table accordingly.
