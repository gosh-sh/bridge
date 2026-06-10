---
name: evm-an-deposit-e2e
description: >-
  Cross-repo map and runbook for the EVM→Acki Nacki deposit bridge: where the
  deposit circuit, relayer, TokenBridge contract, ZKHALO2VERIFYWITHVK opcode,
  VK blob, and e2e artifacts live, plus how to regenerate the VK and recompile
  TokenBridge. Use when working on deposit-prover, deposit-relayer-daemon,
  finalizeDeposit / TokenBridge, the ZKHALO2VERIFYWITHVK opcode, the deposit
  public inputs, or the EVM→AN deposit end-to-end flow.
---

# EVM → Acki Nacki deposit e2e

The deposit flow spans **four repos** (siblings under `/home/sergey/Pruvendo/gosh/`).
Touching the public-input layout means changing all of them in lock-step.

## Public inputs (11, since 2026-06-02)

Instance column order — every producer/consumer MUST agree:

```
[ depositId, sender, amount, contractAddress,
  dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
  blockHashHigh, blockHashLow, promiseCommit ]
```

- `dappIdHigh`/`dappIdLow` = the 256-bit AN dApp identifier tag (config-supplied).
- `anAccountHigh`/`anAccountLow` = the 256-bit AN recipient's two 16-byte halves;
  reconstruct as `anAccountHigh << 128 | anAccountLow`.
- The AN recipient is **bound in-circuit** (not a relayer hint). `dappId` replaced
  the earlier `anWorkchain` slot; total count is **11** (not 7 or 10).

## Where each piece lives

| Piece | Location | Notes |
|---|---|---|
| Deposit circuit | `acki-nacki-bridge/deposit-prover/src/circuit_v2.rs` | `num_instance() == vec![11]`; RLC `EthCircuitImpl` + Blake2b SHPLONK proof (NOT Groth16). |
| Relayer | `acki-nacki-bridge/crates/deposit-relayer-daemon/` | `NUM_PUBLIC_INPUTS=11`; `export_vk_blob` + `export_blake2b_proof` (raw Halo2, no gnark wrap). |
| USDCBridge / TokenBridge (AN) | `acki-nacki/contracts/exchange/USDCBridge.sol` | `finalizeDeposit` → `gosh.zkHalo2VerifyWithVK(VK_BLOB, publicInputs, proof)`. Branch **`halo2_circuit_with_vk`** on `gosh-sh/acki-nacki`. |
| Compiled bridge | `acki-nacki/contracts/0.79.3_compiled/exchange/USDCBridge.tvc` + `.abi.json` | Recompile with `sold --tvm-version gosh`. |
| Opcode `ZKHALO2VERIFYWITHVK` (`0xC7 0x4A`) | `tvm-sdk/tvm_vm/src/executor/zk_halo2_with_vk.rs` | VkBlob-driven; RLC `circuit_shape=1`. Branch **`halo2_circuit_with_vk`** on `tvmlabs/tvm-sdk`. Do **not** use `serhii/node-3406-vergrth16-with-vk` (superseded Groth16-era umbrella). |
| tvm-sdk e2e test | `tvm-sdk/tvm_executor/src/transaction_executor.rs::athens_finalize_deposit_reaches_mint` | Loads `TokenBridge.tvc` + `/tmp/deposit_e2e/live_finalize_msg.boc`, drives finalize → opcode → mint. Skips if artifacts missing. |

## e2e artifacts (machine-local, in `/tmp/deposit_e2e/`)

- `deposit_proof_input.json` / `live_deposit_proof_input.json` — `DepositProofInput` (used for keygen + proving).
- `deposit_vk_blob.bin` / `live_deposit_vk_blob.bin` — v2 RLC VkBlob (embed into `TokenBridge.sol`'s `VK_BLOB`).
- `deposit_public_inputs.bin` — `N × 32` LE Fr operand.
- `deposit_proof_blake2b.bin` — raw Blake2b SHPLONK proof.
- `live_finalize_msg.boc` — signed external message that calls `finalizeDeposit`.
- SRS: `acki-nacki-bridge/deposit-prover/data/kzg_bn254_18.srs` (k=18).

## Regenerate the VK / VkBlob

The VK changes whenever `num_instance` changes. From `deposit-prover/`:

```bash
cargo run --release --example export_vk_blob -- \
  --input /tmp/deposit_e2e/deposit_proof_input.json \
  --output /tmp/deposit_e2e/deposit_vk_blob.bin \
  --config-out /tmp/deposit_e2e/deposit_eth_circuit_params.json \
  --degree 18 --max-data-byte-len 256 --max-log-num 20
```

Then regenerate the matching proof + public inputs (`export_blake2b_proof`) and
re-verify the triple (`verify_opcode_triple`). The degree / max-data-byte-len /
max-log-num MUST match across keygen, prove, and verify. After regen, paste the
new `deposit_vk_blob.bin` bytes into `TokenBridge.sol`'s `VK_BLOB` constant.

## Recompile TokenBridge

TVM-Solidity-Compiler is at `/home/sergey/Pruvendo/gosh/TVM-Solidity-Compiler`.
Compile with `sold --tvm-version gosh` (produces `.tvc` + `.abi.json`).

## Build gotchas

- `deposit-prover` is its own cargo workspace (axiom halo2-lib v0.4.1, stable toolchain).
- The tvm-sdk RLC opcode path pulls `axiom-eth` + `snark-verifier-sdk` which need a
  **nightly** toolchain (`trait_alias` for the `gosh` feature) and a unified halo2
  backend via workspace-root `[patch]`. acki-nacki's `Cargo.toml` repoints `tvm_*`
  to the tvm-sdk RLC branch and mirrors its `[patch]` set; some `[patch]` entries
  point at local sibling forks by path (adjust before building elsewhere).

## In-progress work / open items

- Local reference patch: `acki-nacki-bridge/acki-nacki_deposit-rlc-e2e.patch` (the
  earlier **7-input** TokenBridge + node-deps change — predates the 10-input migration).
- PR for this work is tracked as **#248** (deposit-rlc-e2e). `gh` is not
  authenticated locally; confirm the repo/branch before pushing (the TokenBridge
  source is in `gosh-sh/acki-nacki`, branch `pruvendo/deposit-rlc-e2e`).
- `.bak` files (`TokenBridge.sol.pre7input.bak`, dated `.bak`s of `transaction_executor.rs`)
  are scratch snapshots from this experiment — do not commit them.

## Docs

`acki-nacki-bridge/docs/zkhalo2verifywithvk_reference.md` (§11 call-site sketch),
`docs/verifying_eth_proof_on_an.md` (10-input table), `docs/bridge_verification.md`
(DEP-N-1..5), `AGENTS.md` (Deposit Flow step 4).
