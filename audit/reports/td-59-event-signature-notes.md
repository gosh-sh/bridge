# TD-59 — Event signature / indexed `depositId` regression (log scan)

PoC:
- `audit/spec/ethereum/DepositEventSignature.t.sol`
- `crates/deposit-relayer-daemon/tests/td_59_log_scan_signature.rs`
- `deposit-prover/tests/td_59_event_signature_regression.rs`

## Topic layout (`Deposit` event)

| Index | Field | Source |
|-------|-------|--------|
| topic0 | event signature | `keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)")` |
| topic1 | `depositId` (indexed) | BE `uint256` padded to 32 bytes |
| topic2 | `sender` (indexed) | address left-padded to 32 bytes |
| data | `amount`, `anWorkchain`, `anAccount`, `timestamp` | non-indexed ABI words |

Canonical topic0: `0x8d5d060673b27fac84d56ee262fe8dccad60d198ae11766063f112a9be3d37ee`

## Cross-refs

| ID | Link |
|----|------|
| TD-10 | Multi-log receipt; `log_index` vs topic0/topic1 binding |
| TD-01 | `sender` in topic2; relayer bind |
| Relayer | `EthLogSource::fetch` — `Deposit::SIGNATURE_HASH` + `topic1 = depositId` BE |
| Prover | `get_deposit_event_signature()` in `ethereum_fetcher` + `circuit_v2` |

## Verdict: **META/OK**

L1 emit, relayer filter, prover/circuit signature byte-identical; wrong signature excluded from scan.

## Commands

    cd audit/spec/ethereum && forge test --match-path '*EventSignature*' -q
    cd crates/deposit-relayer-daemon && cargo test td_59 -- --nocapture
    cd deposit-prover && cargo test td_59 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_10 -- --nocapture
