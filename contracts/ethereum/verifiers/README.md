# R15 SHPLONK verifier bytecode (production)

Deploy scripts (`DeployRealBridge`, `DeployShellnetE2EBridge`) load **only** these artefacts — no identity-stub Groth16, no mocks.

| File | Circuit | Inner PIs | Status (2026-06-22) |
|------|---------|-----------|---------------------|
| `PrimaryAggregatorVerifier.bin` | 1A primary attestation | 4 | **21 493 B** — EIP-170 OK |
| `FallbackAggregatorVerifier.bin` | 1B fallback attestation | 4 | **BLOCKED** — ~28.4 KB (see `docs/r15_verifier_sizing_report.md`) |
| `LayerHashesAggregatorVerifier.bin` | 2 layer hashes | 14 | **19 100 B** — EIP-170 OK |
| `BridgeWithdrawalAggregatorVerifier.bin` | 4 withdrawal | 10 | TBD (M4) |

Generate with `bridge-evm-aggregator` after Poseidon inner proofs are exported from the orchestrator:

```bash
cd crates/bridge-prover-orchestrator
cargo run --release --bin export-bound-poseidon-snarks -- \
  --params-dir ../../params --bound-dir ../../proofs/bound \
  --snark-dir ../../proofs/bound/poseidon-snark

cd ../bridge-evm-aggregator
cargo run --release --bin export-inner-aggregator -- \
  --inner-snark ../../proofs/bound/poseidon-snark/primary.snark \
  --out-dir ../../contracts/ethereum/verifiers \
  --name PrimaryAggregatorVerifier
```

`AggregatorConfig::for_verifier_name` picks `K_outer` and universality per circuit.

```bash
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

Override paths via env: `SHPLONK_BIN_PRIMARY`, `SHPLONK_BIN_FALLBACK`, `SHPLONK_BIN_LAYER_HASHES`, `SHPLONK_BIN_WITHDRAWAL`.

M2 multiply spike fixtures live under `test/fixtures/r15_spike/` for Foundry only — **not** valid production verifiers.
