# TD-04 — Owner handoff: AN eccUSDCBridge deploy readiness

**Audience:** owner / partner ops (Acki Nacki cluster).  
**Audience (bridge repo):** auditors — gates green; this doc does **not** perform deploy.

**Upstream:** `../acki-nacki` @ **`contracts/bridge`** — `eccUSDCBridge` v1.3.x + `DepositVoucher` v1.1.x.  
**Audit detail:** `audit/reports/td-04-an-patch-gate.md`

---

## 1. Scope: owner vs bridge repo

| Area | Bridge repo (green) | Owner / ops (this handoff) |
|------|---------------------|----------------------------|
| Synced sources | `eccUSDCBridge.sol` + `DepositVoucher.sol` via `sync_an_contracts.sh` | Deploy paired `.tvc` to target dApp |
| VkBlob pin `9dacd998…` (5006 B, 12 PI) | TD-42 CI gate | Embedded in deployed bridge; rotate → redeploy |
| Relayer / prover 12 PI | mock tests + gates | Live RPC, keys, relayer policy |
| L1 bridge allowlist | `_trustedL1Bridge` SET in contract | **Post-deploy seed:** `setTrustedL1Bridge` per chain |
| E2E finalize path | TD-53 mock smoke | Shellnet `prove-one` / `finalize-one` (optional) |

**Verdict:** QC ops — repo ready after `check_td04_deploy_readiness.sh`; live deploy is owner action.

---

## 2. Contract model (eccUSDCBridge @ contracts/bridge)

| Item | Detail |
|------|--------|
| Contract name | **`eccUSDCBridge`** (not legacy audit-overlay `USDCBridge`) |
| Public inputs | **12 × 32 B** LE Fr; `chainId` at **fr[4]** |
| On-chain dappId | **Pinned to 0** — PI dapp limbs ignored (`f.dappId = 0`) |
| Allowlist | **`mapping(chainId => mapping(l1Bridge => bool))`** — SET per L1 bridge address |
| DepositVoucher | **6 constructor args:** `depositId`, `contractAddr`, `dappId`, **`chainId`**, `amount`, `anAccount` |
| Voucher identity hash | `abi.encode(depositId, contractAddr, dappId, chainId)` — chain-bound replay slot |

---

## 3. Sync & build (before deploy)

From bridge repository root:

    bash scripts/sync_an_contracts.sh
    # default ACKI_NACKI_BRANCH=contracts/bridge (override if needed)

Build TVCs:

    cd audit/spec/an-contracts && bash build.sh

Outputs:

| Artifact | Path |
|----------|------|
| Bridge | `build/eccUSDCBridge.tvc` |
| Voucher (embedded in bridge cell) | `build/DepositVoucher.tvc` + code in `_depositVoucherCode` |

**Ship together:** `eccUSDCBridge` + embedded `DepositVoucher` code — mismatch causes voucher failures / deposits stop minting. Run voucher ABI check before deploy.

---

## 4. Pre-deploy gates (copy-paste)

    bash scripts/check_td04_deploy_readiness.sh

Equivalent manual steps:

    bash scripts/check_deposit_audit_gates.sh
    cd audit/spec/an-contracts && bash build.sh
    python3 scripts/check_voucher_abi_consistency.py \
      --source audit/spec/an-contracts/exchange --source-only
    bash scripts/check_an_overlay_patch_matrix.sh
    cd crates/deposit-relayer-daemon && cargo test td_04 -- --nocapture

VkBlob pin (included in deposit gates):

    bash scripts/check_vk_srs_pin.sh

---

## 5. Deploy steps

1. Build TVCs (section 3).
2. Partner runbooks:
   - [shellnet_usdcbridge_deposit_vk_redeploy.md](shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md) — VkBlob 12 PI, opcode verify, redeploy checklist
   - [shellnet_an_eth_relayer_wiring.md](shellnet/shellnet_an_eth_relayer_wiring.md) — relayer / GraphQL / keys context
3. Deploy via your AN pipeline (zerostate / `updateCode` / dApp publish) — **not automated from this repo**.

---

## 6. Post-deploy seed (primary)

Until allowlisted, `finalizeDeposit` fails with **`ERR_UNSUPPORTED_SRC_CHAIN` (222)**.

For **each supported source L1 chain** (e.g. Sepolia `11155111`, mainnet `1`, …):

1. Compute the **L1 bridge contract address** as the circuit public input **contractAddr** (PI fr[3], 256-bit Fr).
2. Owner call (signed with bridge owner pubkey):

       setTrustedL1Bridge(chainId, l1BridgeAddress, true)

   - `l1BridgeAddress` = uint256 form of the L1 `AckiNackiBridge` (or overlay) address that emits deposits.
   - Multiple bridges per chain are supported (SET, not single-slot).
3. Verify getter: `isTrustedL1Bridge(chainId, l1BridgeAddress)` → `true`.

**Optional:** `setPubkey(newOwnerPubkey)` — rotate owner signing key (existing owner only).

There is **no** `setExpectedBridge`, `setExpectedAnDappId`, or single-bridge Fr slot on `eccUSDCBridge` @ `contracts/bridge`.

---

## 7. OBSOLETE — legacy overlay patch only

The following existed on audit-overlay **`USDCBridge.sol`** (patch `USDCBridge_12pi_chainid_allowlist`) but are **not** in upstream **`eccUSDCBridge.sol`** on `contracts/bridge`. Do **not** expect these setters on deployed bridge branch code:

| Legacy overlay step | Status on `contracts/bridge` |
|---------------------|------------------------------|
| `setExpectedBridge(chainId, bridgeFr)` | **OBSOLETE** — use `setTrustedL1Bridge` |
| `setExpectedAnDappId(dappId)` | **Not in contract** — on-chain dappId pinned to 0 |
| `setAcceptedBlockHash` / anchor gate (224) | **Not in contract** — ops / policy elsewhere |
| `setMintCap` / `_mintCapByChain` | **Not in contract** — caps via ops / product policy |
| `setAttester` / `attestBlockHash` M-of-N | **Not in contract** — not on this bridge branch |
| `disableOwnerAnchors()` | **Not in contract** |

If your deployment still uses legacy overlay `USDCBridge`, see archived notes in `audit/reports/td-04-an-patch-gate.md` § legacy overlay.

---

## 8. DEP-N-5 / voucher identity

Post-`contracts/bridge` voucher uses **`chainId`** (4th identity field), not legacy overlay name `srcChainId`:

    abi.encode(depositId, contractAddr, dappId, chainId)

`confirmDeposit` on bridge accepts the same six fields. Voucher ABI check (`check_voucher_abi_consistency.py`) must pass **6-arg** constructor before deploy.

---

## 9. Post code-upgrade

`updateCode` / migration does **not** automatically restore `_trustedL1Bridge` entries.

Re-run **section 6** allowlist seed after any bridge code upgrade.

---

## 10. Verification smoke (repo)

    cd audit/spec/an && python -m pytest integration/test_bc_an_01_dapp_id_double_mint.py -v
    cd crates/deposit-relayer-daemon && cargo test td_04 td_05 td_15 -- --nocapture
    cd deposit-prover && cargo test td_03 -- --nocapture

Optional ops (shellnet / Sepolia):

    bash scripts/td_53_dry_run_recovery_smoke.sh   # mock only
    # Live: docs/operations/evm_an_deposit_e2e_runbook.md

Full AN pytest (with fixtures):

    cd audit/spec/an && python -m pytest -q

---

## 11. Rollback / VkBlob rotation (TD-42)

| Action | Reference |
|--------|-----------|
| Pin check | `bash scripts/check_vk_srs_pin.sh` |
| Fixture | `deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin` |
| Embed | `python3 scripts/embed_deposit_vk_blob.py audit/spec/an-contracts/exchange/eccUSDCBridge.sol` |
| Rebuild + redeploy | `audit/spec/an-contracts/build.sh` → **both** `.tvc` |
| Notes | `audit/reports/td-42-vk-srs-pin-notes.md` |

Rotating VkBlob without paired redeploy breaks all `finalizeDeposit` (opcode reject).

---

## 12. Related gates & docs

| Item | Path |
|------|------|
| Readiness script | `scripts/check_td04_deploy_readiness.sh` |
| Patch / gate matrix | `scripts/check_an_overlay_patch_matrix.sh` |
| TD-04 audit gate | `audit/reports/td-04-an-patch-gate.md` |
| Phase 2 milestone | `audit/reports/phase-2-deposit-eth-milestone.md` |
| Operator runbook | `docs/audit/deposit-relayer-operator-runbook.md` |
