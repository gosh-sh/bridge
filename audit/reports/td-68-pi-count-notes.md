# TD-68 — `NUM_PUBLIC_INPUTS == 12` CI gate vs docs drift

PoC: `scripts/check_pi_count_docs.sh`, `td_68_pi_count_gate.rs`.

Cross-ref: TD-02 (layout drift), DEP-PI-COUNT.

## Canonical 12-slot layout

| Slot | Label | Source |
|------|-------|--------|
| 0 | `depositId` | event topic |
| 1 | `sender` | event topic |
| 2 | `amount` | log data |
| 3 | `contractAddress` | log address |
| 4 | `chainId` | enclosing EIP-1559 tx RLP |
| 5 | `dappIdHigh` | witness config |
| 6 | `dappIdLow` | witness config |
| 7 | `anAccountHigh` | log data |
| 8 | `anAccountLow` | log data |
| 9 | `blockHashHigh` | `keccak256(header_rlp)` |
| 10 | `blockHashLow` | `keccak256(header_rlp)` |
| 11 | `promiseCommit` | circuit (`EthCircuitImpl`) |

Code: `deposit-prover/src/circuit_v2.rs` `DEPOSIT_PUBLIC_INPUT_LAYOUT`; relayer `NUM_PUBLIC_INPUTS = 12`; operand `PUBLIC_INPUT_BYTES = 384`.

## Stale 11-PI sources (fixed or allowlisted)

| Location | Before | After / note |
|----------|--------|----------------|
| `audit/PROJECT_FACTS.md` | Deposit **11** | **12** + slot list |
| `evm_an_deposit_e2e_runbook.md` | 11 PI, no `chainId` | 12 PI table |
| `zk_halo2_an_side_design.md` | 11 PI list | 12 PI + slots 4/11 |
| `audit/spec/an/AGENT_CONTEXT.md` | deposit 11 PI | 12 PI |
| `shellnet_usdcbridge_deposit_vk_redeploy.md` | historical 11-PI blocks | allowlisted (superseded banner) |
| `audit overlay USDCBridge.sol` | legacy 8-Fr parser | allowlisted (TD-04) |
| `_archive/` | various | not scanned |

## CI gate behaviour

`scripts/check_pi_count_docs.sh`:

1. **Fail** — canonical paths contain bare `11 public input`, `NUM_PUBLIC_INPUTS = 11`, `11-PI`, `11 × 32`, etc. without legacy/stale context.
2. **Pass** — `PROJECT_FACTS` Deposit row `**12**`; no stale patterns in canonical files.
3. Prints `file:line` on failure.

Hook: `scripts/ci_an_audit.sh`, `make audit-deposit-relayer-test`.

`td_68_pi_count_gate.rs`: relayer `NUM_PUBLIC_INPUTS` / `PUBLIC_INPUT_BYTES`; deposit-prover source pins; `PROJECT_FACTS` row; script subprocess.

## Verdict: **META / QC**

Docs drift (11 vs 12) was operational risk, not a soundness BC. Gate prevents regression; AN overlay may still document legacy parser until deploy (TD-04).

## Commands

    bash scripts/check_pi_count_docs.sh
    cd crates/deposit-relayer-daemon && cargo test td_68 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_02 -- --nocapture
    cd deposit-prover && cargo test f10a_binding -- --nocapture
