# TD-16 — Sepolia vs production deposit allowlist

PoC: `deposit-chain-ids` prod/testnet split, `scripts/td_16_prod_no_sepolia.sh`, `td_16_sepolia_prod_allowlist.rs`.

## Policy (DEP-T16 / Opus D-05)

| Set | Chain IDs | Use |
|-----|-----------|-----|
| `PRODUCTION_DEPOSIT_CHAIN_IDS` | 6 L2 mainnets (no Sepolia) | prod `PROFILE` |
| `TESTNET_ONLY_DEPOSIT_CHAIN_IDS` | `11155111` (Sepolia) | shellnet/dev only |
| `SUPPORTED_DEPOSIT_CHAIN_IDS` | 6 L2 + Sepolia | prover/relayer code allowlist |

**Invariant:** `prod ∩ testnet_only = ∅` (enforced in crate tests + preflight gate).

## Verdict: **OK** (ops policy PoC)

1. Sepolia remains in code allowlist for shellnet/fixtures — **QC by design** (not BC in `.sol`).
2. `PROFILE=prod` + `CHAIN_ID=11155111` → **fail** in `td_16_prod_no_sepolia.sh`.
3. `PROFILE=shellnet` + Sepolia → **pass**.
4. Live AN `setExpectedBridge(11155111, …)` on production USDCBridge would be **QC ops misconfig** — gate does not fix AN, documents risk; TD-04 patch context.

**BC** if production runbook required Sepolia mint without ops gate — not observed; preflight now blocks explicit prod+Sepolia combo.

## CREATE2 / same bridge address

Sepolia on prod profile is separate from TD-15 chainId collision; together: prod must use L2-only allowlist AND DEP-N-5 voucher keys.

## Commands

    bash scripts/production_preflight.sh --help
    CHAIN_ID=11155111 PROFILE=prod ./scripts/td_16_prod_no_sepolia.sh
    CHAIN_ID=11155111 PROFILE=shellnet ./scripts/td_16_prod_no_sepolia.sh
    cd crates/deposit-relayer-daemon && cargo test td_16 -- --nocapture
