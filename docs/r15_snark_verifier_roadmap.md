# R15 — Real on-chain Halo2 verifier via snark-verifier aggregator

**Status:** Roadmap accepted 2026-05-26. **M1 ✅, M2 ✅, M3 ✅** (as of
2026-05-27). **M5 instance-exposure de-risked on the synthetic inner
(2026-05-29)** — `expose_previous_instances` now surfaces the inner PIs as
outer aggregator instances, validated end-to-end through the Yul verifier.
M4 (real Circuit 4 prover) blocked on partner Circuit 4 stability; the rest
of M5 is a data-only swap once M4 lands.
**Owner:** Bridge team.
**Audit finding:** R15 — current `circuit-{1a,1b,2,4}` gnark wrappers are **identity stubs**
(`Define()` body is `for i { api.AssertIsEqual(PI[i], PI[i]) }`). They do **not**
verify the underlying Halo2 SHPLONK proof. A Groth16 proof produced by these
wrappers proves only that "the 10 PIs equal themselves and the domain size is
set". On-chain `BridgeWithdrawalVerifier.sol` therefore provides **zero
cryptographic security** — it is a centralised attestation gated by the relayer.

This document is the roadmap to closing R15 by replacing the gnark identity stub
with a **real** in-circuit Halo2 SHPLONK verifier produced via PSE / axiom-crypto's
[`snark-verifier`](https://github.com/axiom-crypto/snark-verifier) aggregator,
emitting a Yul-based Solidity verifier (and optionally a Groth16 wrapper for
the aggregator's output if gas requires it).

## Historical context

The repository previously contained three Halo2-to-gnark wrappers:

| Path (removed) | Removed in | Define() body |
|---|---|---|
| `deposit-prover/gnark-wrapper/circuit.go` | `09b8842` (2026-05-17, "Groth16 removed from ETH→AN") | identity stub w/ TODOs |
| `layer-hashes-prover/gnark-wrapper/circuit.go` | `5468519` ("Removed legacy code") | identity stub w/ explicit "future work" comment |
| `bk-set-rotation-prover/gnark-wrapper/circuit.go` | `5468519` | identity stub w/ explicit "future work" comment |

The deposit-prover variant had an elaborate scaffold (`Halo2VerifierCircuit`
struct with 50 witness commitments, 147 evaluations, SHPLONK W/W', SRS G2Tau,
"Week 1–4 ✓ Week 5 (current) Week 6 (TODO)" header), but the `Define()` body
was still:

```go
for i := 0; i < 7; i++ {
    api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
}
// NOTE: We skip witness commitment checks because G1Point ([32]byte arrays)
// cannot be directly used in constraints without proper conversion
```

A real Halo2-in-gnark verifier was never built.

## Why we pick the snark-verifier aggregator path

Three options were considered (see chat 2026-05-26):

1. **Write a real Halo2 SHPLONK verifier directly inside gnark** (`sw_bn254`
   pairing gadgets + Fiat-Shamir transcript + PLONK gates + permutation +
   lookup + SHPLONK opening). ≥2 months of one full-time crypto engineer; high
   risk; would re-derive a wheel that `snark-verifier` already provides for
   Halo2.

2. **Use `snark-verifier` aggregator** — verify the inner Halo2 SHPLONK proof
   inside another Halo2 circuit (the aggregator) using existing well-audited
   gadgets, then either:
   - (2a) emit the aggregator's proof to chain via `gen_evm_verifier_shplonk`
     (Yul), giving ~600 k–1 M gas; or
   - (2b) further wrap the aggregator with a hand-written gnark Groth16 circuit
     (Scroll-style), giving ~250–450 k gas at the cost of months of work.

3. **Drop Groth16/gnark entirely and verify Halo2 directly on-chain** via
   existing Yul-based verifiers (`Halo2Verifier.sol`, `Blake2bHalo2Verifier.sol`).
   ~2–3 M gas; bypasses the aggregator; the simplest pipeline but the most
   expensive per call.

**Decision: option 2 (snark-verifier aggregator).** The aggregator is well-trodden
ground (used in production by Axiom, Scroll, Taiko, …), reduces the inner
circuit's K to a smaller domain for the on-chain verifier, gives us a clean
upgrade path (2a → 2b later if/when gas matters), and reuses code we already
have working in `deposit-prover/src/aggregation.rs`.

## Pipeline (target architecture)

```text
Partner's bridge-event-prove circuit (Circuit 4, single-final-root)
   |
   |  generate_circuit4_proof (orchestrator)
   |    halo2_proofs::create_proof  with  PoseidonTranscript  (NOT Blake2b)
   v
SHPLONK proof + 10 public inputs (BN254 Fr)
   |
   |  snark-verifier-sdk::AggregationCircuit::new::<SHPLONK>(...)
   v
Aggregation Halo2 SNARK
   |  - inner public inputs (10 PIs) are exposed as outer instances
   |  - outer instances also contain KZG accumulator (4 limbs)
   |  - Fiat-Shamir transcript inside the aggregator is Poseidon (snark-verifier default)
   v
snark-verifier-sdk::evm::gen_evm_verifier_shplonk
   |
   v
Yul source (Solidity-compatible) — single huge function `verifyProof(bytes, uint256[])`
   |
   v
contracts/ethereum/src/BridgeWithdrawalAggregatorVerifier.sol
   |
   |  called by AckiNackiBridge.withdrawByProof
   v
On-chain success ⇒ proof of Circuit 4 valid ⇒ payout authorised
```

## Constraints / risks

### C-1. Transcript compatibility (BLOCKER → mitigation in M3)

`bridge-prover-orchestrator/src/prover.rs:100` (and partner's
`bridge-prover-lib::prover`) hardcodes **Blake2b** Fiat-Shamir transcript:

```rust
let mut transcript = Blake2bWrite::<_, G1Affine, Challenge255<_>>::init(vec![]);
```

`snark-verifier-sdk`'s `gen_snark_shplonk` and its in-circuit verifier use
**Poseidon** transcript by default. There is no Blake2b loader inside
`snark-verifier`; writing one would require:
  1. A Blake2b chip inside halo2-axiom (the partner already has a SHA-256
     chip, so this is plausible but ≥4 weeks of work).
  2. A `Loader::<Blake2bTranscript>` impl for the in-circuit `Transcript` trait.

**Decision:** switch the Circuit 4 prover to **Poseidon** transcript for the
ETH-side aggregator path. Native AN-side `ZKHALO2VERIFYWITHVK` is on the
deposit direction (Circuit 1B), not withdraw (Circuit 4), so the Blake2b
requirement does not affect withdraw.

### C-2. halo2-lib fork compatibility

`snark-verifier-sdk` builds against `axiom-crypto/halo2-lib` (`halo2-pse` or
`halo2-axiom` feature). The partner's circuits build against
`gosh/halo2-lib-zkevm-sha256-and-bls12-381` (snapshot fork of axiom's).

These two cannot coexist in one cargo unit (duplicate-symbol conflicts at
link time). The aggregator therefore lives in a **separate cargo workspace**
(like `deposit-prover/` already does). The interface between orchestrator
(partner-fork) and aggregator (axiom-fork) is a binary file: serialised
SHPLONK proof + serialised verifying key + bincode-encoded instances.

`snark-verifier`'s `compile()` step requires the full halo2 verifying key, so
the aggregator crate must be able to **read** the partner's VK. This is
possible because both forks share the same on-disk VK serialization format
(`SerdeFormat::RawBytes` over `bn254::G1Affine`). The aggregator instantiates
the inner protocol from the deserialised VK without ever touching partner's
circuit Rust code.

### C-3. Aggregator K and verifier size

`gen_evm_verifier_shplonk` for an `AggregationCircuit` at `k=21` produces a
verifier of ~10–15 KB (deposit-prover empirical numbers, see
`deposit-prover/examples/generate_aggregation_verifier.rs`). EVM contract
size limit is 24 KB (EIP-170), so we should fit. If not, we increase `k` or
add a Groth16 wrapper (option 2b).

### C-4. Circuit 4 not yet in orchestrator (BLOCKED on partner)

Partner is finalising the single-final-root variant on
`gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits#circuit4-single-final-root`.
Until that lands, we cannot wire up Circuit 4 directly. **M2 feasibility
spike therefore targets Circuit 1B as a stand-in** — it exercises the same
pipeline (partner halo2 fork → SHPLONK proof → snark-verifier aggregation
→ Yul) so the only thing that changes when Circuit 4 arrives is the inner
verifying key, the instance count, and the prover entry point.

## Milestones (M1 → M8)

### M1. Roadmap (this doc)

**Status:** in progress.
**Deliverable:** `docs/r15_snark_verifier_roadmap.md` describing decision,
constraints, milestone breakdown, owner sign-off path.

### M2. Feasibility spike

**Goal:** prove that, in our build environment, we can take a Halo2 SHPLONK
proof, aggregate it via `snark-verifier-sdk`, and emit a Yul Solidity
verifier that fits under EIP-170.

**Status (2026-05-27): ✅ Closed (Rust-only round-trip). Foundry on-chain
harness deferred to M6/M7.**

**Delivered:** `crates/bridge-evm-aggregator/` — new standalone cargo
workspace (excluded from the main workspace, like `deposit-prover/`):
  - `Cargo.toml` pinning `snark-verifier-sdk v0.1.7-git` against
    `axiom-crypto/halo2-lib v0.4.1-git`,
    `privacy-scaling-explorations/halo2 v2023_04_20`,
    `halo2curves 0.3.1`.
  - `src/multiply.rs` — minimal inner circuit: `a * b == c` with `c` as
    single public input (K=9; built via `BaseCircuitBuilder<Fr>`).
  - `src/aggregator.rs` — three thin wrappers: `prove_inner` (SHPLONK
    proof of `multiply`), `aggregate` (wraps in `AggregationCircuit` at
    K=21, SHPLONK, `VerifierUniversality::Full`), `generate_yul_verifier`
    (`gen_evm_verifier_shplonk` → Yul .sol + raw .bin bytecode).
  - `tests/round_trip.rs` — `#[ignore]`-gated acceptance test, ~3 min
    wall-clock release build.
  - `README.md` — status table, build/run instructions, deviation from
    the Circuit-1B-as-stand-in plan.

**Empirical results captured:**
  - Inner SHPLONK proof generated cleanly (K=9, Poseidon transcript).
  - Aggregation succeeds at **K=21** (K=20 hits "NOT ENOUGH ADVICE COLUMNS"
    once expose_previous_instances is in play).
  - Default aggregator instance shape = **12 limbs of KZG accumulator**
    (`NUM_ACCUMULATOR_INSTANCES = 4 G1 coord × 3 native limbs`); the
    earlier "4-limb" comment in `deposit-prover/src/aggregation.rs` is
    wrong — corrected here.
  - Yul source: **1 192 lines / 56 055 bytes**, compiles cleanly under
    Foundry (`solc 0.8.19`, `via_ir = true`).
  - Raw deployment bytecode: **13 009 bytes (12.7 KB)** — **53 %** of the
    EIP-170 24 576-byte runtime limit. Headroom comfortable.

**Deviation from original M2 plan:** the roadmap proposed aggregating
Circuit 1B (partner-fork) as the inner. That requires two non-trivial
spikes simultaneously: (a) snark-verifier in our env, (b) cross-fork
VK/proof compatibility. We split them — M2 (this milestone) validates
(a) using a synthetic inner; M5 will validate (b) when wiring Circuit 4.
The Circuit-1B-stand-in idea is **abandoned** because Circuit 1B is the
DEPOSIT direction (uses AN-side `ZKHALO2VERIFYWITHVK`, not the
ETH-side aggregator) — Circuit 4 (the withdraw direction) is the actual
target.

**Known deferred items (now scoped to M5):**
  - ~~`expose_previous_instances(false)` to surface inner PIs as outer
    instances. Currently fails with `NOT ENOUGH ADVICE COLUMNS` at K=20/21
    with default `AggregationConfigParams.num_advice` — needs explicit
    `num_advice_per_phase` tuning.~~ **RESOLVED 2026-05-29** — see M5
    progress note below. The `NOT ENOUGH ADVICE COLUMNS` was an *ordering*
    bug, not a column-budget one: `expose_previous_instances` must be called
    **before** `calculate_params` (and on both keygen + prover circuits) so
    the auto-tuner accounts for the extra instance copy constraints. No
    explicit `num_advice_per_phase` override is required at K=21.
  - On-chain harness — deferred to M6/M7 because Foundry's solc + via_ir +
    `optimizer_runs = 1` strips the inline-assembly fallback (same issue
    the legacy `Halo2Verifier.sol` has — its bytecode in `out/` is also
    a 67-byte stub, and the established workaround is `vm.readFileBinary`
    on a `.bin` file built externally). M6 lifts this approach to the
    aggregator's verifier.

### M3. Transcript strategy ✅ (orchestrator side; 2026-05-27)

**Outcome:** the orchestrator can now emit Circuit-1B proofs under either
Blake2b (AN-side default) or Poseidon (ETH-side aggregator) Fiat–Shamir.
The Poseidon implementation is vendored — see "Vendoring decision" below
for why we did not depend on `snark-verifier` directly.

**Deliverables (committed):**

  - `crates/bridge-prover-orchestrator/src/poseidon_transcript.rs` — native
    `Poseidon<Fr, Fr, T=3, RATE=2>` permutation + `PoseidonRead<R>` /
    `PoseidonWrite<W>` implementing
    `halo2_proofs::transcript::{Transcript, TranscriptRead/Write,
    TranscriptReadBuffer/WriterBuffer}`. Spec constants
    `(T, RATE, R_F, R_P, SECURE_MDS) = (3, 2, 8, 57, 0)` match
    `snark-verifier-sdk/src/halo2.rs` v0.1.7-git lines 54–58 so the
    Aggregator's in-circuit verifier (M5) will derive the same challenges.
  - `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs` — extended
    `TranscriptKind` with `Poseidon = 2`. `VkBlob` decoders still bail on
    anything ≠ Blake2b (AN-side opcode is unchanged).
  - `crates/bridge-prover-orchestrator/src/{prover,verifier}.rs` — new
    `generate_fallback_proof_with_transcript(..., TranscriptKind)` and
    `verify_fallback_proof_with_transcript(..., TranscriptKind)` variants;
    the legacy `generate_fallback_proof` / `verify_fallback_proof` now
    delegate to them with `TranscriptKind::Blake2b`.
  - `crates/bridge-prover-orchestrator/src/poseidon_transcript.rs::tests`
    — five unit tests covering deterministic squeezes, scalar
    round-trip, EC point round-trip, tampered-byte detection, spec
    determinism (all green; sub-second).
  - `crates/bridge-prover-orchestrator/tests/fallback_round_trip.rs`
    — heavy integration test extended to prove + verify with BOTH
    transcripts, asserting (1) Poseidon proof verifies under
    Poseidon-Read, (2) mixing transcripts rejects, (3) the two proof byte
    streams differ.

**Vendoring decision — why we re-implemented Poseidon rather than depending
on `snark-verifier`:**

`snark-verifier` declares `halo2-base` (axiom-crypto fork) as a
*non-optional* `[dependencies]` entry — there is no feature flag that
turns it into a pure proofs-only crate. The orchestrator pins
`halo2-base` to the **gosh** fork (so it can compile the partner's
circuits, which embed `gosh-halo2-crypto-lib` chips). Pulling in
`axiom-crypto/halo2-lib` alongside causes a duplicate-symbol conflict
that Cargo refuses to resolve.

So we copied the ~80 lines of native permutation logic from
`snark-verifier/src/util/hash/poseidon.rs` into
`poseidon_transcript.rs`, parameterised over `OptimizedPoseidonSpec`
(which IS available in gosh `halo2-base` under the same module path —
both forks took it from PSE upstream verbatim). The spec generation is
deterministic (the round constants and MDS are computed from
`T`/`RATE`/`R_F`/`R_P`/`SECURE_MDS` and the field modulus, no entropy),
so the gosh-fork build of the spec is byte-identical to the
axiom-fork build.

**Verification of byte-for-byte compatibility with `snark-verifier-sdk`
is deferred to M5** — that's where the orchestrator's Poseidon proof
will actually be fed into an `AggregationCircuit` for the first time. If
the in-circuit verifier accepts our proof, the transcripts agree. If
not, M5 will surface the discrepancy and we'll add a golden-vector
cross-test against `snark-verifier-sdk::PoseidonTranscript<NativeLoader,
_>`.

**What was deliberately deferred:**

  - M3.c (one proof, two transcripts simultaneously) — superseded by the
    cleaner two-flavour model. Each consumer (AN opcode vs. ETH
    aggregator) gets its own proof; the prover takes a transcript
    discriminator at the API boundary.
  - Cross-implementation golden test against `snark-verifier-sdk` — see
    above; the aggregator round-trip in M5 IS the golden test.

### M4. Circuit 4 prover in orchestrator (depends on partner)

When the partner's `circuit4-single-final-root` branch lands a stable
verifying key + prover entry point, add `bridge-prover-orchestrator::
generate_circuit4_proof(...)` that mirrors `generate_fallback_proof` but
calls `BridgeEventProveCircuit::new(...)` with Poseidon transcript.

### M5. Circuit 4 aggregator

In the `crates/bridge-evm-aggregator/` crate (created in M2), add the
real Circuit 4 path:
  - Read Circuit 4 VK + proof.
  - Construct `AggregationCircuit` over it.
  - Produce aggregated SNARK.
  - Acceptance: `aggregator_round_trip` test passes locally.

**Progress (2026-05-29) — instance exposure de-risked on the synthetic
inner (partner-independent):**

The single hard technical risk in M5 was getting the aggregator to
**re-expose the inner circuit's public inputs** as outer instances (without
it, the on-chain verifier only commits to the KZG accumulator and the bridge
contract cannot read `tokenId`/`amount`/`recipient`/`nullifier`/`finalRoot`).
This is now implemented and validated against the synthetic `multiply` inner,
so the only thing left for M5 when M4 lands is a **data swap** (real Circuit 4
VK + proof + 10 PIs instead of 1).

  - `aggregator::aggregate` and `generate_yul_verifier` now call
    `AggregationCircuit::expose_previous_instances(false)` on **both** the
    keygen and prover circuits, **before** `calculate_params`. The earlier
    `NOT ENOUGH ADVICE COLUMNS` failure was an ordering bug, not a
    column-budget one — calling it before `calculate_params` lets the
    auto-tuner size `num_advice` for the added instance copy constraints. No
    hand-pinned `num_advice_per_phase` needed at K=21.
  - `generate_yul_verifier` derives `num_instance` from the circuit
    (`CircuitExt::num_instance`) instead of the hardcoded accumulator-only
    count, so the Yul calldata layout always matches the proof.
  - `tests/round_trip.rs` now asserts the aggregator instance column is
    `[acc(12) ‖ inner_pi]` (len 13) and that `instances[0][12] == 77`
    (the inner `a*b`).
  - **Empirical (2026-05-29, nightly + `solc 0.8.19`):** aggregation at
    K=21 succeeds with the exposed instance; Yul verifier deployment
    bytecode = **13 172 bytes (12.9 KB)**, 54 % of the EIP-170 limit (was
    13 009 B with the accumulator-only 12-instance layout). Round-trip
    test green in ~115 s on a warm SRS cache.

Remaining M5 (blocked on M4): swap `multiply` for the partner's Circuit 4
prover, re-confirm K=21 fits the larger inner VK + 10 PIs (bump K if not),
and pin a production SRS in place of the deterministic test SRS.

### M6. Yul EVM verifier

Run `gen_evm_verifier_shplonk` on the M5 aggregator, save the Yul under
`contracts/ethereum/src/BridgeWithdrawalAggregatorVerifier.sol`,
`forge build` succeeds, and `verifier_size <= 24 576 bytes`.

### M7. End-to-end Foundry test

Add `contracts/ethereum/test/AckiNackiBridgeWithdrawByProofE2E.t.sol`:
  - Bootstraps a fork with the aggregator Yul deployed.
  - Calls `AckiNackiBridge.withdrawByProof` with a real aggregated proof.
  - Asserts on-chain success, balance change, nullifier consumed,
    `_knownAnchors[finalRoot] == true`.

This is the deliverable that **actually closes R15**.

### M8. Cleanup

  - Remove `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/`
    (the identity stub) and replace any references to
    `IBridgeWithdrawalGroth16Verifier` with the new Yul aggregator.
  - Move circuit-1a/1b/2 wrappers to similar aggregator-based path if/when
    their on-chain consumers are wired up. (Currently only circuit-4 has
    an on-chain consumer; 1a/1b/2 are AN-side only via
    `ZKHALO2VERIFYWITHVK`, so they don't strictly need an aggregator.)
  - Update `AGENTS.md`, `docs/an_partner_integration_plan.md` Decision
    Log, audit reports, partner pack.

## Estimated effort

| Milestone | Wall-clock (one engineer) | Risk |
|---|---|---|
| M1 — Roadmap | 0.5 day | none |
| M2 — Feasibility spike | 3–5 days | medium (dep conflicts) |
| M3 — Transcript strategy | done (2 days) | n/a |
| M4 — Circuit 4 prover | 1–2 days **+ partner blocker** | low once unblocked |
| M5 — Circuit 4 aggregator | 5–8 days | medium |
| M6 — Yul verifier | 1–2 days | low |
| M7 — Foundry E2E | 2–3 days | low |
| M8 — Cleanup | 1 day | none |
| **Total** | **15–25 working days** | medium overall |

Plus partner-side blocker for M4 (Circuit 4 finalised + verifying key
published). Realistically: **4–6 weeks** of focused work once M4 unblocks.

## Out of scope (deferred)

- Groth16 wrapper around the aggregator (option 2b). To be revisited if
  the Yul aggregator gas cost (M7) exceeds ~1 M and that becomes
  business-critical.
- Aggregating multiple circuits in one proof (Circuit 4 + Circuit 1B +
  Circuit 2 + Circuit 3 in one big AggregationCircuit). Possible later but
  not needed for v1.
- Blake2b transcript loader in `snark-verifier`. Required only if we
  decide we cannot tolerate maintaining a Poseidon-transcript fork of
  the Circuit 4 prover.
