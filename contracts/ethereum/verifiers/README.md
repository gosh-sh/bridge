# R15 SHPLONK verifier bytecode (production)

Deploy scripts (`DeployRealBridge`, `DeployShellnetE2EBridge`) load these artefacts — no identity-stub Groth16 for 1A/2/C4.

| File | Circuit | Inner PIs | Status (2026-06-22) |
|------|---------|-----------|---------------------|
| `PrimaryAggregatorVerifier.bin` | 1A primary attestation | 4 | **21 493 B** — EIP-170 OK |
| `LayerHashesAggregatorVerifier.bin` | 2 layer hashes | 14 | **19 100 B** — EIP-170 OK |
| `BridgeWithdrawalAggregatorVerifier.bin` | 4 withdrawal | 10 | TBD (M4) |
| `FallbackGroth16VerifierGenerated.sol` | 1B fallback attestation | 4 | **~7 KB runtime** — gnark Groth16 (hybrid deploy) |

Circuit **1B fallback** does not use a `.bin` file: the SHPLONK aggregator Yul exceeds EIP-170 (~28 KB). Production deploys `FallbackVerifier` + checked-in `FallbackGroth16VerifierGenerated.sol` instead.

## Generate SHPLONK `.bin` (1A + 2)

```bash
cd crates/bridge-prover-orchestrator
cargo run --release --bin export-bound-block-proofs -- \
  --params-dir ../../params --out-dir ../../proofs/bound
cargo run --release --bin export-bound-poseidon-snarks -- \
  --params-dir ../../params --bound-dir ../../proofs/bound \
  --snark-dir ../../proofs/bound/poseidon-snark

cd ../bridge-evm-aggregator
cargo run --release --bin export-inner-aggregator -- \
  --inner-snark ../../proofs/bound/poseidon-snark/primary.snark \
  --out-dir ../../contracts/ethereum/verifiers \
  --name PrimaryAggregatorVerifier
cargo run --release --bin export-inner-aggregator -- \
  --inner-snark ../../proofs/bound/poseidon-snark/layer_hashes.snark \
  --out-dir ../../contracts/ethereum/verifiers \
  --name LayerHashesAggregatorVerifier
```

## Generate Groth16 fallback (1B)

```bash
./scripts/install_fallback_groth16_verifier.sh \
  crates/bridge-prover-orchestrator/proofs/bound/fallback/halo2_proof.json
```

```bash
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

Override SHPLONK paths via env: `SHPLONK_BIN_PRIMARY`, `SHPLONK_BIN_LAYER_HASHES`, `SHPLONK_BIN_WITHDRAWAL`.

M2 multiply spike fixtures live under `test/fixtures/r15_spike/` for Foundry only — **not** valid production verifiers.
