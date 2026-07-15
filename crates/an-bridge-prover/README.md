# Acki Nacki → Ethereum bridge prover

This repo turns withdrawal events on Acki Nacki into zero-knowledge proofs that an Ethereum smart contract can check.

> Looking for the deep-dive? See **[TECHNICAL_README.md](./TECHNICAL_README.md)**. This file is the short, end-user quickstart.

---

## What the bridge does, in 30 seconds

You want to move tokens from Acki Nacki to Ethereum.

1. On Acki Nacki, you call the **TokenBridge** contract. It **burns your tokens** and emits a `WithdrawalInitiated` event into the block.
2. Off-chain, this repo produces a small ZK proof saying *"that event really happened inside a finalised Acki Nacki block"*.
3. That proof is submitted to an Ethereum bridge contract, which checks it and **releases the equivalent funds to your Ethereum address**.

The script in this guide drives step 1 and produces the proof for step 2. Step 3 (submitting the proof to Ethereum) needs a small helper that is **not shipped yet** — see [Status](#status) below.

---

## What `test_bridge_short_stub.py` does

`python/test_bridge_short_stub.py` is a stripped-down version of the existing dev orchestrator. It:

1. Deploys a small multisig wallet on Acki Nacki and funds it with test tokens.
2. Waits for the right moment in the block stream, then calls `TokenBridge.initiateWithdrawal(...)`. This burns the tokens and emits `WithdrawalInitiated`.
3. Grabs the event's metadata via GraphQL.
4. Runs three Rust binaries that turn the event into a Halo2 KZG proof.
5. **Stops there.** The proof file is left under `/tmp/bridge-e2e-stub/event_000000/` for the Ethereum-side submitter to pick up later.

---

## Prerequisites

### 1. A running Acki Nacki node

You need a node you can reach via GraphQL. The defaults assume a local devnet at `http://localhost/graphql`; for shellnet, set two environment variables (see [Run it](#run-it)).

### 2. `tvm-cli`

Used to encode contract calls and deploy the multisig.

A pre-built copy is already bundled under `python/bin/tvm-cli` — if it works on your machine, you can skip this section.

If you need to build it yourself:

```bash
git clone https://github.com/tvmlabs/tvm-sdk.git --branch full_dex_and_bridge_test_with_final_halo2_circuit
cd tvm-sdk
cargo build --release -p tvm_cli   # produces target/release/tvm-cli
cp target/release/tvm-cli /usr/local/bin/  # or any other directory on your $PATH
```

You do **not** need `sold` (the Solidity-to-TVM compiler) — all contracts the script touches are pre-compiled and bundled under `python/contracts/`.

### 3. The three Rust binaries from this repo

From inside this repo:

```bash
cargo build --release -p bridge-event-private-witness-export
cargo build --release -p bridge-event-witness-builder
cargo build --release -p bridge-event-halo2-prover
```

This produces three executables under `target/release/`. Build status:

| Binary | Ready? | Notes |
|---|---|---|
| `bridge-event-private-witness-export` | ✅ Yes | Decodes the event BOC into a partial witness. Done. |
| `bridge-event-halo2-prover` | ✅ Yes | Generates the Halo2 proof. Done. |
| `bridge-event-witness-builder` | ⚠️ **Not in final shape** | Today it builds a witness against the off-chain modelling verifier daemon. Will be rewritten to anchor against the live Ethereum bridge contract. The stub script will be updated to track that change. |

### 4. Circuit 4 proving + verifying key (~3 GB)

The Circuit 4 keys live under `params/event_pk.bin` + `params/event_vk.bin`. They are **not downloaded** — you have to generate them locally, once. Subsequent runs reuse the cached files.

Why up front: today the stub still calls `bridge-event-witness-builder`, which reads state written by `bridge-verifier-daemon`. The verifier daemon refuses to start unless `params/event_vk.bin` already exists. So generate the keys **before** you start the daemons:

```bash
cargo run --release --bin bridge-event-halo2-prover -- --selftest
```

Takes ~5 min and several GB of disk + RAM. On the same run it also creates `params/kzg_bn254_20.srs` (~128 MB), which is the trusted-setup SRS reused by all three circuits.

Circuits 1A and 2 keys (`primary_*.bin`, `layer_*.bin`) are produced separately by `bridge-prover-daemon` on its first start — same as in the local-devnet runbook in [TECHNICAL_README.md § Step 3](./TECHNICAL_README.md#step-3--keys-first-run-only).

### 5. Python 3

Stdlib only — no `pip install` needed.

---

## Bundled contract ABIs

Already in this repo under `python/contracts/` — you don't need to fetch them anywhere:

| File | What it is |
|---|---|
| `TokenBridge.abi.json` | ABI of the Acki Nacki bridge contract you call to start a withdrawal. |
| `GiverV3.abi.json` + `GiverV3.keys.json` | The dev "giver" that hands out test tokens. Used to fund the multisig. |
| `UpdateCustodianMultisigWallet.abi.json` + `.tvc` | The multisig wallet the script deploys to act as the withdrawer. |

---

## Run it

### Against shellnet

```bash
NETWORK=shellnet.ackinacki.org \
GRAPHQL_URL=https://shellnet.ackinacki.org/graphql \
python3 python/test_bridge_short_stub.py
```

### Against a local devnet

```bash
python3 python/test_bridge_short_stub.py
```

(Local-devnet defaults: `NETWORK=http://127.0.0.1:80`, `GRAPHQL_URL=http://localhost/graphql`.)

### What success looks like

You should see this near the end of the output:

```
[T+HH:MM] === STUB END — proof produced and self-verified ===
[T+HH:MM]   proof:     /tmp/bridge-e2e-stub/event_000000/proof_event_000000.json
[T+HH:MM]   artefacts: /tmp/bridge-e2e-stub/event_000000
```

That proof file is what step 3 (the not-yet-shipped Ethereum submitter) will consume.

### Useful environment overrides

| Variable | Default | When you might set it |
|---|---|---|
| `NETWORK` | `http://127.0.0.1:80` | Shellnet or a remote devnet. |
| `GRAPHQL_URL` | `http://localhost/graphql` | Match your node's GraphQL endpoint. |
| `PROVER_DIR` | this repo | If you keep the built binaries somewhere else. |
| `WORK_DIR` | `/tmp/bridge-e2e-stub` | Where the script drops intermediates and the final proof. |
| `CLI_NAME` | bundled `python/bin/tvm-cli` | Path to a different `tvm-cli` binary. |

---

## Status

What works today:

- ✅ Acki Nacki side: deploy multisig, fire `initiateWithdrawal`, capture event.
- ✅ `bridge-event-private-witness-export`: decodes the event.
- ✅ `bridge-event-halo2-prover`: produces a Halo2 KZG proof of the event.

What's not done yet (planned next):

- 🔲 **Ethereum submitter.** A small helper (likely a `web3.py` script or a Rust binary using `ethers-rs`) that takes the proof file and posts a `proveWithdrawal(...)` transaction to the Ethereum bridge contract.
- 🔲 **`bridge-event-witness-builder` rewrite.** Today it builds the Circuit-4 witness against the off-chain `bridge-verifier-daemon`'s mirror state. The rewrite will read the Ethereum bridge contract's `layerWindows` storage directly, so the stub no longer depends on running an off-chain verifier daemon.
- 🔲 **`bridge-prover-daemon` ETH-submission mode.** A variant (or extra mode) of the existing `bridge-prover-daemon` that posts the per-bundle attestation proof (Circuit 1A *or* 1B, selected by the daemon's path classifier — see [`docs/fallback_path.md`](./docs/fallback_path.md)) + Circuit 2 proof to the Ethereum contract via `verifyBlock(...)` instead of writing them to disk for the modelling verifier daemon to read.

Once all three land, this stub becomes the real end-to-end happy path against a live Ethereum bridge contract.

---

## Where to go next

- **[TECHNICAL_README.md](./TECHNICAL_README.md)** — architecture, daemon internals, IPC, bundle math, troubleshooting.
- **[`acki-nacki-to-eth-bridge-halo2-circuits/README.md`](https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits)** — the five Halo2 circuits and how block finalisation + withdrawal authenticity are encoded.
