# Acki Nacki Bridge — User Guide

**Move test USDC between Ethereum (Sepolia) and Acki Nacki.**

*Revision: July 2026 · Sepolia testnet · test tokens only, no real value*

---

This bridge moves test **USDC** in both directions:

- **Deposit — Ethereum → Acki Nacki.** You send USDC on Sepolia; a relayer proves
  it and credits your Acki Nacki account. **You do this yourself in MetaMask.**
- **Withdraw — Acki Nacki → Ethereum.** You start a withdrawal on Acki Nacki; a
  relayer proves it and pays USDC to your Ethereum address. **Operator-assisted
  today** (a relayer submits the proofs for you).

Both directions have been run end-to-end on the live testnet — see
[Section 5](#5-proof-it-works-latest-live-run).

---

## 1. What you need

1. **MetaMask** set to **Sepolia** (chain id `11155111`). Enable test networks in
   MetaMask settings if Sepolia is hidden.
2. **Sepolia ETH** for gas — any public "Sepolia faucet".
3. **Test USDC** on Sepolia — mint from the
   [Aave Sepolia faucet](https://app.aave.com/faucet/) (connect MetaMask, pick
   Sepolia, mint USDC).
4. **An Acki Nacki account id** (to receive deposits, or to send from when
   withdrawing). This is **not** your MetaMask address — your operator gives it
   to you (64 hex characters).

---

## 2. Addresses (Sepolia)

| Item | Address |
| --- | --- |
| **Bridge** (`AckiNackiBridge`) | `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82` |
| **USDC** (6 decimals) | `0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8` |
| **Aave faucet** (mint test USDC) | `0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D` |
| **Explorer** | [sepolia.etherscan.io](https://sepolia.etherscan.io) |

**USDC uses 6 decimals** (not 18). Amounts you type on Etherscan:

| Amount | Enter as |
| --- | --- |
| 1 USDC | `1000000` |
| 10 USDC | `10000000` |
| 100 USDC (max/deposit) | `100000000` |

Confirm addresses with your operator — test deployments can change.

---

## 3. Deposit: Ethereum → Acki Nacki

Two MetaMask transactions on Etherscan: **approve**, then **deposit**.

**Step 1 — Approve.** Open USDC →
[Write Contract](https://sepolia.etherscan.io/address/0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8#writeContract)
→ **Connect to Web3** → `approve`:
- `spender` = bridge `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82`
- `amount` = at least what you'll deposit (e.g. `1000000` for 1 USDC)

**Step 2 — Deposit.** Open the bridge →
[Write Contract](https://sepolia.etherscan.io/address/0x99c37fb75326ae6953ebbbdcd261ec331df4ce82#writeContract)
→ `deposit`:

| Field | Value |
| --- | --- |
| `amount` | USDC base units, e.g. `1000000` for 1 USDC (≤ what you approved) |
| `anWorkchain` | `0` (ask operator if unsure) |
| `anAccount` | Your Acki Nacki account as **bytes32**: 64 hex chars, `0x`-prefixed. Must **not** be all zeros. Any valid account works, including full-width ids like `0x20c2db9c…834c9`. |

Confirm in MetaMask and **save the transaction hash**.

**Then wait.** The relayer scans your `Deposit` event, builds a zero-knowledge
proof (**minutes, not seconds**), and calls `finalizeDeposit` on Acki Nacki. Your
USDC appears on your AN account. You don't run anything.

### What you'll see

**On Etherscan** — a successful `deposit` emits exactly one `Deposit` event.
Write down your **depositId** (you'll need it if you ever contact the operator).
A real Sepolia deposit looks like this:

| Field | Example |
| --- | --- |
| tx status | Success (1) |
| gas used | ~58,600 |
| `depositId` (indexed) | `9` |
| `sender` (indexed) | `0x4A35…7592` |
| `amount` | `1000000` (1 USDC) |
| `anWorkchain` | `0` |
| `anAccount` | `0x20c2db9c…834c9` |

Event signature `Deposit(uint256,address,uint256,int8,bytes32,uint256)` =
`0x8d5d0606…3d37ee`.

**On Acki Nacki** (relayer-driven, a few minutes later) — the relayer proves the
deposit and calls `finalizeDeposit`. On success the AN transaction has
`exit_code 0`, a `DepositVoucher` is deployed, and `confirmDeposit` mints ECC
currency **#3 (USDC)** to your account. You then see the balance on your AN
account, e.g. `ecc{3:1000000}` = 1 USDC (last fully verified credit:
depositId=9 → `0x20c2db9c…834c9`, AN tx `e217cc56…`, `exit_code 0`).

> If your USDC hasn't arrived after several minutes, the AN side may be mid-
> maintenance (e.g. a testnet reset temporarily invalidates the relayer's AN
> account). Send your **tx hash + depositId** to the operator — the ETH-side
> deposit is already final and will be credited once the relayer's AN account is
> restored.

---

## 4. Withdraw: Acki Nacki → Ethereum

1. Start a withdrawal on the **Acki Nacki** side, specifying the **Ethereum
   address** that should receive the USDC (ask your operator for the exact step).
2. The relayer proves the Acki Nacki block state on Ethereum (`verifyBlock`) and
   then pays out (`withdrawByProof`) — **automatically, no MetaMask action from
   you.**
3. USDC arrives at your Ethereum address. Each withdrawal can be paid **once**
   (replay-protected).

This direction is **operator-assisted** on testnet today — the relayer submits
the proofs. Tell your operator you want a withdrawal.

### What you'll see

The relayer submits two Sepolia transactions on your behalf (no MetaMask action
from you):

| Step | What it does | Example tx |
| --- | --- | --- |
| `verifyBlock` | Registers the Acki Nacki block anchor on the bridge | `0x0f54a486…` |
| `withdrawByProof` | Pays your USDC to your Ethereum address (once) | `0xdf01a367…` |

When `withdrawByProof` succeeds you'll see a USDC `Transfer` to your address on
Etherscan and the funds in your wallet. A second attempt on the same withdrawal
reverts — each withdrawal is replay-protected (nullifier).

> Behind the scenes the Acki-Nacki-side proof (`proof_event_*.json`, Circuit 4)
> is produced by the partner prover and self-verifies (`"verified": true`) before
> the relayer touches Ethereum. Because `verifyBlock` enforces a **monotonic**
> Acki Nacki block seq_no, the on-chain payout requires the bridge's stored
> `storedLastSeenBlockSeqNo` to be at or below the proof's block — a fresh bridge
> deployment is needed after an Acki Nacki testnet reset.

---

## 5. Proof it works (latest live run)

Both directions, run end-to-end on **2026-07-03** (1 USDC each), fully
relayer-driven (the ETH-side steps are automatic — no manual submission):

| Direction | Evidence |
| --- | --- |
| **Deposit** (ETH→AN) | Sepolia `deposit` id=8 tx [`0x2b3781ef…`](https://sepolia.etherscan.io/tx/0x2b3781efccade0afdff0a7b531227c3ee0df6c3397d9ba2cf5ea8fec9e8f88b8) → relayer proved it → `finalizeDeposit` **accepted on Acki Nacki** (1 USDC credited to `0x20c2db9c…834c9`) |
| **Withdraw** (AN→ETH) | relayer `verifyBlock` tx [`0x502b7648…`](https://sepolia.etherscan.io/tx/0x502b7648eaed23b365beb00e28b756690acc0d08825b33dd8e5364d2fd1c2ca8) (registers the AN block anchor) → relayer payout `withdrawByProof` tx [`0xb6e351c8…`](https://sepolia.etherscan.io/tx/0xb6e351c8086674ccf38f0b79946974f8aa3824d7a8d6b18b8e909c7e0cb18dcb) (1 USDC paid to the recipient's Ethereum address) |

> The **deposit** run above is fully fresh, including the zero-knowledge proving.
> The **withdraw** run exercises the relayer's Ethereum automation end-to-end
> (fresh `verifyBlock` + `withdrawByProof`, real payout) on an Acki-Nacki-side
> withdrawal proof produced earlier by the partner prover; originating a brand-new
> withdrawal on Acki Nacki and re-proving it is a partner-prover step.

---

## 6. Troubleshooting (deposit)

| Symptom | Fix |
| --- | --- |
| Out of gas / "insufficient funds" | You need **Sepolia ETH** (separate from USDC). |
| `transferFrom` failed / revert | You skipped **approve** or approved too little. Redo Step 1. |
| `InvalidAmount` | `amount` was `0`. Enter a positive number. |
| `InvalidAnAccount` | `anAccount` was zero or malformed. Use a non-zero 64-hex bytes32. |
| `DepositTooLarge` | Max is 100 USDC (`100000000`) per deposit. |
| Deposit reverts, no token error | Check amount limits, allowance, and `anAccount != 0`. |
| USDC not on Acki Nacki yet | Proving takes minutes. If still missing, send your **tx hash** + **depositId** (from the `Deposit` event log) to the operator. |

**Safety:** Sepolia only. Confirm the bridge address before approving. Never
share your MetaMask secret recovery phrase. Test USDC is not real money.

---

## 7. For developers

| Task | Where |
| --- | --- |
| Deposit relayer (ETH→AN: listen → prove → submit) | `crates/deposit-relayer-daemon/` — `deposit-relayer watch` / `prove-one` / `finalize-one` / `daemon` |
| Withdraw relayer (AN→ETH: verifyBlock + payout) | `crates/bridge-relayer-daemon/` — `relayer submit-verify-block` / `daemon-prover` / `daemon-withdraw` |
| Latest AN→ETH daemon E2E | `docs/an_eth_daemon_withdraw_e2e_2026-07-03.md` |
| Shellnet / VK redeploy checklist | `docs/shellnet_usdcbridge_deposit_vk_redeploy.md` |
| 12 deposit public inputs (Track-2 chain-binding, 2026-07-23) | `crates/deposit-relayer-daemon/src/types.rs`, `deposit-prover/src/types.rs` (`NUM_PUBLIC_INPUTS = 12`) |
| Deploy scripts | `contracts/ethereum/script/` |
| Env template (hosted relayer) | `scripts/ursus/deposit-relayer.env.example` |
| Hermez KZG pins (repos/branches/blobs) | `docs/hermez_kzg_repos_and_branches.md` |

> **Deposit prover must key on the Hermez ceremony.** Shellnet's `USDCBridge`
> currently embeds the Hermez deposit `VkBlob` (`304c1c4e…`, 3982 B, **11 PI** —
> pre-Track-2) and the AN node opcode embeds Hermez `s_g2` (`928fafb3…`). A proof
> keyed on the old chain SRS (`VkBlob 20cf9018…`) is rejected on-chain with
> `ERR_INVALID_ZKPROOF` (TVM `exit_code 220`). Build the prover from the Hermez
> branch (`pruvendo/hermez-kzg-fixtures`) with `deposit-prover/data/kzg_params_18.srs`
> (Hermez), delete any stale PK so keygen re-writes the `…pk.bp.json` sidecar, and
> confirm `vk_blob.bin` matches the shellnet-deployed hash before submitting.
>
> **Track-2 follow-up (2026-07-23):** the current `deposit-prover` circuit exposes
> **12 PI** (adds `chainId` at slot 4) and produces VkBlob `7322fb82…93f92541`
> (5006 B, rotated 2026-07-30 by the Prague header fix; the earlier 12-PI blobs
> `de1dd3ab…7dd8d1` / `006cca5d…191dec05` / `3e2a2db2…bf0d049c` are superseded —
> same PI layout, different constraint system).
> This blob is **not yet redeployed to shellnet** — the on-chain
> `USDCBridge.VK_BLOB` is still the 11-PI `304c1c4e…`. Track the redeploy in
> `docs/shellnet_usdcbridge_deposit_vk_redeploy.md`.

**Verify the bridge on Etherscan** (run from `contracts/ethereum/`, which pins
solc 0.8.19 / optimizer / `via_ir`):

```bash
forge verify-contract --chain 11155111 --watch --guess-constructor-args \
  --etherscan-api-key "$ETHERSCAN_API_KEY" \
  0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 \
  src/AckiNackiBridge.sol:AckiNackiBridge
```

The Yul aggregator verifiers are raw bytecode (`contracts/ethereum/verifiers/*.bin`);
publish their provenance from `contracts/ethereum/verifiers/README.md` instead of
a source match.

---

*Confirm network and addresses with the operator before each test campaign.*
