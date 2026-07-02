# Acki Nacki Bridge — User Guide

**Move test USDC from Ethereum (Sepolia) to Acki Nacki.**

*Revision: July 2026 · Sepolia testnet*

---

> This guide is for **testers and reviewers** who want to make a deposit with
> **MetaMask only** — no custom website, no command line. You interact with the
> bridge **on [Sepolia Etherscan](https://sepolia.etherscan.io)** and sign
> transactions in MetaMask.

Developers: see [Section 8](#for-developers).

---

## 1. What this bridge does

The bridge connects two blockchains:

- **Ethereum Sepolia** — where you send test **USDC**.
- **Acki Nacki** — where matching funds can appear after an automated relayer
  proves your deposit.

Flow in plain terms:

1. You **approve** then **deposit** USDC into the bridge contract on Sepolia.
2. A **relayer** watches for your `Deposit` event, builds a zero-knowledge proof,
   and submits it to Acki Nacki (`finalizeDeposit`).

Only **Ethereum → Acki Nacki** is supported in this test setup. The reverse
direction is still in development.

---

## 2. Before you start

You need:

1. **MetaMask** on **Sepolia** ([metamask.io](https://metamask.io)). Enable test
   networks in MetaMask settings if Sepolia is hidden.
2. **Test ETH** on Sepolia (for gas) — any public “Sepolia faucet”.
3. **Test USDC** on Sepolia — mint from the [Aave Sepolia faucet](https://app.aave.com/faucet/)
   (connect MetaMask, select Sepolia, mint USDC).
4. An **Acki Nacki recipient account** (where funds should land on the AN side).
   This is **not** your MetaMask address. Use the account id your operator or
   wallet gives you (64 hex characters, optionally with a `0x` prefix).

> Test tokens have **no real value**. Do not use mainnet Ethereum or real USDC.

---

## 3. Reference addresses (Sepolia)

Confirm these with your operator before depositing — test deployments can change.

| Item | Address |
| --- | --- |
| **Bridge** (`AckiNackiBridge`) | `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82` |
| **USDC** (6 decimals) | `0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8` |
| **Aave faucet** (mint test USDC) | `0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D` |
| **Block explorer** | [sepolia.etherscan.io](https://sepolia.etherscan.io) |

**Limits:** minimum deposit > 0; maximum **100 USDC** per deposit.

**USDC amounts on Etherscan** use **6 decimal places** (not 18 like ETH):

| You want to deposit | Enter as `amount` (uint256) |
| --- | --- |
| 1 USDC | `1000000` |
| 10 USDC | `10000000` |
| 100 USDC | `100000000` |

---

## 4. Deposit step by step (MetaMask + Etherscan)

You will make **two** transactions: **approve**, then **deposit**.

### Step 1 — Connect MetaMask to Etherscan

1. Open [Sepolia Etherscan](https://sepolia.etherscan.io).
2. Click **Connect Wallet** (top right) and choose MetaMask.
3. Ensure MetaMask is on **Sepolia** (chain id `11155111`).

### Step 2 — Approve USDC for the bridge

1. Open the USDC contract:
   [0x94a9…E4C8 on Sepolia](https://sepolia.etherscan.io/address/0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8#writeContract).
2. Go to the **Contract** tab → **Write Contract** → **Connect to Web3**.
3. Find **`approve`** and fill in:
   - **`spender`**: `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82` (bridge address)
   - **`amount`**: at least the USDC base units you plan to deposit (see table
     above), e.g. `10000000` for 10 USDC. On a testnet you may instead approve
     the maximum `uint256` value once so repeat deposits do not need another
     approve transaction.
4. Click **Write**, confirm in MetaMask, wait until the transaction succeeds.

This only **allows** the bridge to pull USDC; it does not deposit yet.

### Step 3 — Call `deposit` on the bridge

1. Open the bridge contract:
   [0x99c37…e82 on Sepolia](https://sepolia.etherscan.io/address/0x99c37fb75326ae6953ebbbdcd261ec331df4ce82#writeContract).
2. **Contract** → **Write Contract** → connect MetaMask if needed.
3. Find **`deposit`** and fill in:

   | Field | What to enter |
   | --- | --- |
   | **`amount`** | USDC base units (6 decimals), e.g. `10000000` for 10 USDC |
   | **`anWorkchain`** | Usually `0` (ask your operator if unsure) |
   | **`anAccount`** | Your Acki Nacki account as **bytes32**: exactly **64 hex
   characters** (32 bytes), with optional `0x` prefix. If your account id is
   shorter, pad with leading zeros on the left. Example:
   `0x00000000000000000000000000000000000000000000000000000000000000ab` for
   account byte `0xab`. **Must not be all zeros.** |

   > **✅ Full 256-bit recipients supported.** You can use any valid Acki Nacki
   > account id, including a real full-width multisig address that starts with
   > non-zero bytes (e.g. `0x20c2db9c…`). This was verified end-to-end on
   > 2026-07-02: a 1 USDC Sepolia deposit to the full-width account
   > `0x20c2db9c…834c9` was proved and credited on Acki Nacki. (An earlier
   > shellnet build required the high 16 bytes to be zero; that limitation was
   > fixed by `acki-nacki` #2271 and is no longer in effect.)

   The **`amount`** here must be less than or equal to the USDC you approved in Step 2.

4. Click **Write**, confirm in MetaMask, wait for confirmation.

You are done on Ethereum. Save the **transaction hash** from Etherscan or
MetaMask — the relayer and support staff use it to trace your deposit.

### Step 4 — Confirm the event (optional)

Open your deposit **transaction** on Etherscan (from MetaMask or the bridge
**Transactions** tab) and check **Logs**. You should see a **`Deposit`** event
with `depositId`, `sender`, `amount`, `anWorkchain`, `anAccount`, and
`timestamp`. Note the **`depositId`** — the relayer uses it as the cursor when
processing deposits.

---

## 5. What happens after you deposit

You do not need to run anything yourself:

1. The **deposit relayer** scans Sepolia for `Deposit` events.
2. It generates a **Halo2 proof** binding your deposit to the block, bridge
   address, amount, and Acki Nacki recipient.
3. It calls **`finalizeDeposit`** on Acki Nacki so test USDC can be credited on
   your AN account (when the operator’s relayer and AN contract are configured).

**Timing:** proof generation takes **minutes**, not seconds.

**Testnet status (July 2026):** live crediting on shellnet **works end-to-end**,
including **full 256-bit recipients**. A real 1 USDC Sepolia deposit to the
full-width account `0x20c2db9c…834c9` was proved and credited on Acki Nacki on
2026-07-02 (AN `finalizeDeposit` tx `cc8dfff5…`; recipient credited ECC
currency #3 = `1000000`). The AN-side `USDCBridge` is deployed with the correct
11-public-input verifying key and the recipient-parsing fix (`acki-nacki` #2271),
so there is no longer any restriction on the shape of your `anAccount`.
Technical details: `docs/shellnet_usdcbridge_deposit_vk_redeploy.md`.

Your Ethereum deposit is always valid and visible on Etherscan regardless.

---

## 6. Troubleshooting

**“Insufficient funds” / out of gas**  
You need **Sepolia ETH** for gas (separate from USDC).

**`transferFrom` failed / deposit reverts**  
You skipped **approve**, approved too little USDC, or have insufficient USDC
balance. Repeat Step 2 with a large enough `amount`.

**`InvalidAmount`**  
`amount` was `0`. Enter a positive USDC base-unit amount (see the table in
[Section 3](#reference-addresses-sepolia)).

**`InvalidAnAccount`**  
`anAccount` was zero or wrong format. Use a non-zero 256-bit Acki Nacki account
id (64 hex chars as bytes32).

**`DepositTooLarge`**  
Maximum is 100 USDC per deposit (`100000000` base units).

**Deposit reverts with no clear token error / bridge paused**  
On the bridge **Read Contract** tab, check **`paused`**. If `true`, deposits are
disabled until the operator unpauses the bridge.

**Wrong network**  
All steps must be on **Sepolia**, not Ethereum mainnet.

**Funds not on Acki Nacki yet**  
Wait several minutes (proof generation takes minutes). If still missing, send
your **deposit transaction hash** and **depositId** (from the event) to the
operator. The relayer or AN contract may be mid-upgrade.

**Deposit succeeded on Sepolia but never arrives on Acki Nacki**  
Proof generation takes minutes, so first allow a few minutes plus the async
Acki Nacki credit hops. If it still hasn't arrived, send your **deposit
transaction hash** and **depositId** to the operator — the relayer or AN
contract may be mid-upgrade. (The old "high 16 bytes must be zero" recipient
limitation was fixed by `acki-nacki` #2271 and no longer applies.)

**Can I withdraw back to Ethereum?**  
Not as a self-serve MetaMask path yet. The reverse **AN → ETH** direction does
exist and has been demonstrated on Sepolia (the bridge proves Acki Nacki block
state via `verifyBlock`, then pays out via `withdrawByProof`), but it is
**operator-driven** (a relayer submits the proofs) and currently uses
integration-grade verifiers — not a production-safe, user-initiated flow. Ask
your operator if you need a testnet withdrawal.

---

## 7. Safety reminders

- Use **Sepolia** only; confirm the bridge address before approving USDC.
- Never share your MetaMask **secret recovery phrase**.
- Test USDC is not real money.

---

## 8. For developers

| Task | Where to look |
| --- | --- |
| Deposit relayer (ETH→AN: listen → prove → submit) | `crates/deposit-relayer-daemon/` — `deposit-relayer watch`, `prove-one`, `finalize-one`, `daemon` |
| Reverse relayer (AN→ETH: `verifyBlock` + `withdrawByProof`) | `crates/bridge-relayer-daemon/` — `relayer submit-verify-block`, `submit-withdraw`, `daemon-prover`; wiring: `docs/shellnet_an_eth_relayer_wiring.md` |
| Shellnet / VK redeploy checklist | `docs/shellnet_usdcbridge_deposit_vk_redeploy.md` |
| 11 deposit public inputs | `crates/deposit-relayer-daemon/src/types.rs` |
| Optional local UI (not required for testing) | `frontend/` — `trunk serve` → `http://localhost:8080` |
| `cast` / Foundry scripts | `contracts/ethereum/script/DeployTestBridge.s.sol` |
| Verify deployed contract on Etherscan | see below |

Example env template for a hosted relayer:
`scripts/ursus/deposit-relayer.env.example`.

### Verify & Publish the bridge contract on Etherscan

Source-verification publishes the Solidity that produced the deployed bytecode,
so the contract is readable on Etherscan and its Read/Write tabs work. Two
contract kinds verify differently:

- `AckiNackiBridge` + mocks — normal Solidity verification (below).
- The SHPLONK aggregator verifiers (`PrimaryAggregatorVerifier`,
  `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier`,
  `BridgeWithdrawalAggregatorVerifier`) are **raw Yul bytecode** deployed from
  `contracts/ethereum/verifiers/*.bin` via `ShplonkDeployLib`, so there is no
  Solidity source to submit — publish their provenance
  (`contracts/ethereum/verifiers/README.md`: `.bin` hash + snark-verifier rev +
  regen command) instead of a source match.

**Prerequisites**

- An Etherscan **V2** API key (one key works across chains):
  `export ETHERSCAN_API_KEY=…` (create at <https://etherscan.io/myapikey>).
- The deployed address + chain id (Sepolia = `11155111`, mainnet = `1`).
- Run `forge` **from `contracts/ethereum/`** so it reads `foundry.toml` and
  submits the exact settings the deployment used:
  **solc 0.8.19, optimizer on, `optimizer_runs = 1`, `via_ir = true`**. A
  settings mismatch is the #1 cause of "bytecode does NOT match".

**Option A — verify at deploy time (simplest):** add `--verify` to the deploy run.

```bash
cd contracts/ethereum
forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge \
  --rpc-url "$SEPOLIA_RPC_URL" --private-key "$DEPLOYER_PK" \
  --broadcast --verify --etherscan-api-key "$ETHERSCAN_API_KEY" -vvvv
```

**Option B — verify an already-deployed contract.** `AckiNackiBridge` has a
struct-heavy constructor, so let Foundry recover the args from on-chain creation
code. The address below is this guide's deposit bridge
(`0x99c37f…ce82`); substitute whichever deployment you are verifying:

```bash
cd contracts/ethereum
forge verify-contract --chain 11155111 --watch --guess-constructor-args \
  --etherscan-api-key "$ETHERSCAN_API_KEY" \
  0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 \
  src/AckiNackiBridge.sol:AckiNackiBridge
```

Other ways to get the constructor args: read them from
`broadcast/<script>/<chainId>/run-latest.json`, or ABI-encode by hand
(structs are tuples):

```bash
cast abi-encode \
  "constructor(address,address,address,address,(address,address,address,uint256,uint256),(address,uint256,uint256,uint256,uint256,uint256))" \
  "$ORACLE" "$USDC" "$AAVE_POOL" "$AUSDC" \
  "($PRIMARY,$FALLBACK,$LAYERHASHES,$GENESIS_BK_SET_COMMITMENT,$GENESIS_PREV_MAX_LEVEL_LAYER_HASH)" \
  "($WITHDRAW_VERIFIER,$DAPP_FR,$ACC_FR,$ALT_DST_CHAIN_ID,$ALT_DST_HOST_CHAIN_ID,$ALT_TOKEN_ID)"
```

**Etherscan V2 gotchas:**

- If Foundry doesn't recognise the chain (`ETHERSCAN_API_KEY must be set…`), add
  `--verifier etherscan --verifier-url "https://api.etherscan.io/v2/api?chainid=<id>"`.
- If via-IR metadata trips verification, run `forge verify-contract
  --show-standard-json-input …` and submit the JSON manually in the Etherscan UI.

After a green ✓ badge, record the verified address + explorer URL in the
deployment notes, and link the `verifiers/README.md` provenance entry for the
raw-bytecode verifiers.

---

*Confirm network and contract addresses with the operator before each test
campaign. This document describes the Sepolia MetaMask + Etherscan deposit path
only.*
