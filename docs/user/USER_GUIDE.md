# Acki Nacki Bridge — User Guide (CLI)

**A cross-chain bridge between Ethereum and Acki Nacki, secured end-to-end by zero-knowledge proofs.**

*Revision: 2 June 2026 · Test deployment (Sepolia)*

---

> 🛠️ **This is a command-line guide for developers and advanced testers.** You drive
> the bridge directly with `cast` (Foundry) and the repo's `deposit-relayer` CLI.
> Everything below assumes a checkout of this repository, a funded Sepolia key, and the
> toolchain from `make setup`. A browser-based **web frontend** also ships in the repo
> (`frontend/`) — you run it yourself; see §7. It is not a hosted product, and the CLI
> below is the source of truth for the current USDT flow.

---

## 1. What is the Acki Nacki Bridge?

The Acki Nacki Bridge lets you move value between the **Ethereum** network and the
**Acki Nacki** blockchain. The bridge moves **USDT** — the ERC-20 stablecoin
(Tether), which uses **6 decimals** on Ethereum. Unlike traditional bridges that rely
on a trusted group of signers, this bridge uses **zero-knowledge (ZK) cryptographic
proofs** to verify that every cross-chain event really happened. In plain terms:

> You don't have to trust an operator. The bridge proves, mathematically, that your
> deposit was recorded on Ethereum before any tokens are issued on Acki Nacki.

> 💵 **The bridge accepts USDT, not ETH.** You deposit USDT (an ERC-20 token), and
> you still pay the usual Ethereum **gas in ETH**. If you only have ETH, mint test
> USDT first — see §3.

### Two directions, two mechanisms

| Direction | What it does | Status |
| --- | --- | --- |
| **Ethereum → Acki Nacki** (Deposit) | You send **USDT** on Ethereum; equivalent tokens are minted to you on Acki Nacki after a ZK proof of your deposit is verified. | ✅ **Available** (deposit + prove on a real chain; AN submit is mock-only — see §6) |
| **Acki Nacki → Ethereum** (Withdraw / burn) | Burn tokens on Acki Nacki and release USDT on Ethereum. | 🚧 **In development** — see §9 |

---

## 2. Before you start

You will need:

1. **Foundry** (`cast`, `forge`, `anvil`) — install with `curl -L https://foundry.paradigm.xyz | bash && foundryup`, or via `make setup`.
2. **A Sepolia private key** with a little test ETH for gas (§3). The bridge runs on
   Ethereum's **Sepolia testnet**, not mainnet.
3. **Test USDT on Sepolia** — the asset you actually bridge (§3).
4. **This repository, built** — `make build` (Rust workspace + Solidity). The
   `deposit-relayer` CLI lives in `crates/deposit-relayer-daemon/`.
5. A few minutes of patience — ZK proof generation is CPU/memory-heavy and not instant.

### Network details (testnet)

| Parameter | Value |
| --- | --- |
| Network name | Sepolia |
| Chain ID | `11155111` (`0xaa36a7`) |
| Public RPC | `https://rpc.sepolia.org` |
| Block explorer | https://sepolia.etherscan.io |
| Bridge contract | `0xDE8180911Ab2EbC9A6c1F5526bCE4c8242C061d9` |
| USDT token (Aave-faucet, Sepolia) | `0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0` |
| Aave Sepolia faucet | `0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D` |

> 💡 These values are for the current **test deployment** and may change. Always confirm
> the active contract and token addresses with the team before bridging.

A handy way to keep the commands below short — export these once per shell:

```bash
export RPC=https://rpc.sepolia.org
export PK=0x<YOUR_SEPOLIA_PRIVATE_KEY>
export ME=$(cast wallet address --private-key "$PK")
export BRIDGE=0xDE8180911Ab2EbC9A6c1F5526bCE4c8242C061d9
export USDT=0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0
export FAUCET=0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D
```

---

## 3. Get test ETH and test USDT

### Sepolia ETH (for gas)

Obtain free Sepolia ETH from any public faucet (search "Sepolia faucet"). A small
amount (0.01–0.05 ETH) is plenty to cover gas.

### Test USDT (what you bridge)

The bridge's test USDT is the Aave V3 Sepolia faucet token. Mint some straight from the
CLI — the faucet's `mint(address token, address to, uint256 amount)` sends test USDT to
any address:

```bash
# Mint 100 USDT (100 * 10^6, because USDT has 6 decimals) to yourself
cast send "$FAUCET" "mint(address,address,uint256)" "$USDT" "$ME" 100000000 \
    --rpc-url "$RPC" --private-key "$PK"
```

Check your balance any time:

```bash
cast call "$USDT" "balanceOf(address)(uint256)" "$ME" --rpc-url "$RPC"
# prints base units; divide by 1e6 for USDT (100000000 = 100 USDT)
```

> 💡 Amounts are always in **base units** (6 decimals): `1000000` = 1 USDT,
> `100000000` = 100 USDT.

---

## 4. Deposit USDT (Ethereum → Acki Nacki)

A USDT deposit is a **two-step ERC-20 flow**: first authorize the bridge to pull your
USDT (`approve`), then deposit. Both are ordinary Ethereum transactions, so you pay gas
in **ETH** for each.

```bash
# 1) Approve the bridge to spend your USDT (here: 100 USDT)
cast send "$USDT" "approve(address,uint256)" "$BRIDGE" 100000000 \
    --rpc-url "$RPC" --private-key "$PK"

# 2) Deposit (must be > 0 and <= 100 USDT = MAX_DEPOSIT_AMOUNT)
cast send "$BRIDGE" "deposit(uint256)" 100000000 \
    --rpc-url "$RPC" --private-key "$PK"
```

The `deposit(amount)` call pulls `amount` USDT from your wallet via `transferFrom` into
the bridge and emits a `Deposit(uint256 indexed depositId, address indexed sender,
uint256 amount, uint256 timestamp)` event. Your address (`msg.sender`) is recorded as
the depositor and is the party credited on Acki Nacki.

That's all you do on the Ethereum side. **You don't need to note or look up a Deposit
ID** — the relayer (§6) discovers your deposit on-chain and processes it automatically,
tracking the id internally as its own cursor.

> 💡 *(Optional)* If you want to prove one specific deposit by hand with `prove-one`
> (§6.3), the id you just created is `depositCounter - 1`
> (`cast call "$BRIDGE" "depositCounter()(uint256)" --rpc-url "$RPC"`); you can also list
> decoded deposits with the relayer's `watch` command (§6.1). The normal `daemon` flow
> needs none of this.

> ℹ️ **Who receives the tokens on Acki Nacki?** The bridge credits the **same address
> that made the deposit** (`msg.sender`). A configurable Acki Nacki receiver is planned;
> until the AN side provides concrete receiving details, a custom receiver is tracked
> off-chain only and is **not** sent on-chain.

---

## 5. Checking bridge status

Read-only views, no key or signature needed:

```bash
# How many deposits have been made
cast call "$BRIDGE" "depositCounter()(uint256)" --rpc-url "$RPC"

# USDT principal currently held by the bridge (treasury), in base units
cast call "$BRIDGE" "treasuryBalance()(uint256)" --rpc-url "$RPC"
```

For decoded, human-readable deposit listings use the relayer's read-only `watch`
command (§6.1). Your own transactions are visible on https://sepolia.etherscan.io.

---

## 6. The full Ethereum → Acki Nacki route, end-to-end

A deposit travels through three stages. The repo ships a dedicated relayer,
`deposit-relayer` (crate `crates/deposit-relayer-daemon/`), that automates all three:

| Stage | What runs | Tooling |
| --- | --- | --- |
| **1. Listen** | Watch the bridge contract for `Deposit` events and wait for confirmations. | `EthLogSource` (alloy `eth_getLogs`) |
| **2. Prove** | Build the Halo2 ZK proof of the deposit (the `vk_blob` + `public_inputs` + `proof` triple the AN verification opcode consumes). | `deposit-prover`, run out-of-process |
| **3. Submit** | Call `TokenBridge.finalizeDeposit(...)` on Acki Nacki, which verifies the proof natively and credits your tokens. | `AnSubmitter` over `IAckiNacki` |

> ⚠️ **Current limitation (stage 3).** A live Acki Nacki transaction-submission client
> (`IAckiNacki` over the `tvm-sdk`) is **not wired up yet**, and the on-chain
> `finalizeDeposit` message ABI isn't frozen. So today you can drive **stages 1 and 2
> against a real chain end-to-end**, and exercise stage 3 against an **in-memory mock
> Acki Nacki** (the `--dry-run` mode below). No transaction reaches a real Acki Nacki
> node yet. The relayer's *read-side* AN connectivity (BK-set queries) **is** live and
> is what the preflight checks.

### Prerequisites for stage 3 / a local AN node

The `deposit-prover` proving artefacts must be available in `deposit-prover/` (SRS /
proving key — see that crate's README). For an end-to-end run against a real,
locally-controlled chain, bring up the 5-node Acki Nacki cluster from the sibling
`acki-nacki` checkout:

```bash
cd ../acki-nacki/nock && docker-compose build && docker-compose up -d
# Node0 REST API is then reachable at http://127.0.0.1:11000
```

You can also point at the shared public AN test node (`http://94.156.178.19:8600`) for
read-only connectivity checks.

All commands below run from `crates/deposit-relayer-daemon/`.

### 6.1 Confirm the relayer can see your deposit (read-only)

```bash
cargo run --bin deposit-relayer -- \
    watch --rpc-url "$RPC" --bridge-address "$BRIDGE" \
    --start 0 --count 16
```

Prints `depositCounter()` and lists each confirmed `Deposit` (id, sender, amount, tx
hash, block). Nothing is proven or submitted — it just verifies visibility and
confirmation depth.

### 6.2 Check Acki Nacki connectivity (read-only)

```bash
cargo run --bin deposit-relayer -- \
    an-preflight --an-node-url http://127.0.0.1:11000   # or the public test node
```

Hits the AN node's `/v2/bk_set` endpoint and prints the current BK-set summary
(sequence number + committee sizes). If this fails, fix your AN node URL before going
further — the daemon runs the same check on startup so a mis-typed endpoint fails fast.

### 6.3 Generate the proof for one deposit (stages 1 + 2, real chain)

```bash
cargo run --bin deposit-relayer -- \
    prove-one --rpc-url "$RPC" --bridge-address "$BRIDGE" \
    --deposit-id <YOUR_DEPOSIT_ID> \
    --deposit-prover-dir ../../deposit-prover \
    --out-dir ./out
```

The relayer finds your deposit, runs the `deposit-prover` out-of-process, and writes the
three opcode operands to `./out/`:

- `vk_blob.bin` — the verifying-key blob,
- `public_inputs.bin` — the 7 public inputs
  (`depositId, sender, amount, contractAddress, blockHashHigh, blockHashLow, promiseCommit`),
- `proof.bin` — the Halo2 SHPLONK proof.

These are exactly the bytes Acki Nacki's verification opcode consumes. This step proves
the full *listen → prove* path works against a real chain, independent of the AN side.

### 6.4 Run the full loop (stages 1 → 3, dry-run submit)

```bash
cargo run --bin deposit-relayer -- \
    daemon --rpc-url "$RPC" --bridge-address "$BRIDGE" \
    --deposit-prover-dir ../../deposit-prover \
    --an-node-url http://127.0.0.1:11000 \
    --dry-run
```

This is the normal path. The daemon **automatically discovers new deposits on-chain and
processes them in order** — you never pass it a Deposit ID. It loops
*listen → prove → submit* with exponential backoff and clean SIGINT/SIGTERM shutdown,
persisting its cursor to `state.json` so a restart resumes from the last finalized
deposit. In `--dry-run` it proves against the real chain but "finalizes" only in an
in-memory mock Acki Nacki — so it's safe to run repeatedly.

> `--dry-run` is **mandatory** today: the daemon refuses to start without it and tells
> you why (no live `IAckiNacki` client yet). When the Acki Nacki team ships the live
> `tvm-sdk` client and the `finalizeDeposit` ABI is frozen, dropping `--dry-run` will
> make stage 3 send real transactions.

### 6.5 Why running it twice is safe (idempotency)

The relayer uses the **Deposit ID** as a monotonic cursor, and Acki Nacki keeps a
**nullifier set** (`usedDepositIds`) so a given deposit can be finalized at most once.
Re-running the daemon, restarting it, or retrying a failed submit will never
double-credit a deposit — already-finalized ids are simply skipped.

---

## 7. Optional: the web frontend (run it yourself)

The repo ships a browser UI under `frontend/` — a **Rust + Yew + WebAssembly** app with
MetaMask wallet connection, a USDT deposit form (with a built-in faucet button), bridge
stats, and transaction history. It is **not hosted anywhere**; you build and serve it
yourself. See `frontend/README.md` for full details.

```bash
cd frontend
cargo install trunk                         # one-time (WASM bundler)
rustup target add wasm32-unknown-unknown    # one-time
trunk serve                                 # dev server at http://localhost:8080
# or build a static bundle:  trunk build --release   (output in dist/)
# or run via Docker:         docker build -t an-bridge-frontend . && docker run -p 8080:80 an-bridge-frontend
```

Configure contract addresses / RPC in `frontend/src` (see the README's *Configuration*
section) before connecting a wallet.

The deposit form runs the same **USDT `approve` → `deposit(uint256)`** flow as the CLI:
enter an amount (≤ 100 USDT), and it approves the bridge if needed, then deposits,
waiting for each transaction to confirm. The **"Get 100 test USDT (faucet)"** button
mints test USDT from the Aave Sepolia faucet so you can try it with an empty wallet.
Update the contract / token / faucet addresses in `frontend/src/config.rs` if you're on
a different deployment.

> ℹ️ Withdrawals (AN → Ethereum) aren't exposed in the UI yet (see §9), and the
> deposit-relayer still picks up deposits regardless of how they were submitted — so the
> web app and the CLI (§4–§6) are interchangeable for the deposit step.

---

## 8. Fees, limits and safety

- **Per-deposit limit:** 100 USDT maximum per `deposit(amount)` call (`MAX_DEPOSIT_AMOUNT`).
  Zero is rejected (`InvalidAmount`); over 100 USDT is rejected (`DepositTooLarge`).
- **What you bridge vs. what you pay:** You bridge **USDT**; you pay gas in **ETH**.
  A typical deposit costs two Ethereum transactions (an `approve` and a `deposit`).
- **Treasury & yield:** Idle USDT held by the bridge can optionally be supplied to the
  AAVE V3 USDT market for yield by the bridge owner. This is operator-side and does not
  affect your deposit balance or your claim to bridged tokens.
- **Non-custodial trust model:** The bridge does not ask you to trust a signer set.
  Cross-chain state is advanced only when valid ZK proofs are verified on-chain.

### Safety checklist

- ✅ Confirm you are on **Sepolia** (testnet) and using the **correct contract and USDT
  addresses** (§2).
- ✅ Make sure you hold both **USDT** (to bridge) and a little **ETH** (for gas).
- ✅ Keep your private key out of shell history (`HISTCONTROL=ignorespace`, or use a
  keystore / hardware wallet). Never paste it into untrusted tooling.
- ✅ Double-check the amount (in 6-decimal base units) before sending.
- ⚠️ This is a **testnet deployment**. Do not bridge mainnet funds or treat test USDT as
  having real value.

---

## 9. Withdrawals (Acki Nacki → Ethereum) — current status

True cross-chain withdrawals (burning tokens on Acki Nacki to release USDT on Ethereum)
are **not yet available to users**. The legacy refund-style `withdraw` flow from earlier
versions has been **retired**, and the production burn-proof flow is still under active
development.

Under the hood, the Ethereum contract already carries the proof-verified machinery for
the reverse channel — a permissionless `verifyBlock` entry point that advances the
on-chain commitment to Acki Nacki's state after checking ZK attestations, and a
proof-gated `withdrawByProof` path — but these are protocol/operator-level and are
**not** exposed as a user action yet.

What this means for you right now:

- You **can** deposit USDT and (with the relayer) prove it end-to-end.
- You **cannot** yet move value back from Acki Nacki to Ethereum through the bridge.

This guide will be updated when withdrawals go live.

---

## 10. How the bridge stays trustworthy (plain-English overview)

- **Your deposit is proven, not asserted.** A zero-knowledge proof demonstrates that your
  deposit transaction was included in a real Ethereum block before any tokens are minted.
- **Acki Nacki's own state is attested with proofs too.** The reverse channel checks
  cryptographic attestations of Acki Nacki blocks (validator signatures, block identity,
  and chain progression) using ZK proofs verified on Ethereum.
- **Strict on-chain rules.** Every state update enforces invariants such as monotonic
  block sequence numbers and chain-anchor consistency, so the bridge can't be tricked into
  accepting out-of-order or forged state.

---

## 11. Troubleshooting

| Problem | Likely cause | What to do |
| --- | --- | --- |
| `deposit` reverts with `InvalidAmount` | Amount was 0 | Pass a positive amount in base units (e.g. `1000000` = 1 USDT). |
| `deposit` reverts with `DepositTooLarge` | Amount > 100 USDT | Lower the amount to ≤ `100000000`. |
| `deposit` reverts / `transferFrom` failed | No (or too small) `approve`, or insufficient USDT balance | Run the `approve` step (§4) for at least the deposit amount; mint more USDT (§3). |
| Transaction fails: out of gas / insufficient funds | Not enough Sepolia **ETH** for gas | Top up Sepolia ETH (§3). |
| `deposit` reverts when paused | Owner paused the contract | Wait until unpaused; deposits/verify/withdraw are blocked while paused. |
| `prove-one` says "not visible / not confirmed yet" | Deposit not mined or not enough confirmations | Wait for confirmations (default 12), or lower `--confirmations`. |
| `an-preflight` fails | AN node URL wrong/unreachable | Fix `--an-node-url`; bring up the local cluster (§6) or use the public node. |
| `daemon` refuses to start | `--dry-run` missing | Add `--dry-run` — live AN submit isn't wired yet (§6). |

---

## 12. Glossary

- **ZK proof (zero-knowledge proof):** A cryptographic proof that a statement is true
  without revealing extra information. Here, it proves your deposit really happened.
- **Halo2 / Groth16:** ZK proof systems used by the bridge. Halo2 proofs are sometimes
  wrapped in compact Groth16 proofs to fit Ethereum's contract size limits.
- **Deposit ID:** A unique number assigned to each deposit (`depositCounter - 1` right
  after your deposit), emitted in the `Deposit` event.
- **USDT:** A US-dollar stablecoin (Tether) issued as an ERC-20 token with **6 decimals**.
  This is the asset the bridge moves. On Sepolia, the bridge uses the Aave faucet USDT (§2).
- **ERC-20 approve:** A token permission step. Before the bridge can pull your USDT, you
  send an `approve` transaction authorizing it to spend up to a set amount on your behalf.
- **Treasury (`treasuryBalance`):** USDT principal held by the bridge from user deposits.
- **AAVE V3:** A lending protocol where idle treasury USDT may be placed to earn yield
  (operator-managed; does not affect your funds' claimability).
- **Sepolia:** An Ethereum test network used for the current deployment.
- **TVM:** The virtual machine Acki Nacki uses to execute contracts, including native
  proof verification.
- **Axiom (halo2-lib):** The Halo2 circuit framework the deposit prover is built on; it
  produces proofs the Acki Nacki verification opcode can check natively.
- **deposit-relayer:** The CLI tool (crate `crates/deposit-relayer-daemon/`) that
  automates the deposit pipeline — listen for `Deposit` events, generate the ZK proof,
  and finalize on Acki Nacki. See §6.
- **verifyBlock:** A permissionless Ethereum-side function that advances the bridge's
  view of Acki Nacki's chain state after verifying ZK attestation proofs (protocol-level).
- **Nullifier (`usedDepositIds`):** An Acki Nacki–side record of which deposits have
  already been finalized, guaranteeing each deposit is credited at most once.

---

*This document describes the current test deployment of the Acki Nacki Bridge and will
evolve as features such as withdrawals become available. Always verify network and
contract details with the team before bridging.*
