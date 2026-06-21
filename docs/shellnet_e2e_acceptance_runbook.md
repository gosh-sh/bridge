# Shellnet E2E Acceptance Runbook

**Target:** local 5-node AN cluster + Sepolia bridge with **R15 SHPLONK aggregators** (no identity-stub Groth16).

**Prerequisite:** read `docs/testnet_security_status.md` — do not run against legacy stub deployments for sign-off.

## 1. Cluster + relayers

```bash
# AN cluster (history_proofs enabled)
cd ../acki-nacki/nock && docker compose up -d

# GraphQL tunnel if prover runs on n14
ssh -fN -R 127.0.0.1:18080:127.0.0.1:80 -p 22488 gosh@94.156.178.14

# Prover + verifier daemons (partner repo)
cd ../acki-nacki-to-eth-bridge-halo2-prover
./scripts/run_n14_prover_stack.sh   # or local equivalent
```

## 2. Generate R15 aggregated proofs

For each circuit, produce **Poseidon-transcript** inner proofs + aggregator bundle:

```bash
# Example: bound block (1A + 2) with Poseidon flavour — orchestrator
cd crates/bridge-prover-orchestrator
# export with TranscriptKind::Poseidon (see export_* binaries / bound export)

cd ../bridge-evm-aggregator
cargo test --release -- --ignored aggregator_round_trip   # M2 gate sanity
# Per-circuit: aggregate_inner + generate_yul_verifier_gated → contracts/ethereum/verifiers/*.bin
```

Verify EIP-170:

```bash
./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers
```

## 3. Deploy Sepolia bridge (Shplonk adapters)

```bash
cd contracts/ethereum

# Generate four .bin files into verifiers/ first (see §2)
./scripts/check_eip170_verifier_bins.sh verifiers

export WIRE_VERIFY_BLOCK=true
export WIRE_WITHDRAW_BY_PROOF=true
export GENESIS_BK_SET_COMMITMENT=0x...
export GENESIS_PREV_MAX_LEVEL_LAYER_HASH=0x...
export WITHDRAW_ACC_FR=0x...
export START_PAUSED=true   # default; owner unpause() after sign-off

forge script script/DeployRealBridge.s.sol:DeployRealBridge \
  --rpc-url $SEPOLIA_RPC --broadcast
```

Shellnet E2E variant (always wires verifyBlock + withdraw):

```bash
forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url $SEPOLIA_RPC --broadcast
```

Deploy scripts load `verifiers/*.bin` via `ShplonkDeployLib` — no stub Groth16, no mocks.
Override paths with `SHPLONK_BIN_PRIMARY`, `SHPLONK_BIN_FALLBACK`, `SHPLONK_BIN_LAYER_HASHES`, `SHPLONK_BIN_WITHDRAWAL`.

## 4. Acceptance sequence

| Step | Action | Pass criterion |
|------|--------|----------------|
| A | `verifyBlock` × N key blocks | `storedLastSeenBlockSeqNo` monotonic; real Shplonk proofs |
| B | BK rotation (if cluster rotates) | `applyBkSetUpdate` tx; `storedBkSetCommitment` advances |
| C | `withdrawByProof` | Treasury −amount; nullifier set; Shplonk C4 proof |
| D | Sepolia `deposit` → AN `finalizeDeposit` | Live `deposit-relayer` without `--dry-run` |
| E | Forgery negatives | `forge test --match-contract ShplonkAggregatorForgery` green |

## 5. Operator script

```bash
./scripts/shellnet_e2e.sh --rpc $SEPOLIA_RPC --bridge $BRIDGE --an-url http://127.0.0.1:11000
```

## 6. Sign-off artefact

Archive in `logs/shellnet_e2e_YYYYMMDD/`:

- Sepolia tx hashes (verifyBlock, applyBkSetUpdate, withdraw, deposit)
- Verifier `.bin` SHA-256 + byte sizes (all ≤ 24 576)
- `forge test` summary
- Prover `proofs/result_*.json` excerpt showing `verified=true`

## Exit criteria

- Zero stub Groth16 in production path
- All four verifier runtime bytecodes ≤ 24 576 B
- One complete logged run of steps A–D
