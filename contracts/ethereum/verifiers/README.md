# R15 SHPLONK verifier bytecode (production)

Deploy scripts (`DeployRealBridge`, `DeployShellnetE2EBridge`) load these artefacts. 

Sizes below were measured with `wc -c` on the committed `.bin` files on **2026-09-13**, the same
metric `scripts/check_eip170_verifier_bins.sh` uses. Every figure a table here has carried has at
some point drifted from the artefacts (Primary 21 493 → 21 494 and Withdrawal 20 987 → 20 990 when
they were regenerated on 2026-08-13; Layer hashes 19 100 → 23 111 in this change, see below), so
re-measure rather than trusting the row.

| File | Circuit | Inner PIs | Size | EIP-170 (24 576 B) |
|------|---------|-----------|------|--------------------|
| `PrimaryAggregatorVerifier.bin` | 1A primary attestation | 4 | 21 494 B | OK |
| `FallbackAggregatorVerifier.bin` | 1B fallback attestation | 4 | 21 493 B | OK (K=21 inner) |
| `LayerHashesAggregatorVerifier.bin` | 2 layer hashes | 14 | 23 111 B | OK (k_outer=21) |
| `BridgeWithdrawalAggregatorVerifier.bin` | 4 withdrawal | 10 | 20 990 B | OK (K=19 inner) |

Layer hashes grew because the aggregator was re-keygen'd at `k_outer=21`: at `k_outer=20` the
outer circuit did not fit the 14 inner public inputs. The margin to EIP-170 is 1 465 B, the
tightest of the four — regenerate with the size gate in the loop, not after it.

Every size is also pinned in `SIZES`, next to `SHA256SUMS`, and
`scripts/check_shplonk_artefacts.sh` fails on drift and warns from 90% of EIP-170 (layer hashes
warns today, at 94%). That is deliberate: past the limit `CREATE` returns the zero address and
`deployYulFromBin` reverts `YulDeployFailed`, so growth has to be visible in a diff rather than in
a failed deploy. Regenerating an artefact means updating `SIZES` in the same commit.

Each also ships a `*_calldata.bin` reference fixture (`instances ‖ proof`). The generated
`Halo2Verifier` Solidity sources are kept for reference for 1A, 1B and 2 only — the `.bin` is what
deploys.

Circuit **4** (`withdrawByProof`) uses the same SHPLONK aggregator path. Its inner event circuit
is keygen'd at `K=19`; the aggregated Yul is 20 990 B (22 outer instances = 12 KZG accumulator
limbs + 10 re-exposed Circuit-4 public inputs). Landed 2026-07-07 (M4).

All three `verifyBlock` circuits use the SHPLONK aggregator path. Circuit **1B** is keygen'd at
inner `K=21` (vs `K=20` for primary/layer): the fallback circuit verifies two attestation
envelopes, so at `K=20` it needs 44 advice columns and the aggregator Yul exceeds EIP-170
(~28 KB). At `K=21` it auto-configures to 22 advice columns and the Yul drops to 21 493 B. 

## Generate SHPLONK `.bin` (1A + 1B + 2)

```bash
cd crates/bridge-snark-utils
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

## Generate SHPLONK `.bin` (Circuit 4 withdrawal)

Circuit 4's inner Poseidon snark is produced by a dedicated `bridge-snark-utils` bin (the
`export-bound-poseidon-snarks` bound-block path only emits 1A/1B/2). It keygens the
event circuit (K=19), provisions `params/kzg_bn254_19.srs` by downsizing the shared
K=21 ceremony, proves a synthetic-but-valid reference witness under a Poseidon
transcript, and self-verifies before wrapping:

```bash
cd crates/bridge-snark-utils
cargo run --release --bin export-c4-poseidon-snark -- \
  --params-dir ../../params --snark-dir ../../proofs/bound/poseidon-snark

cd ../bridge-evm-aggregator
cargo run --release --bin export-inner-aggregator -- \
  --inner-snark ../../proofs/bound/poseidon-snark/circuit4.snark \
  --out-dir ../../contracts/ethereum/verifiers \
  --name BridgeWithdrawalAggregatorVerifier
```

The synthetic reference witness only pins the **circuit shape** (VK): real withdrawal
proofs generated from live Acki Nacki blocks verify against the emitted verifier
byte-for-byte, since a SHPLONK verifier is bound to the VK, not the witness.

Or run the whole pipeline on n14: `./scripts/n14_r15_proving_run.sh continue-c && ./scripts/n14_r15_proving_run.sh pull-artifacts`.

```bash
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

Override SHPLONK paths via env: `SHPLONK_BIN_PRIMARY`, `SHPLONK_BIN_FALLBACK`, `SHPLONK_BIN_LAYER_HASHES`, `SHPLONK_BIN_WITHDRAWAL`.

Hashes are pinned in `SHA256SUMS`. Gate: `./scripts/check_shplonk_artefacts.sh` then `forge test --match-contract ShplonkArtefactPairing` (all four pairs). CREATE `extcodehash` pins are in `ShplonkDeployLib`.

M2 multiply spike fixtures live under `test/fixtures/r15_spike/` for Foundry only — **not** valid production verifiers.
