# TD-64 — Seven-chain deposit header corpus

PoC: `deposit-prover/tests/td_64_seven_chain_corpus.rs`, `CHAIN_HEADER_CORPUS` in `rlp_utils.rs`.

## Corpus table (7/7)

| chainId | Name | Fixture | Fields | Hash prefix |
|---------|------|---------|--------|-------------|
| 10 | optimism | `op_mainnet.json` | 21 | `0x5758ca91…` |
| 480 | world_chain | `world_chain_mainnet.json` | 21 | `0x3efb9b43…` |
| 5000 | mantle | `mantle_mainnet.json` | 21 | `0x7157cb53…` |
| 8453 | base | `base_mainnet.json` | 21 | `0x33461303…` |
| 42161 | arbitrum_one | `arbitrum_one.json` | 16 | `0x2e486de0…` |
| 81457 | blast | `blast_mainnet.json` | 20 | `0x9941541d…` |
| 11155111 | sepolia | `sepolia_prague.json` | 21 | `0x34fbf817…` |

Pinned via public RPC `eth_getBlockByNumber("latest")` at corpus import (2026-08-14).

## Legacy shape (not deposit allowlist)

| Fixture | Role |
|---------|------|
| `mainnet_shanghai.json` | TD-32 / TD-54 shape ref (Ethereum mainnet, TD-41 not deposit chain) |
| `op_ecotone.json`, `op_isthmus.json` | Historical OP upgrades in `HEADER_SHAPE_SAMPLES` |

## Cross-refs

| ID | Link |
|----|------|
| TD-32 | Field count matrix 16–21, 717B envelope |
| TD-41 | Mainnet chainId=1 not in deposit allowlist |
| TD-54 | `HEADER_SHAPE_SAMPLES` parity |

## Verdict: **META/QC**

Each `SUPPORTED_DEPOSIT_CHAIN_IDS` entry has a real-block fixture; `verify_block_header_rlp` green for all.

## Commands

    cd deposit-prover && cargo test td_64 -- --nocapture
    cd deposit-prover && cargo test td_32_l2_header_matrix -- --nocapture
    bash scripts/check_deposit_header_corpus.sh
