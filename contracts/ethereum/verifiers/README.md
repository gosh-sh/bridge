# R15 SHPLONK verifier bytecode (production)

Deploy scripts (`DeployRealBridge`, `DeployShellnetE2EBridge`) load **only** these artefacts — no identity-stub Groth16, no mocks.

| File | Circuit | Inner PIs |
|------|---------|-----------|
| `PrimaryAggregatorVerifier.bin` | 1A primary attestation | 4 |
| `FallbackAggregatorVerifier.bin` | 1B fallback attestation | 4 |
| `LayerHashesAggregatorVerifier.bin` | 2 layer hashes | 14 |
| `BridgeWithdrawalAggregatorVerifier.bin` | 4 withdrawal | 10 |

Generate with `bridge-evm-aggregator` after Poseidon inner proofs are exported:

```bash
cd crates/bridge-evm-aggregator
cargo run --release --bin export-inner-aggregator -- \
  --inner-snark /path/to/primary_poseidon.snark \
  --out-dir ../../contracts/ethereum/verifiers \
  --name PrimaryAggregatorVerifier \
  --inner-instances 4
```

Inner instance counts: 1A=4, 1B=4, 2=14, C4=10.

```bash
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

Override paths via env: `SHPLONK_BIN_PRIMARY`, `SHPLONK_BIN_FALLBACK`, `SHPLONK_BIN_LAYER_HASHES`, `SHPLONK_BIN_WITHDRAWAL`.

M2 multiply spike fixtures live under `test/fixtures/r15_spike/` for Foundry only — **not** valid production verifiers.
