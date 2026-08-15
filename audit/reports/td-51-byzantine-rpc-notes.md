# TD-51 — Byzantine RPC inconsistent responses per tick

PoC: `tests/td_51_byzantine_rpc.rs`, `source.rs` helpers `test_td51_*`.

## RPC call → cache → Byzantine scenario → relayer behaviour

| RPC call | Cached? | Byzantine scenario | Relayer behaviour |
|----------|---------|-------------------|-------------------|
| `eth_blockNumber` (`EthLogSource::fetch`) | no (per tick) | Shrinking head between ticks | **stall** `Ok(None)` → `NotYetAvailable`; cursor not advanced past finalized deposits |
| `eth_getLogs` + receipt (`fetch`) | no | Log at block above shrunk `safe_head` | **stall** — deposit not surfaced (same as honest RPC) |
| `eth_chainId` (`resolve_chain_id`) | **yes** (first fetch) | Post-startup chainId lie | **QC** — stale cache; startup TD-30 preflight only |
| `eth_chainId` (`fetch_deposit_from_receipt`) | no (per call) | Flip vs cached source | **fail-closed** if event/prover disagree (`check_binds_to`) |
| `get_transaction_receipt` (`fetch`) | no | Receipt `block_hash` ≠ getLogs `block_hash` | **fail-closed** `Err` at `check_binds_to` (blockHash mismatch) |
| `eth_chainId` (prover subprocess) | n/a | Witness RPC ≠ source RPC | **fail-closed** bind or TD-30 startup `Err` |

## Tick outcomes (PoC)

| Case | Tick outcome | `last_processed_deposit_id` |
|------|--------------|----------------------------|
| (a) Shrinking head pre-finalize | `NotYetAvailable` → later `Finalized` | unchanged until `Finalized` |
| (a) Shrinking head post-dep-0 | `NotYetAvailable` for dep+1 | stays at last good id |
| (b) ChainId split-brain (source vs prover) | `Err` (bind) | no advance |
| (b) Sequential events same id, flip chain | bind unit **reject** | n/a |
| (c) BlockHash getLogs vs receipt | `Err` (bind) | no advance |
| (d) Control consistent | `Finalized` | advanced |

## Cross-refs

| TD | Link |
|----|------|
| TD-30 | Startup dual-RPC `eth_chainId` preflight (`rpc_preflight.rs`) |
| TD-39 | Incremental scan / per-tick `get_block_number` (`td_39_incremental_scan_cost.rs`) |
| TD-08 | `check_binds_to` chainId bind (`td_08_dual_rpc_chain_id.rs`) |
| QC-OFF | Ops assumption: honest RPC; Byzantine = META/QC surface |

## Verdict: **META/QC** (not BC)

No silent accept on inconsistent RPC within PoC scope. Inconsistent PI vs event fails at `check_binds_to` before AN submit. Shrinking head stalls like honest finality gate. Stale `resolve_chain_id` cache is documented QC (TD-30 closes startup split-brain only).

## Commands

    cd crates/deposit-relayer-daemon && cargo test td_51 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_30 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_39_incremental_scan_cost -- --nocapture
