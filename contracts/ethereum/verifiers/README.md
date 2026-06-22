# R15 SHPLONK verifier bytecode (production)

Deploy scripts (`DeployRealBridge`, `DeployShellnetE2EBridge`) load these artefacts — no identity-stub Groth16 for 1A/1B/2/C4.

| File | Circuit | Inner PIs | Status (2026-06-22) |
|------|---------|-----------|---------------------|
| `PrimaryAggregatorVerifier.bin` | 1A primary attestation | 4 | **21 493 B** — EIP-170 OK |
| `FallbackAggregatorVerifier.bin` | 1B fallback attestation | 4 | **21 493 B** — EIP-170 OK (K=21 inner) |
| `LayerHashesAggregatorVerifier.bin` | 2 layer hashes | 14 | **19 100 B** — EIP-170 OK |
| `BridgeWithdrawalAggregatorVerifier.bin` | 4 withdrawal | 10 | TBD (M4) |

All three `verifyBlock` circuits use the SHPLONK aggregator path. Circuit **1B** is keygen'd at
inner `K=21` (vs `K=20` for primary/layer): the fallback circuit verifies two attestation
envelopes, so at `K=20` it needs 44 advice columns and the aggregator Yul exceeds EIP-170
(~28 KB). At `K=21` it auto-configures to 22 advice columns and the Yul drops to 21 493 B. The
gnark Groth16 fallback hybrid is retired. See `docs/r15_verifier_sizing_report.md`.

## Generate SHPLONK `.bin` (1A + 1B + 2)

```bash
cd crates/bridge-prover-orchestrator
cargo run --release --bin export-bound-block-proofs -- \
  --params-dir ../../params --out-dir ../../proofs/bound
cargo run --release --bin export-bound-poseidon-snarks -- \
  --params-dir ../../params --bound-dir ../../proofs/bound \
  --snark-dir ../../proofs/bound/poseidon-snark

cd ../bridge-evm-aggregator
for c in primary:PrimaryAggregatorVerifier fallback:FallbackAggregatorVerifier layer_hashes:LayerHashesAggregatorVerifier; do
  snark="${c%%:*}"; name="${c##*:}"
  cargo run --release --bin export-inner-aggregator -- \
    --inner-snark ../../proofs/bound/poseidon-snark/${snark}.snark \
    --out-dir ../../contracts/ethereum/verifiers \
    --name ${name}
done
```

Or run the whole pipeline on n14: `./scripts/n14_r15_proving_run.sh continue-c && ./scripts/n14_r15_proving_run.sh pull-artifacts`.

```bash
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

Override SHPLONK paths via env: `SHPLONK_BIN_PRIMARY`, `SHPLONK_BIN_FALLBACK`, `SHPLONK_BIN_LAYER_HASHES`, `SHPLONK_BIN_WITHDRAWAL`.

M2 multiply spike fixtures live under `test/fixtures/r15_spike/` for Foundry only — **not** valid production verifiers.
