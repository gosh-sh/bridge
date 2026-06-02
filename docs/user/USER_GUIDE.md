# Acki Nacki Bridge — User Guide

**A cross-chain bridge between Ethereum and Acki Nacki, secured end-to-end by zero-knowledge proofs.**

*Revision: 31 May 2026 · Test deployment (Sepolia)*

---

## 1. What is the Acki Nacki Bridge?

The Acki Nacki Bridge lets you move value between the **Ethereum** network and the
**Acki Nacki** blockchain. Unlike traditional bridges that rely on a trusted group
of signers, this bridge uses **zero-knowledge (ZK) cryptographic proofs** to verify
that every cross-chain event really happened. In plain terms:

> You don't have to trust an operator. The bridge proves, mathematically, that your
> deposit was recorded on Ethereum before any tokens are issued on Acki Nacki.

### Two directions, two mechanisms

| Direction | What it does | Status for users |
| --- | --- | --- |
| **Ethereum → Acki Nacki** (Deposit) | You send ETH on Ethereum; equivalent tokens are minted to you on Acki Nacki after a ZK proof of your deposit is verified. | ✅ **Available** |
| **Acki Nacki → Ethereum** (Withdraw / burn) | Burn tokens on Acki Nacki and release ETH on Ethereum. | 🚧 **In development** — see §6 |

> ⚠️ **Important:** At the time of writing, only the **Deposit** direction is available
> to end users. Genuine cross-chain withdrawals are still being built. Any "Withdraw"
> control you may see in an early build of the web app is a placeholder and will not
> move funds back to Ethereum.

---

## 2. Before you start

You will need:

1. **A MetaMask wallet** (browser extension or mobile). Other EIP-1193 wallets may work,
   but MetaMask is the supported and tested option.
2. **Test ETH on the Sepolia network.** The bridge currently runs on Ethereum's
   **Sepolia testnet**, not on Ethereum mainnet. Sepolia ETH has no real monetary value
   and is meant for testing.
3. A few minutes of patience — proof generation and verification happen automatically
   in the background but are not instant.

### Network details (testnet)

| Parameter | Value |
| --- | --- |
| Network name | Sepolia |
| Chain ID | `11155111` (`0xaa36a7`) |
| Public RPC | `https://rpc.sepolia.org` |
| Block explorer | https://sepolia.etherscan.io |
| Bridge contract | `0xDE8180911Ab2EbC9A6c1F5526bCE4c8242C061d9` |

> 💡 These values are for the current **test deployment** and may change. Always confirm
> the active contract address with the team before bridging anything you care about.

### Getting Sepolia test ETH

You can obtain free Sepolia ETH from public faucets (search "Sepolia faucet"). You only
need a small amount to try the bridge — for example, 0.01–0.1 ETH.

---

## 3. Depositing ETH (Ethereum → Acki Nacki)

This is the main user flow. Here is what happens, step by step.

### What you do

1. **Open the Bridge web app** in a browser that has MetaMask installed.
2. **Connect your wallet.** Click *Connect Wallet* in the header and approve the
   MetaMask prompt.
3. **Switch to Sepolia.** If you're on another network, the app will ask MetaMask to
   switch to Sepolia automatically. Approve it.
4. **Enter the amount** of ETH you want to bridge in the *Deposit* form.
   - The amount must be **greater than 0** and **at most 100 ETH** per deposit (`MAX_DEPOSIT_AMOUNT`). A zero amount is rejected (`InvalidAmount`); anything above 100 ETH is rejected (`DepositTooLarge`).
5. **Submit and confirm** the transaction in MetaMask. This calls the bridge's
   `deposit()` function (it takes no arguments — the ETH you send is the deposit)
   and transfers your ETH to the bridge contract. Your **connected wallet address**
   (`msg.sender`) is recorded as the depositor and is the party credited on Acki Nacki.
6. **Wait for the transaction to be mined.** Once confirmed, the bridge emits a
   `Deposit` event containing your unique **Deposit ID**, your address, the amount,
   and a timestamp.

### What happens behind the scenes

You don't have to do any of this yourself — it's fully automated — but here's what the
system does so your tokens appear on Acki Nacki:

1. An **off-chain prover** reads your deposit transaction and its inclusion proof from
   Ethereum, then builds a **Halo2 ZK proof** (Axiom-based circuit backend) that your
   `Deposit` event was genuinely emitted by the bridge contract in a real Ethereum block.
   The proof artifacts are exported in a form the Acki Nacki verification opcode can
   consume directly.
2. The Acki Nacki side **verifies that proof natively** (via a dedicated TVM verification
   opcode) and checks the public inputs.
3. Once the proof is accepted, the corresponding **tokens are minted to you on
   Acki Nacki**.

The result: your ETH is locked on Ethereum, and you receive matching tokens on
Acki Nacki — without trusting any human operator.

### Tracking your deposit

- Use your **transaction hash** on https://sepolia.etherscan.io to confirm the deposit
  landed on Ethereum.
- Note your **Deposit ID** from the `Deposit` event — it uniquely identifies your
  bridging operation.

> ℹ️ **Who receives the tokens on Acki Nacki?** In the current deployment, the bridge
> credits the **same address that made the deposit** (`msg.sender`). A configurable
> Acki Nacki receiver is being prepared for a future release; until the Acki Nacki side
> provides concrete receiving details, a custom receiver is tracked off-chain only and
> is **not** sent on-chain.

---

## 4. Checking bridge status & history

The web app surfaces live, read-only information pulled directly from the contract:

- **Total deposits** made through the bridge (`depositCount`).
- **Total value deposited** (`totalDeposited`).
- **Your transaction history**, with direct links to Etherscan.

These are informational and never require a signature.

---

## 5. Fees, limits and safety

- **Per-deposit limit:** 100 ETH maximum per `deposit()` call.
- **Gas:** You pay normal Ethereum (Sepolia) gas fees for the deposit transaction.
- **Treasury & yield:** Idle ETH held by the bridge can optionally be put to work in
  AAVE V3 for yield by the bridge owner. This is an operator-side feature and does not
  affect your deposit balance or your claim to bridged tokens.
- **Non-custodial trust model:** The bridge does not ask you to trust a signer set.
  Cross-chain state is advanced only when valid ZK proofs are verified on-chain.

### Safety checklist

- ✅ Confirm you are on **Sepolia** (testnet) and using the **correct contract address**.
- ✅ Never enter your seed phrase anywhere — MetaMask never asks for it in a dApp.
- ✅ Double-check the amount before confirming in MetaMask.
- ⚠️ This is a **testnet deployment**. Do not bridge mainnet ETH or treat test tokens as
  having real value.

---

## 6. Withdrawals (Acki Nacki → Ethereum) — current status

True cross-chain withdrawals (burning tokens on Acki Nacki to release ETH on Ethereum)
are **not yet available to users**. The legacy refund-style `withdraw` flow from earlier
versions has been **retired**, and the production burn-proof flow is still under active
development.

Under the hood, the Ethereum contract already carries the proof-verified machinery for
the reverse channel — a permissionless `verifyBlock` entry point that advances the
on-chain commitment to Acki Nacki's state after checking ZK attestations, and a
proof-gated `withdrawByProof` path — but these are protocol/operator-level and are **not**
exposed as a user action yet.

What this means for you right now:

- You **can** deposit ETH and receive tokens on Acki Nacki.
- You **cannot** yet move value back from Acki Nacki to Ethereum through the bridge.
- If you see a "Withdraw" form in an early build of the app, treat it as a **non-functional
  placeholder**.

This guide will be updated when withdrawals go live.

---

## 7. How the bridge stays trustworthy (plain-English overview)

For the curious, here's why the bridge is considered secure without a trusted operator:

- **Your deposit is proven, not asserted.** A zero-knowledge proof demonstrates that your
  deposit transaction was included in a real Ethereum block before any tokens are minted.
- **Acki Nacki's own state is attested with proofs too.** The reverse channel checks
  cryptographic attestations of Acki Nacki blocks (validator signatures, block identity,
  and chain progression) using ZK proofs verified on Ethereum.
- **Strict on-chain rules.** Every state update enforces invariants such as monotonic
  block sequence numbers and chain-anchor consistency, so the bridge can't be tricked into
  accepting out-of-order or forged state.

You don't need to understand the cryptography to use the bridge — but it's the reason the
bridge can be trusted by math rather than by people.

---

## 8. Troubleshooting

| Problem | Likely cause | What to do |
| --- | --- | --- |
| "Please connect your wallet first" | MetaMask not connected | Click *Connect Wallet* and approve. |
| "Please switch to Sepolia network" | Wrong network selected | Approve the network switch in MetaMask, or add Sepolia manually. |
| "Invalid amount" | Empty or non-numeric amount | Enter a positive number of ETH (≤ 100). |
| Transaction fails / reverts | Insufficient gas, deposit over 100 ETH, or paused contract | Check your Sepolia ETH balance, lower the amount, and retry. |
| Deposit confirmed but no tokens on Acki Nacki yet | Proof generation/verification still in progress | Wait — minting happens after the ZK proof is verified. |
| MetaMask not detected | Extension missing or disabled | Install/enable MetaMask and reload the page. |

---

## 9. Glossary

- **ZK proof (zero-knowledge proof):** A cryptographic proof that a statement is true
  without revealing extra information. Here, it proves your deposit really happened.
- **Halo2 / Groth16:** ZK proof systems used by the bridge. Halo2 proofs are sometimes
  wrapped in compact Groth16 proofs to fit Ethereum's contract size limits.
- **Deposit ID:** A unique number assigned to each deposit, emitted in the `Deposit` event.
- **Sepolia:** An Ethereum test network used for the current deployment.
- **TVM:** The virtual machine Acki Nacki uses to execute contracts, including native
  proof verification.
- **Treasury:** ETH held by the bridge contract from user deposits.
- **AAVE V3:** A lending protocol where idle treasury ETH may be placed to earn yield
  (operator-managed; does not affect your funds' claimability).
- **Axiom (halo2-lib):** The Halo2 circuit framework the deposit prover is built on; it
  produces proofs the Acki Nacki verification opcode can check natively.
- **verifyBlock:** A permissionless Ethereum-side function that advances the bridge's
  view of Acki Nacki's chain state after verifying ZK attestation proofs (protocol-level).

---

*This document describes the current test deployment of the Acki Nacki Bridge and will
evolve as features such as withdrawals become available. Always verify network and
contract details with the team before bridging.*
