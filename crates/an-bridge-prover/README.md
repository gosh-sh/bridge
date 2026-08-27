# Acki Nacki → Ethereum bridge prover

This repo turns withdrawal events on Acki Nacki into zero-knowledge proofs that an Ethereum smart contract can check.

> Looking for the deep-dive? See **[TECHNICAL_README.md](./TECHNICAL_README.md)**. This file is the short, end-user quickstart.

## What the bridge does, in 30 seconds

You want to move tokens from Acki Nacki to Ethereum.

1. On Acki Nacki, you call the **TokenBridge** contract. It **burns your tokens** and emits a `WithdrawalInitiated` event into the block.
2. Off-chain, this repo produces a small ZK proof saying *"that event really happened inside a finalised Acki Nacki block"*.
3. That proof is submitted to an Ethereum bridge contract, which checks it and **releases the equivalent funds to your Ethereum address**.

The script in this guide drives step 1 and produces the proof for step 2. Step 3 (submitting the proof to Ethereum) needs a small helper that is **not shipped yet** — see [Status](#status) below.

## Where to go next

- **[TECHNICAL_README.md](./TECHNICAL_README.md)** — architecture, daemon internals, IPC, bundle math, troubleshooting.
- **[`acki-nacki-to-eth-bridge-halo2-circuits/README.md`](https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits)** — the five Halo2 circuits and how block finalisation + withdrawal authenticity are encoded.
