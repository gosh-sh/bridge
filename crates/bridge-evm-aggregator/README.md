# `bridge-evm-aggregator` — R15 / M2 feasibility spike

This is the **M2** milestone of the R15 roadmap
(`docs/r15_snark_verifier_roadmap.md`). It stands up the
snark-verifier-sdk → Yul EVM verifier pipeline in our build environment
and confirms it produces an EIP-170-fitting Solidity verifier.

## Status (as of 2026-05-27, M2)

| Step | Status | Evidence |
|---|---|---|
| Inner Halo2 SHPLONK proof (K=9) of `a * b == c` | ✅ | `cargo test --release --test round_trip -- --ignored --nocapture` |
| Wrap in `snark_verifier_sdk::AggregationCircuit` (K=21, SHPLONK, Universality::Full) | ✅ | round-trip test passes |
| Aggregator instance shape = `[acc_0..acc_11]` (12 KZG accumulator limbs) | ✅ | asserted in round-trip test |
| Emit Yul EVM verifier source + raw deployment bytecode | ✅ | `target/spike/AggregatorVerifierSpike.{sol,bin}` |
| Bytecode under EIP-170 24 576 B runtime limit | ✅ | **13 009 bytes (12.7 KB)** |
| Solidity source compiles in our Foundry profile (`solc 0.8.19`, `via_ir = true`) | ✅ | `forge build` was clean (validated 2026-05-27; stub artifact then removed) |
| Re-expose inner SNARK public inputs as aggregator instances | ⏸ deferred | `expose_previous_instances(false)` triggers `NOT ENOUGH ADVICE COLUMNS` at K=20/21 with default `num_advice` — needs custom `AggregationConfigParams` tuning. Will be done when wiring real Circuit 4 (M5). |
| Foundry on-chain harness (deploy bytecode, call fallback with `instances ‖ proof`) | ⏸ M6/M7 | The legacy `Halo2Verifier.sol` in this repo has the same Solidity-optimizer issue (Solc + `via_ir = true` + `optimizer_runs = 1` strips inline-assembly fallback down to a 67-byte stub); the established workaround is to deploy from `test/halo2_verifier_bytecode.bin` via `vm.readFileBinary`. Same approach will work here. |

## What this crate *is not*

It is **not** the real on-chain Circuit 4 verifier yet. The inner circuit is
a one-line multiplication gate (`crate::multiply`) — it exists purely to
produce a SHPLONK proof of any shape so the aggregator pipeline can be
exercised end-to-end.

When the partner's Circuit 4 lands (M4 in
`docs/r15_snark_verifier_roadmap.md`), the trivial multiply gate is replaced
with the real Circuit 4 prover, the aggregator's K may bump (M5 sizing),
and the Yul output becomes the production
`BridgeWithdrawalAggregatorVerifier.sol` (M6) that the bridge contract
calls (M7).

## Layout

```
src/
├── lib.rs            ← module declarations + crate docs
├── multiply.rs       ← BaseCircuitBuilder<Fr> circuit proving a*b == c
└── aggregator.rs     ← prove_inner, aggregate, generate_yul_verifier
tests/
└── round_trip.rs     ← #[ignore]-gated M2 acceptance test (~3 min wall-clock)
```

## Build

```bash
cd crates/bridge-evm-aggregator
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo +nightly check --tests
```

`CARGO_NET_GIT_FETCH_WITH_CLI=true` is needed to fetch the
`axiom-crypto/snark-verifier`, `axiom-crypto/halo2-lib`,
`privacy-scaling-explorations/halo2{,curves}` git deps. The crate is
deliberately a **separate cargo workspace** (note the empty `[workspace]`
section in `Cargo.toml` — same pattern as `deposit-prover/`) because it
depends on `axiom-crypto/halo2-lib`, which clashes with the
gosh-fork (`halo2-lib-zkevm-sha256-and-bls12-381`) used by
`crates/bridge-prover-orchestrator/`.

## Run M2 acceptance test

```bash
cd crates/bridge-evm-aggregator
cargo +nightly test --release --test round_trip -- --ignored --nocapture
```

Wall-clock ~3 minutes (release profile, M=8 logical cores). Outputs land
under `target/spike/`:

- `AggregatorVerifierSpike.sol` — 1 192-line Yul-style Solidity verifier.
- `AggregatorVerifierSpike.bin` — raw deployable bytecode, 13 009 bytes.
- `params/kzg_bn254_{9,21}.srs` — KZG params (deterministic test SRS;
  Hermez Perpetual Powers of Tau will replace at M5).

These are all `.gitignore`d (see root `.gitignore` `# R15 / M2` section).

## Why the spike's inner circuit is `a * b = c`

Two reasons:

1. **Independence from the partner.** Circuit 4 is still on
   `gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits#circuit4-single-final-root`
   (M4 blocker). The spike must NOT block on that work — its job is to
   validate the pipeline in our build env.
2. **Minimum surface area.** A single multiplication gate keeps inner-K
   small (9), keygen seconds, total wall-clock ~3 min, and makes the
   spike obvious-by-inspection (it's literally `Fr::from(7) * Fr::from(11) == 77`).

## Pointers to next steps

- M3 — switch the partner's prover in `bridge-prover-orchestrator` from
  Blake2b to Poseidon transcript, so aggregator can in-circuit verify.
- M4 — partner finishes Circuit 4, we wire it into orchestrator.
- M5 — replace `multiply::build_multiply_circuit` with Circuit 4's prover;
  retune aggregator K + `num_advice_per_phase`; enable
  `expose_previous_instances(false)` so the bridge can read Circuit 4's
  10 PIs.
- M6 — move the generated Yul/bin into `contracts/ethereum/src/` as
  `BridgeWithdrawalAggregatorVerifier.{sol,bin}` and wire
  `BridgeWithdrawalVerifier.sol` (the adapter) to call it.
- M7 — Foundry E2E test: deploy bytecode, call `verifyProof(proof, pub)`
  with real aggregated proof for a Circuit 4 burn, assert balance change
  and nullifier consumption.
