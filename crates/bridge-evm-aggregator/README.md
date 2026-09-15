# `bridge-evm-aggregator` — SHPLONK aggregator pipeline (started as the R15 / M2 spike)

*Reviewed against the sources at commit `a69ba36`, 2026-08-18.*

This crate began as the **M2** feasibility spike: stand up the snark-verifier-sdk → Yul EVM verifier
pipeline in our build environment and confirm it produces an EIP-170-fitting verifier.

**It is no longer only a spike.** Its `export-inner-aggregator` bin (`Cargo.toml:57`) is what
produces all four production verifier artefacts under `contracts/ethereum/verifiers/` — see that
directory's README for the exact invocations. The spike parts described below (the multiply gate,
`export-spike-artifacts`) still exist alongside it.

## The runtime self-check needs no compiler

`aggregate-proof` — the per-proof bin the withdrawal CLI and the relayer shell out to — regenerates
the outer verifier's Solidity source from the aggregator key and refuses to emit calldata unless it
is byte-identical to the committed `contracts/ethereum/verifiers/<name>.sol`. It compiles nothing,
so the hosts that run it need no `solc`. `--allow-source-drift` turns the refusal into a warning and
exists only to bootstrap a verifier whose source is not committed yet.

Bytecode is produced only when a verifier is regenerated: `export-inner-aggregator` writes the
`.sol` and the `.bin` compiled from it and applies the EIP-170 gate, and that path, like
`export-spike-artifacts` and the round-trip test, needs `solc 0.8.19` on `PATH`. That each committed
`.sol` compiles to its `.bin` is checked by `scripts/check_verifier_sources.sh` in CI.

A mismatch reads `aggregator VK drift` and names the first differing line. It means either a
different key or a `snark-verifier` upgrade that changed the generated text; after an upgrade,
regenerate both files of every pair.

## Status (M2 closed 2026-05-27; M5 instance-exposure de-risked 2026-05-29)

| Step | Status | Evidence |
|---|---|---|
| Inner Halo2 SHPLONK proof (K=9) of `a * b == c` | ✅ | `cargo test --release --test round_trip -- --ignored --nocapture` |
| Wrap in `snark_verifier_sdk::AggregationCircuit` (K=21, SHPLONK, Universality::Full) | ✅ | round-trip test passes |
| **Re-expose inner SNARK public inputs as aggregator instances** | ✅ 2026-05-29 | `expose_previous_instances(false)` on **both** keygen + prover circuits, called **before** `calculate_params` so the auto-tuner sizes `num_advice` for the added copy constraints (this ordering is what previously misfired as `NOT ENOUGH ADVICE COLUMNS`). No hand-pinned `num_advice` needed at K=21. |
| Aggregator instance shape = `[acc_0..acc_11, inner_pi_0..]` (12 KZG accumulator limbs **+ re-exposed inner PIs**) | ✅ | round-trip asserts `len == 12 + 1` and `instances[0][12] == 77` (the inner `a*b`) |
| Emit Yul EVM verifier source + raw deployment bytecode | ✅ | `target/spike/AggregatorVerifierSpike.{sol,bin}` |
| Bytecode under EIP-170 24 576 B runtime limit | ✅ | **13 172 bytes (12.9 KB)** with 13 instances (was 13 009 B at 12 instances) |
| Solidity source compiles with `solc 0.8.19` (exact pragma pin emitted by snark-verifier) | ✅ | `compile_solidity` (`solc --bin -`) succeeds; validated 2026-05-29 |
| Foundry on-chain harness (deploy bytecode, call fallback with `instances ‖ proof`) | ⏸ M6/M7 | Needs Foundry + a persisted EVM proof/instances vector. Deploy from `AggregatorVerifierSpike.bin` via `vm.readFileBinary` (same workaround the legacy `Halo2Verifier.sol` uses to dodge the solc-optimizer inline-assembly stub). |

## What the spike part *is not*

The **spike** inner circuit is a one-line multiplication gate (`crate::multiply`) — it exists purely
to produce a SHPLONK proof of some shape so the aggregator pipeline can be exercised end to end. Its
output, `target/spike/AggregatorVerifierSpike.{sol,bin}`, is **not** a production verifier; the
Foundry fixtures derived from it live under `contracts/ethereum/test/fixtures/r15_spike/` and are
test-only.

**That future has since arrived.** Circuit 4 landed: the inner event snark is produced by
`export-c4-poseidon-snark` in `bridge-snark-utils`, aggregated by this crate's
`export-inner-aggregator`, and the result is committed as
`contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin` (20 990 B, inner `K=19`), which
`AckiNackiBridge.withdrawByProof` calls through its adapter. The same pipeline produces the 1A, 1B
and Circuit-2 verifiers. So the milestone text below (M4 → M7) is a historical record of a plan that
has since been executed, not a description of pending work.

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
`crates/bridge-snark-utils/`.

## Run M2 acceptance test

```bash
cd crates/bridge-evm-aggregator
cargo +nightly test --release --test round_trip -- --ignored --nocapture
```

Wall-clock ~3 minutes (release profile, M=8 logical cores). Outputs land
under `target/spike/`:

- `AggregatorVerifierSpike.sol` — Yul-style Solidity verifier (~56.7 KB source).
- `AggregatorVerifierSpike.bin` — raw deployable bytecode, 13 172 bytes
  (12 accumulator limbs + 1 re-exposed inner PI).
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

- M3 — switch the partner's prover in `bridge-snark-utils` from
  Blake2b to Poseidon transcript, so aggregator can in-circuit verify.
- M4 — partner finishes Circuit 4, we wire it into orchestrator.
- M5 — `expose_previous_instances(false)` is **done and validated on the
  synthetic inner** (2026-05-29): the aggregator now surfaces the inner PIs
  after the 12 accumulator limbs. The remaining M5 work is purely a data
  swap — replace `multiply::build_multiply_circuit` with Circuit 4's prover
  (10 PIs) once M4 lands; re-confirm K=21 still fits (bump if not).
- M6 — move the generated Yul/bin into `contracts/ethereum/src/` as
  `BridgeWithdrawalAggregatorVerifier.{sol,bin}` and wire
  `BridgeWithdrawalVerifier.sol` (the adapter) to call it.
- M7 — Foundry E2E test: deploy bytecode, call `verifyProof(proof, pub)`
  with real aggregated proof for a Circuit 4 burn, assert balance change
  and nullifier consumption.
