# Acki Nacki Bridge — User Guide

**Move test funds from Ethereum to the Acki Nacki blockchain, safely and automatically.**

*Revision: 2 June 2026 · Test version (Sepolia test network)*

---

> 👋 **This guide is for everyday users.** You do **not** need to be a developer or use
> any command-line tools. Everything here is done by opening a webpage and clicking
> buttons in your browser. If you are a developer and want the technical/command-line
> version, jump to the last section, [For developers](#9-for-developers-advanced).

---

## 1. What does this bridge do?

A **bridge** is a tool that lets you move money from one blockchain to another. Think of
it like a money-transfer service that connects two different banking systems.

This particular bridge connects two blockchains:

- **Ethereum** — a well-known blockchain. (We use its free *test* version, called
  **Sepolia**, so no real money is involved.)
- **Acki Nacki** — a newer, fast blockchain.

Here is what happens in plain terms:

1. You **deposit** some **USDC** on Ethereum. USDC is a "stablecoin" — a digital dollar,
   where 1 USDC is meant to be worth about 1 US dollar. (In this test version, the USDC
   is fake/test money with no real value.)
2. The bridge then makes the **same amount available to you on Acki Nacki**.

Behind the scenes, the bridge uses a **zero-knowledge proof** to do this safely. A
zero-knowledge proof is a piece of math that proves your deposit really happened on
Ethereum — without you having to trust any company or person to confirm it. You don't
have to understand the math; just know that it's what keeps the bridge honest.

> 💡 **Good to know:** Right now the bridge only moves funds **one way** — from Ethereum
> to Acki Nacki. Moving funds back (Acki Nacki → Ethereum) is still being built.

---

## 2. What you need before you start

You'll need three things, all free for testing:

1. **A MetaMask wallet.** MetaMask is a free browser extension that acts like a digital
   wallet for your blockchain funds. If you don't have it, install it from
   [metamask.io](https://metamask.io) and follow its setup steps. **Keep your secret
   recovery phrase private** — never share it with anyone.

2. **A little test ETH (for "gas").** Every action on Ethereum has a small network fee
   called **gas**, paid in ETH (Ethereum's own coin). It's like a postage stamp for your
   transaction. You only need a tiny amount of *test* ETH — see [Section 3](#3-how-to-get-test-eth-and-test-usdc).

3. **Some test USDC (the money you'll bridge).** This is the actual amount you send
   across. You can get free test USDC from a faucet — see [Section 3](#3-how-to-get-test-eth-and-test-usdc).

> ⚠️ **This is a test version.** Everything uses the Sepolia *test* network and fake test
> coins. Do **not** send real money or treat any of these tokens as having real value.

### Make sure MetaMask is on the right network

This bridge runs on **Sepolia**, Ethereum's test network — not the real Ethereum
network. In MetaMask, switch your network to **Sepolia** before you start. (MetaMask
sometimes hides test networks by default; if you don't see Sepolia, enable "Show test
networks" in MetaMask's settings.)

The web app should also help you switch to Sepolia automatically when you connect.

---

## 3. How to get test ETH and test USDC

Because this is a test network, the coins are free. You get them from "faucets" —
websites that hand out free test coins.

### Test ETH (for gas)

1. Open your web browser and search for **"Sepolia faucet"**.
2. Pick one of the public faucets in the results.
3. Paste in **your MetaMask wallet address** (open MetaMask and click your account name
   to copy it).
4. Request the test ETH. A small amount (about 0.01–0.05 ETH) is plenty to cover many
   transactions.

### Test USDC (the money you'll bridge)

The easiest way is the **Aave Sepolia faucet**:

1. Go to the [Aave faucet](https://app.aave.com/faucet/) and connect your MetaMask wallet
   (make sure it's set to the Sepolia test network).
2. Find **USDC** in the list and request/mint some test USDC.
3. It will appear in your wallet on the Sepolia network.

> 💡 The bridge's own web app may also include a **"Get test USDC"** button that does this
> for you in one click. If you see it, that's the simplest option.

---

## 4. How to make a deposit (step by step)

You do everything in the **web app** — a simple webpage you open in your browser with
MetaMask installed. Ask the bridge operator (or check the project page) for the link to
the web app.

A deposit takes a few clicks. Here's the whole flow:

### Step 1 — Open the web app and connect your wallet

1. Open the bridge web app in your browser.
2. Click **"Connect Wallet"**. MetaMask will pop up asking for permission — click
   **Connect**.
3. If MetaMask asks to switch to the **Sepolia** network, approve it.

### Step 2 — Approve the bridge to use your USDC

Before the bridge can move your USDC, you have to give it permission. This is called an
**approve** step — it's a one-time "yes, you may use up to this much of my USDC" message.
It does **not** send any money yet; it just unlocks it.

1. Enter the amount you want to deposit (for example, `10` USDC).
2. Click **Approve** (the app may label it "Approve USDC").
3. MetaMask pops up — review it and click **Confirm**. This costs a small amount of gas.
4. Wait a few seconds for it to confirm.

### Step 3 — Choose your Acki Nacki recipient and deposit

Now you tell the bridge **where on Acki Nacki the funds should arrive**, and send the
deposit.

1. Enter your **Acki Nacki recipient account** — this is your address on the Acki Nacki
   side, where the funds will appear. (Acki Nacki uses a different kind of address than
   Ethereum, so you can't just reuse your MetaMask address here. If you're unsure what to
   put, ask the operator.)
2. Double-check the **amount**.
3. Click **Deposit**.
4. MetaMask pops up again — click **Confirm**. This also costs a little gas.
5. Wait for the transaction to confirm.

That's it — you're done on the Ethereum side! 🎉

> 💡 **Why two confirmations?** The first (approve) unlocks your USDC; the second
> (deposit) actually sends it. This two-step pattern is standard for this kind of token
> on Ethereum.

---

## 5. What happens next

After your deposit confirms, **you don't have to do anything else.** An automated service
called a **relayer** takes over:

1. The relayer notices your deposit on Ethereum.
2. It creates the **zero-knowledge proof** (the math that proves your deposit is real).
3. It sends that proof to Acki Nacki, where your funds are created in your Acki Nacki
   account.

You don't run or manage any of this — it happens on its own in the background.

> ⏱️ **How long does it take?** Generating the proof is computation-heavy, so it isn't
> instant. Allow a few minutes (sometimes longer, depending on system load). Once it
> finishes, your funds appear on the Acki Nacki side.

> 🧪 **Heads-up for this test version:** the final step that delivers funds onto a live
> Acki Nacki network is still being connected. So in today's test setup you can complete
> the deposit and watch the proof get generated, but the funds may not yet land on a real
> Acki Nacki node. The operator can tell you the current status.

---

## 6. Reference details

You usually don't need these, but they're handy to have:

| Item | Value |
| --- | --- |
| Network | **Sepolia** (Ethereum's test network) |
| Bridge contract address | `0xDE8180911Ab2EbC9A6c1F5526bCE4c8242C061d9` |
| Token you deposit | **USDC** (test version, on Sepolia) |
| Where to get test USDC | Aave Sepolia faucet |
| Where to get test ETH | Any public "Sepolia faucet" |
| Block explorer (to view transactions) | [sepolia.etherscan.io](https://sepolia.etherscan.io) |

> ⚠️ **Always confirm the current addresses.** This is a test deployment and addresses can
> change. Check the **current bridge contract address** in the web app or with the
> operator before you deposit, rather than trusting an address copied from a guide.

To see your own transactions, copy the transaction link from MetaMask (or paste your
wallet address into [sepolia.etherscan.io](https://sepolia.etherscan.io)).

---

## 7. Troubleshooting & FAQ

**My transaction is stuck or pending for a long time.**
This usually sorts itself out. If it's truly stuck, MetaMask offers a "Speed up" or
"Cancel" option on the pending transaction. Also make sure you're connected to the
**Sepolia** network.

**I got an "insufficient funds" or "out of gas" error.**
You don't have enough test **ETH** to pay the network fee (gas). Get more test ETH from a
Sepolia faucet (see [Section 3](#3-how-to-get-test-eth-and-test-usdc)). Remember: gas is
paid in ETH, separately from the USDC you're bridging.

**The deposit button is greyed out, or the deposit fails.**
Common reasons:
- You haven't done the **Approve** step yet (Step 2), or you approved less than you're
  trying to deposit. Approve again for at least the deposit amount.
- You don't have enough **test USDC** in your wallet. Get more from the faucet.
- The amount is `0`, or above the per-deposit limit. Try a smaller, positive amount.
- You left the **Acki Nacki recipient** blank or it's invalid. Enter a valid recipient.

**I'm on the wrong network.**
Open MetaMask and switch the network to **Sepolia**. The web app may also prompt you to
switch automatically.

**Where did my funds go? I don't see them yet.**
After the deposit, the automated relayer needs a few minutes to generate the proof and
deliver your funds to Acki Nacki (see [Section 5](#5-what-happens-next)). Your Ethereum
deposit transaction is always viewable on
[sepolia.etherscan.io](https://sepolia.etherscan.io). If funds still don't appear after a
reasonable wait, contact the operator (next question).

**Can I move funds back from Acki Nacki to Ethereum?**
Not yet — that direction is still being built. For now, the bridge only moves funds from
Ethereum to Acki Nacki.

**Something else is wrong / who do I contact?**
Reach out to the bridge operator or project team for help. Have your **transaction hash**
ready (you can copy it from MetaMask or Etherscan) — it helps them find your deposit
quickly.

---

## 8. A few safety reminders

- ✅ Make sure MetaMask is on the **Sepolia** test network.
- ✅ Keep a little **test ETH** for gas and some **test USDC** to bridge.
- ✅ Confirm the **current bridge address** in the app before depositing.
- ✅ **Never share your MetaMask secret recovery phrase** with anyone — not even support
  staff. No legitimate operator will ever ask for it.
- ⚠️ This is a **test version**. The coins are not real money.

---

## 9. For developers (advanced)

Prefer to drive the bridge from the command line, run the relayer yourself, or host the
web frontend? That's all documented separately. See the developer documentation in this
repository — start with the project's main docs and the
`crates/deposit-relayer-daemon/` and `frontend/` READMEs — for the `cast`/Foundry deposit
flow, the `deposit-relayer` CLI, proof generation, and self-hosting instructions.

---

*This document describes the current test version of the Acki Nacki Bridge and will be
updated as new features (such as moving funds back from Acki Nacki to Ethereum) become
available. Always confirm network and contract details with the operator before
depositing.*
