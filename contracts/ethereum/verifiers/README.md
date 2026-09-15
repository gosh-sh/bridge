# R15 SHPLONK verifier bytecode (production)

Deploy scripts (`DeployRealBridge`, `DeployShellnetE2EBridge`) load these artefacts. 

Sizes below were measured with `wc -c` on the committed `.bin` files on **2026-08-18**, the same
metric `scripts/check_eip170_verifier_bins.sh` uses. Two of them had drifted from the figures this
table carried since 2026-06-22 (Primary 21 493 → 21 494, Withdrawal 20 987 → 20 990), because the
artefacts were regenerated on 2026-08-13 and the table was not.

| File | Circuit | Inner PIs | Size | EIP-170 (24 576 B) |
|------|---------|-----------|------|--------------------|
| `PrimaryAggregatorVerifier.bin` | 1A primary attestation | 4 | 21 494 B | OK |
| `FallbackAggregatorVerifier.bin` | 1B fallback attestation | 4 | 21 493 B | OK (K=21 inner) |
| `LayerHashesAggregatorVerifier.bin` | 2 layer hashes | 14 | 19 100 B | OK |
| `BridgeWithdrawalAggregatorVerifier.bin` | 4 withdrawal | 10 | 20 990 B | OK (K=19 inner) |

Each also ships a `*_calldata.bin` reference fixture (`instances ‖ proof`) and its generated Solidity
source, `<name>.sol`. The two roles are different and both matter:

- the `.bin` is what deploys, and what the CLI's stage 1 and the relayer preflight compare with the
  runtime code on chain;
- the `.sol` is what `aggregate-proof` compares every proof's regenerated verifier against. It does
  not compile anything, so neither the withdrawal CLI nor the relayer needs `solc`.

Nothing at run time checks that a `.sol` compiles to its `.bin`. `scripts/check_verifier_sources.sh`
does, with `solc 0.8.19`, and `.woodpecker/verifier_sources.yaml` runs it on every pull request that
touches this directory:

```bash
SOLC=/path/to/solc-0.8.19 scripts/check_verifier_sources.sh contracts/ethereum/verifiers
```

**Regenerate the pair together.** `export-inner-aggregator` writes both files from one run; never
replace one of them alone. A `snark-verifier` upgrade can change the generated source without
changing the key — `aggregate-proof` then refuses with `aggregator VK drift`, and the fix is to
regenerate both files, not to suspect the key. Regeneration compiles the source, so it needs
`solc 0.8.19` on `PATH`.

Circuit **4** (`withdrawByProof`) uses the same SHPLONK aggregator path. Its inner event circuit
is keygen'd at `K=19`; the aggregated Yul is 20 987 B (22 outer instances = 12 KZG accumulator
limbs + 10 re-exposed Circuit-4 public inputs). Landed 2026-07-07 (M4).

All three `verifyBlock` circuits use the SHPLONK aggregator path. Circuit **1B** is keygen'd at
inner `K=21` (vs `K=20` for primary/layer): the fallback circuit verifies two attestation
envelopes, so at `K=20` it needs 44 advice columns and the aggregator Yul exceeds EIP-170
(~28 KB). At `K=21` it auto-configures to 22 advice columns and the Yul drops to 21 493 B. 

## Generate SHPLONK `.sol` + `.bin` (1A + 1B + 2)

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

## Generate SHPLONK `.sol` + `.bin` (Circuit 4 withdrawal)

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
# then check the pairs
SOLC=/path/to/solc-0.8.19 scripts/check_verifier_sources.sh contracts/ethereum/verifiers
```

Override SHPLONK paths via env: `SHPLONK_BIN_PRIMARY`, `SHPLONK_BIN_FALLBACK`, `SHPLONK_BIN_LAYER_HASHES`, `SHPLONK_BIN_WITHDRAWAL`.

M2 multiply spike fixtures live under `test/fixtures/r15_spike/` for Foundry only — **not** valid production verifiers.
