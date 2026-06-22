# Production Plan — Acki Nacki Bridge (R15 SHPLONK)

**Last updated:** 2026-06-22  
**Status:** Phase 1 gates implementable; Phase 2–4 blocked on partner / AN wiring.

This document is the operator-facing production checklist. Automated gates live in
`scripts/production_preflight.sh` (`make production-preflight`).

## Architecture (production)

| Circuit | On-chain verifier | Proof submitted by relayer |
|---------|-------------------|----------------------------|
| 1A Primary | `PrimaryAggregatorVerifier` (SHPLONK `.bin`) | `PrimaryAggregatorVerifier_calldata.bin` shape (~3.8 KB) |
| 1B Fallback | `FallbackAggregatorVerifier` (SHPLONK `.bin`, K=21 inner) | `FallbackAggregatorVerifier_calldata.bin` shape (~3.8 KB) |
| 2 Layer hashes | `LayerHashesAggregatorVerifier` (SHPLONK `.bin`) | `LayerHashesAggregatorVerifier_calldata.bin` shape (~3 KB) |
| 4 Withdrawal | `BridgeWithdrawalAggregatorVerifier` (SHPLONK `.bin`) | C4 aggregator calldata — **not ready** |

All three `verifyBlock` circuits use the R15 SHPLONK aggregator path. Circuit 1B is
keygen'd at inner `K=21` (one degree above the `K=20` primary path) so its aggregated Yul
fits EIP-170 at 21 493 B — the gnark Groth16 fallback hybrid is retired (see
`docs/r15_verifier_sizing_report.md`).

**Never deploy:** identity-stub Groth16 for 1A/2, `MockBridgeWithdrawalVerifier`, legacy
`PrimaryGroth16VerifierGenerated` / `LayerHashesGroth16VerifierGenerated` in production
`verifyBlock` slots.

---

## Phase 0 — Release gates (automated)

**Goal:** CI / operator can run one command before any deploy.

```bash
make production-preflight
```

Checks:

1. `verifiers/PrimaryAggregatorVerifier.bin` + `LayerHashesAggregatorVerifier.bin` exist and ≤ 24 576 B
2. Matching `*_calldata.bin` fixtures present (bound smoke)
3. `forge test --match-contract 'ShplonkAggregatorForgery|AckiNackiBridgeProductionVerifyBlock|ShplonkDeployLib'`
4. `cargo test` in `bridge-relayer-daemon`
5. SHA-256 manifest of verifier artefacts

**Exit:** green preflight log archived under `logs/production_preflight_YYYYMMDD/`.

---

## Phase 1 — `verifyBlock` on Sepolia / shellnet (ready)

**Goal:** All-SHPLONK bridge deployed, paused, smoke-verified, then unpaused for state sync only.

**Known issue (2026-06-22):** Primary (1A) and Fallback (1B) SHPLONK calldata both verify in
Foundry (`test_productionPrimaryAttestation_isolated` / `test_productionFallbackAttestation_isolated`).
The full `verifyBlock` E2E is still gated on Circuit 2: the layer-hashes SHPLONK proof fails its
KZG pairing in isolation regardless of `K_outer` (21 or 22), so the issue is in the Circuit 2
aggregation itself, not the verifier size. `test_productionVerifyBlock_boundCalldata_advancesState`
skips with a logged note until this is resolved. **Do not unpause for production traffic** until the
full E2E is green.

### 1.1 Generate artefacts (n14 or local)

```bash
# Full pipeline (A → A2 → C, exports all three SHPLONK aggregators incl. 1B) or resume:
./scripts/n14_r15_proving_run.sh continue-c
./scripts/n14_r15_proving_run.sh pull-artifacts
```

The fallback 1B inner snark keygens at `K=21` (`FALLBACK_K` in
`crates/bridge-prover-orchestrator/src/keys.rs`); delete `params/fallback_*.bin` to force a
re-keygen if the degree ever changes.

### 1.2 Preflight

```bash
make production-preflight
cp scripts/production_env.example .env.production   # fill PRIVATE_KEY, RPC, genesis anchors
```

### 1.3 Deploy (paused)

```bash
cd contracts/ethereum
source ../../.env.production   # or export vars manually

export WIRE_VERIFY_BLOCK=true
export START_PAUSED=true
export GENESIS_BK_SET_COMMITMENT=0x...    # from cluster / bk_set
export GENESIS_PREV_MAX_LEVEL_LAYER_HASH=0x...

forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url "$SEPOLIA_RPC" --broadcast
```

Withdraw wiring stays off until Phase 2 (`WIRE_WITHDRAW_BY_PROOF=false` or omit C4 `.bin`).

### 1.4 Post-deploy smoke (read-only + one tx)

```bash
# Anchor alignment (no key)
cargo run --bin relayer -- verify-fixture \
  --fixtures-dir ../bridge-prover-orchestrator/proofs/bound \
  --rpc-url "$SEPOLIA_RPC" --bridge-address "$BRIDGE"

# One bound block (needs RELAYER_PRIVATE_KEY)
cargo run --bin relayer -- smoke-fixture \
  --fixtures-dir ../bridge-prover-orchestrator/proofs/bound \
  --rpc-url "$SEPOLIA_RPC" --bridge-address "$BRIDGE" \
  --private-key "$RELAYER_PRIVATE_KEY" --max-ticks 1
```

### 1.5 Unpause

Owner calls `unpause()` only after Phase 0 green + 1.4 smoke tx succeeded.

**Trust note:** Fallback blocks rely on off-chain Halo2 verification at gnark-wrap time.
Primary + layer paths verify Halo2 inside the SHPLONK aggregator on-chain.

---

## Phase 2 — `withdrawByProof` (blocked: partner M4)

**Goal:** Real C4 SHPLONK aggregator on chain; remove mock withdrawal verifier.

| Step | Owner | Blocker |
|------|-------|---------|
| Partner ships stable Circuit 4 + Poseidon export | Partner | M4 |
| `circuit4.snark` → `BridgeWithdrawalAggregatorVerifier.bin` | Bridge team | M5–M6 |
| `WIRE_WITHDRAW_BY_PROOF=true` in deploy | Bridge team | C4 `.bin` |
| Relayer `submit-withdraw` against real calldata | Bridge team | C4 proofs from AN |
| Forgery tests for C4 adapter | Bridge team | M7 |

Keep `withdrawByProof` disabled until this phase completes.

---

## Phase 3 — ETH→AN deposits (blocked: AN live client)

**Goal:** `deposit-relayer daemon` without `--dry-run`.

| Step | Owner |
|------|-------|
| Redeploy AN `TokenBridge` with deposit-prover VkBlob | AN / bridge |
| Land live `IAckiNacki` send in `acki-nacki-interface` | AN SDK |
| Shellnet VK redeploy per `docs/shellnet_usdcbridge_deposit_vk_redeploy.md` | Bridge team |
| E2E: Sepolia `deposit` → `finalizeDeposit` | Bridge team |

---

## Phase 4 — Live AN→ETH relayer (blocked: partner proof format)

**Goal:** `daemon-prover` drives `verifyBlock` from partner `proof_<seqno>.json`.

| Step | Owner |
|------|-------|
| Partner prover emits SHPLONK calldata for 1A + 1B + 2 (not 256-byte Groth16) | Partner |
| `relayer daemon-prover` on n14 + Sepolia signer | Ops |
| 10-block shellnet acceptance (runbook §4 step A) | Bridge team |

---

## Phase 5 — BK rotation (`applyBkSetUpdate`)

**Goal:** On-chain BK-set commitment updates when cluster rotates.

| Step | Owner |
|------|-------|
| Relayer `submit-bk-update` wired to live attestation proofs | Bridge team |
| Optional Circuit 3 ZK rotation proof (future) | Partner |

---

## Phase 6 — Mainnet readiness

- External audit sign-off on the all-SHPLONK trust model (1A/1B/2 aggregator verifiers)
- Treasury / pause / multisig runbooks
- Monitoring: relayer metrics, `storedLastSeenBlockSeqNo`, nullifier set
- Incident: `pause()` + rotate keys if proving material leaks

---

## Quick reference

| Command | Purpose |
|---------|---------|
| `make production-preflight` | All Phase 0 gates |
| `./scripts/shellnet_e2e.sh` | Shellnet driver (extends preflight) |
| `./scripts/n14_r15_proving_run.sh continue-c` | Regenerate SHPLONK `.bin` on n14 |
| `docs/shellnet_e2e_acceptance_runbook.md` | Full acceptance sequence |
| `docs/testnet_security_status.md` | Safe vs unsafe paths |
