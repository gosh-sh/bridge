> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Hermez KZG — required repos, branches, and links

Correct pins for Hermez Powers-of-Tau migration of deposit + Circuit 1B
fixtures and USDCBridge `VK_BLOB` (`ZKHALO2VERIFYWITHVK` embeds Hermez
`s_g2 = 928fafb3…`, **not** chain `c6028acf…`).

Generated: 2026-07-15.

---

## 1. Core (use these tips)

| Role | Repo | Branch / ref | Link |
|------|------|--------------|------|
| **Opcode fixtures (deposit + Circuit 1B) — living tip** | `tvmlabs/tvm-sdk` | `pruvendo/hermez-deposit-fixtures` @ `915e6998` | https://github.com/tvmlabs/tvm-sdk/tree/pruvendo/hermez-deposit-fixtures |
| Bridge Hermez-only prover + regenerated fixtures | `gosh-sh/bridge` | `pruvendo/hermez-kzg-fixtures` @ `903efff` | https://github.com/gosh-sh/bridge/tree/pruvendo/hermez-kzg-fixtures |
| Shellnet E2E parent (base of bridge Hermez PR) | `gosh-sh/bridge` | `pruvendo/shellnet-e2e-landing` | https://github.com/gosh-sh/bridge/tree/pruvendo/shellnet-e2e-landing |
| USDCBridge `VK_BLOB` = Hermez deposit VkBlob | `gosh-sh/acki-nacki` | `pruvendo/hermez-deposit-vk-blob` @ `0d2f7a063` | https://github.com/gosh-sh/acki-nacki/tree/pruvendo/hermez-deposit-vk-blob *(needs write access to push)* |

Bridge GitLab mirror (when `:22` reachable):

| Role | URL |
|------|-----|
| Canonical GitLab | https://vcs.modus-ponens.com/ton/acki-nacki-bridge (`origin`) |

### Important tip note (`tvm-sdk`)

| Ref | What it has |
|-----|-------------|
| `pruvendo/hermez-deposit-fixtures` @ **`915e6998`** | Hermez **deposit_10proofs** (`304c1c4e…`) **and** Hermez Circuit 1B `fallback_*` K=21 (`9ba63795…`) |
| Merge of PR #276 @ `1de7fa9e` | Hermez **deposit** only (1B commit landed on the branch **after** that merge) |
| `feature/hermez-kzg-resurrection`, `fix_rc` | **Deleted** after PRs merged — do not clone by those names |
| `main`, `3.0.4.an-rc`, `full_dex_and_bridge_test_with_final_halo2_circuit` (checked 2026-07-15) | Still older deposit VkBlob `20cf9018…` / old K=20 fallback — **not** Hermez-correct for this work |

Until Hermez lands on a durable release branch, pin **`pruvendo/hermez-deposit-fixtures`**.

---

## 2. Pull requests

| PR | State | URL |
|----|-------|-----|
| tvm-sdk #275 — Hermez SRS + DarkDex W=128 VK | MERGED (into then-deleted `fix_rc`) | https://github.com/tvmlabs/tvm-sdk/pull/275 |
| tvm-sdk #276 — Hermez `deposit_10proofs` + un-ignore deposit tests | MERGED (into then-deleted `feature/hermez-kzg-resurrection`); **follow-up 1B commit still only on branch tip** | https://github.com/tvmlabs/tvm-sdk/pull/276 |
| bridge #10 — Hermez-only fixtures + no chain / `gen_srs` fallback | OPEN → `pruvendo/shellnet-e2e-landing` | https://github.com/gosh-sh/bridge/pull/10 |
| bridge #5 — Shellnet E2E landing | OPEN → `main` | https://github.com/gosh-sh/bridge/pull/5 |

Alina migration doc:

- https://github.com/tvmlabs/tvm-sdk/blob/pruvendo/hermez-deposit-fixtures/tvm_vm/doc/HERMEZ_KZG_MIGRATION_FOR_SERGEY.md

---

## 3. Fixture / artefact paths

| Artefact | Path | Expected digests / size |
|----------|------|-------------------------|
| Deposit VkBlob + 10 proofs | `tvm_vm/halo2_test_data/deposit_10proofs/` | VkBlob sha256 **`304c1c4ed1e4cf09a00fb1d83a0ae2ba42db2afead035ae089a4faa85346251a`** (3982 B) |
| Same (producer) | `bridge` → `deposit-prover/fixtures/deposit_10proofs/` | same sha256 |
| Circuit 1B operands | `tvm_vm/halo2_test_data/fallback_{vk_blob,public_inputs,proof,vk,config_params}.*` | vk_blob **`9ba63795…6444c9`** (3364 B), proof 7616 B, **k=21** |
| Same (producer) | `bridge` → `crates/bridge-snark-utils/fixtures/circuit_1b_fallback/` | same |
| USDCBridge constant | `acki-nacki` → `contracts/exchange/USDCBridge.sol` | `VK_BLOB` byte-identical to deposit VkBlob |
| Partner zip (gitignored) | `hermez_usdcbridge_vk_for_alina_2026-07-14.zip` | drop for Alina/Serhii |
| Partner pack manifest | `scripts/partner_packs/hermez_usdcbridge_vk_for_alina.manifest` | rebuild via `scripts/build_partner_pack.sh` |

---

## 4. Dependency pins (gosh graph)

| Crate / lib | Correct repo | Branch |
|-------------|--------------|--------|
| halo2-base / halo2-ecc | https://github.com/gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381 | `bump-halo2-lib-v0.4.1` |
| gosh-zk-snark-halo2-utils | https://github.com/gosh-sh/gosh-zk-snark-halo2-utils | `main` |
| gosh-halo2-crypto-lib | https://github.com/gosh-sh/gosh-halo2-crypto-lib | `bump-halo2-lib-v0.4.1` |
| AN→ETH circuits | https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits | as Cargo-pinned (`circuit4-single-final-root` for C4) |
| AN→ETH prover | https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-prover | `main` / live n14 tip |

---

## 5. Do **not** use for Hermez deposit / 1B WithVK

| Wrong | Why |
|-------|-----|
| Chain SRS (`s_g2 = c6028acf…`) | Opcode rejects |
| Deposit VkBlob `20cf9018…647a39` | Pre-Hermez (still on `3.0.4.an-rc` / older shellnet) |
| Circuit 1B fixtures 6308 B / K=20 (`2b3850aa…`) | Pre-Hermez |
| Soft chain / `gen_srs` fallbacks | Removed on `pruvendo/hermez-kzg-fixtures` |
| GitHub SSH **`:22`** from this network | Filtered — use `ssh://git@ssh.github.com:443/` |

---

## 6. Quick clone map

```text
github.com/tvmlabs/tvm-sdk.git
  └─ pruvendo/hermez-deposit-fixtures @ 915e6998   # Hermez deposit + 1B fixtures (use this)

github.com/gosh-sh/bridge.git                      # acki-nacki-bridge
  └─ pruvendo/hermez-kzg-fixtures @ 903efff        # #10
  └─ pruvendo/shellnet-e2e-landing                 # #5 parent

github.com/gosh-sh/acki-nacki.git
  └─ pruvendo/hermez-deposit-vk-blob @ 0d2f7a063   # USDCBridge VK_BLOB
```
