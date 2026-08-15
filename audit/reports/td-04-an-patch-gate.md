# TD-04 — AN deposit security gate (`eccUSDCBridge` @ contracts/bridge)

Bridge-repo gate + synced AN contracts. Upstream: `acki-nacki@contracts/bridge` — **`eccUSDCBridge` v1.3.x**, **`DepositVoucher` v1.1.x**.

**Legacy:** audit-overlay `USDCBridge_12pi_chainid_allowlist.patch` (single `setExpectedBridge`, anchors, mint cap, attesters) — **superseded** by upstream bridge branch for deploy handoff.

**Owner handoff (v2):** `docs/operations/td-04-an-deploy-owner-handoff.md` — `setTrustedL1Bridge`, paired `.tvc`, VkBlob pin.  
**Repo gate:** `scripts/check_td04_deploy_readiness.sh`

---

## Checklist: bridge branch vs legacy overlay

| Security item | `contracts/bridge` (`eccUSDCBridge`) | Legacy overlay (`USDCBridge.sol` patch) | Live deploy |
|---------------|--------------------------------------|----------------------------------------|-------------|
| 12-PI parser (`chainId` fr[4], 9 Fr read) | **OK** — synced + prover 12 PI | **OK** — same parser shape | **QC ops** |
| L1 bridge allowlist | **`_trustedL1Bridge` SET** + `setTrustedL1Bridge` | `_expectedBridgeFr` + `setExpectedBridge` | **QC ops** — owner seed |
| AN dapp id gate | **Pinned `f.dappId = 0`** on-chain | `setExpectedAnDappId` / ERR_WRONG_DAPP | N/A on bridge branch |
| Block anchor / ERR_UNKNOWN_BLOCK | **Not in contract** | `_acceptedBlockHash` + attesters | **Ops elsewhere** |
| Cross-chain voucher identity | **`chainId` in hash** (6-arg voucher) | `srcChainId` naming in overlay patch | **QC ops** — breaking voucher if mismatched |
| Per-chain mint cap | **Not in contract** | `_mintCapByChain` | **Ops / policy** |
| M-of-N `attestBlockHash` | **Not in contract** | overlay attester set | **Ops elsewhere** |
| VkBlob 12-PI (5006 B, `9dacd998…`) | **OK** — TD-42 pin + `preserve_audit_vk_blob.sh` | same pin | **QC ops** — rotate on deploy |

---

## Verdict summary

| Layer | Class |
|-------|-------|
| Bridge relayer/prover 12-PI + mock gates | **OK** |
| Synced `eccUSDCBridge` + patch matrix (`_trustedL1Bridge`) | **OK** (repo gate green) |
| BC-AN-01 dual-proof regression (384 B PI) | **OK** |
| Live AN deploy + `setTrustedL1Bridge` seed | **QC ops** |
| Legacy overlay-only setters on live cluster | **OBSOLETE** if deploying bridge branch bytecode |

**Not BC** in bridge repo: upstream aligned with 12 PI; gap is live deployment and allowlist seeding.

---

## Deploy readiness checklist (ops — repo does not deploy)

**Pre-deploy (code):**

1. `bash scripts/sync_an_contracts.sh` (default `contracts/bridge`).
2. `cd audit/spec/an-contracts && bash build.sh` → `eccUSDCBridge.tvc`, `DepositVoucher.tvc`.
3. `python3 scripts/check_voucher_abi_consistency.py --source-only audit/spec/an-contracts/exchange` — **6-arg** voucher / `confirmDeposit`.
4. `bash scripts/check_an_overlay_patch_matrix.sh` — `_trustedL1Bridge` + `setTrustedL1Bridge` on `eccUSDCBridge.sol`.
5. `bash scripts/check_td04_deploy_readiness.sh` — aggregator.

**Post-deploy owner seed (fail-closed until done):**

1. For each supported L1 `chainId`: **`setTrustedL1Bridge(chainId, l1BridgeAddress, true)`** where `l1BridgeAddress` matches PI `contractAddr` for that chain's bridge.
2. Optional: `setPubkey` — owner key rotation.

**OBSOLETE on bridge branch** (do not document as required for eccUSDCBridge deploy):

- `setExpectedBridge`, `setExpectedAnDappId`, `setAcceptedBlockHash`, `setMintCap`, attester bootstrap — see handoff §7.

**Post code-upgrade:** re-run allowlist seed — `_trustedL1Bridge` is not carried by `updateCode` alone.

**VkBlob:** fixture sha256 `9dacd998af5fd03af8097cb80a571df098c925bba235af61d920cc808360fae3`; embed via `scripts/embed_deposit_vk_blob.py` into `eccUSDCBridge.sol`.

---

## `scripts/check_an_overlay_patch_matrix.sh`

On `eccUSDCBridge.sol` (default after sync):

| Check | Pattern |
|-------|---------|
| 12-PI parser | `for (uint k = 0; k < 9` |
| chainId at fr[4] | `f.chainId = fr[4]` |
| Trusted SET allowlist | `_trustedL1Bridge`, `setTrustedL1Bridge` |
| Voucher chainId hash | `abi.encode(depositId, contractAddr, dappId, chainId)` |

Legacy overlay checks (`_expectedBridgeFr`, anchors, mint cap, attesters) run only when `USDCBridge.sol` overlay is present.

---

## `scripts/check_voucher_abi_consistency.py`

```
python3 scripts/check_voucher_abi_consistency.py --source-only audit/spec/an-contracts/exchange
```

Expect **6** constructor parameters including chain-binding identity field (`chainId` on bridge branch).

---

## Bridge PoC registry

| TD | Covers |
|----|--------|
| TD-02 | 12-PI vs legacy parser misread |
| TD-03 | forged block / anchor (relayer; anchor not on eccUSDCBridge) |
| TD-05 | dappId injection (on-chain pin to 0) |
| TD-15 | cross-chain identity / collision |
| TD-04 | this gate + `td_04_an_patch_gate.rs` + `check_an_overlay_patch_matrix.sh` |
| BC-AN-01 | dual dappId PI proofs — regression green (384 B) |

---

## Commands

    bash scripts/check_td04_deploy_readiness.sh
    bash scripts/check_an_overlay_patch_matrix.sh
    cd crates/deposit-relayer-daemon && cargo test td_04 -- --nocapture
    cd audit/spec/an && python -m pytest integration/test_bc_an_01_dapp_id_double_mint.py -v
