# TD-41 — Mainnet `chainId=1` rejected by design (DEP-MAINNET-ID)

PoC: `deposit-chain-ids/tests/td_41_mainnet_chain_id.rs`, `td_41_mainnet_reject.rs`.  
Cross-ref: TD-30 (`td_30_unsupported_chain_id_fails_preflight`), TD-16 (prod profile gates).

## Policy

Deposit bridge accepts deposits from **six L2 mainnets + Sepolia** (testnet/shellnet).  
Ethereum L1 (`chainId = 1`) is **not** in `SUPPORTED_DEPOSIT_CHAIN_IDS` or `PRODUCTION_DEPOSIT_CHAIN_IDS`.

Startup path: `run_daemon` → `ensure_supported_rpc_chain` → `ensure_supported_chain_id_value` **before** relayer tick.

## Verdict: **partial QC / OK by design**

Reject at preflight — no prove/mint path for L1 mainnet. Not BC.

## Commands

    cd crates/deposit-chain-ids && cargo test td_41 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_41 -- --nocapture
    cd crates/deposit-relayer-daemon && cargo test td_30 -- --nocapture
    ./scripts/td_41_no_mainnet_chain.sh
