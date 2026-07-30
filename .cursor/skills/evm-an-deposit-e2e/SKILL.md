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

## Public inputs (12, since 2026-07-23 — `chainId` added)

Instance column order — every producer/consumer MUST agree:

```
[ depositId, sender, amount, contractAddress, chainId,
  dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
  blockHashHigh, blockHashLow, promiseCommit ]
```

- `chainId` (slot #4, added 2026-07-23) = the EIP-1559 RLP field-0 value,
  extracted from the enclosing tx and **MPT-bound under `transactionsRoot`** —
  a *proven* value, NOT VK-baked and NOT read from the contract. The AN-side
  `USDCBridge` must allowlist `(chainId → expected bridge Fr)`.
- `contractAddress` (slot #3, bridge address) is bound in-circuit via
  `constrain_equal(tx.to, contractAddress)` — provably the emitter of the Deposit
  event, not a contract-supplied value.
- `dappIdHigh`/`dappIdLow` = the 256-bit AN dApp identifier tag (config-supplied).
- `anAccountHigh`/`anAccountLow` = the 256-bit AN recipient's two 16-byte halves;
  reconstruct as `anAccountHigh << 128 | anAccountLow`.
- The AN recipient is **bound in-circuit** (not a relayer hint). `dappId` replaced
  the earlier `anWorkchain` slot; total count is **12** (was 11 before `chainId`).

## Where each piece lives

| Piece | Location | Notes |
|---|---|---|
| Deposit circuit | `acki-nacki-bridge/deposit-prover/src/circuit_v2.rs` | `num_instance() == vec![12]` (`chainId` at slot #4, MPT-bound to the EIP-1559 tx); RLC `EthCircuitImpl` + Blake2b SHPLONK proof (NOT Groth16). Keyed on **Hermez** SRS. |
| Relayer | `acki-nacki-bridge/crates/deposit-relayer-daemon/` | `NUM_PUBLIC_INPUTS=12`; decodes `chainId` at `scalars[4]`; `export_vk_blob` + `export_blake2b_proof` (raw Halo2, no gnark wrap). |
| USDCBridge / TokenBridge (AN) | `acki-nacki/contracts/exchange/USDCBridge.sol` | ABI is **`finalizeDeposit(bytes proof, bytes publicInputs)`** — the contract parses all deposit fields out of the operand itself and calls `gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)`. Branch **`poseidon_dex`** on `gosh-sh/acki-nacki` (NOT `halo2_circuit_with_vk`, which never existed). **Currently deployed to shellnet** (code-hash `b38e934a…898e154`; embedded `VK_BLOB` = `147efe14…068abaf`) — but that VK was keygen'd from **synthetic 1-node fixtures** and **cannot verify real deposits**. **VK swap required:** the deposit circuit had a witness-dependent VK bug (per-deposit `contractAddress` leaked into a fixed column + unpinned keccak capacity), both now fixed in `deposit-prover` (see `docs/deposit_vk_witness_independence.md`). The new **witness-independent** production VkBlob is **`20cf9018…647a39`** (3982 B, `[13,10]`, cap=64); the recompiled `.tvc` has code-hash **`818fb76d…a9abc4`** (toolchain-validated: old source reproduces the deployed `b38e934a` exactly). Pending partner shellnet redeploy. |
| Compiled bridge | `acki-nacki/contracts/0.79.3_compiled/exchange/USDCBridge.tvc` + `.abi.json` | Recompile with `sold --tvm-version gosh --base-path . exchange/USDCBridge.sol -o exchange/` (run from `contracts/`). Canonical ABI snapshot mirrored at `acki-nacki-bridge/scripts/ursus/USDCBridge.abi.json`. ABI unchanged across the VK swap. |
| Opcode `ZKHALO2VERIFYWITHVK` (`0xC7 0x4A`) | `tvm-sdk/tvm_vm/src/executor/zk_halo2_with_vk.rs` (builds params via `build_shared_kzg_params` → `zk_halo2_utils.rs::KZG_S_G2_BYTES`) | VkBlob-driven (reads PI count from the VkBlob — no opcode change for 11→12 PI). RLC `circuit_shape=1`. **`KZG_S_G2_BYTES` = Hermez `[s]·G2` since 2026-07-23** (`92 8f af b3 …`), so it accepts Hermez-keyed deposit proofs; the legacy Dark DEX path keeps `DARK_DEX_KZG_S_G2_BYTES` = chain ceremony. Verify green: `cargo +nightly test -p tvm_vm --features gosh deposit_rlc` (3/3). Branch **`full_dex_and_bridge_test_with_final_halo2_circuit`** on `tvmlabs/tvm-sdk`. Do **not** use `halo2_circuit_with_vk` (never existed) or `serhii/node-3406-vergrth16-with-vk` (superseded Groth16-era umbrella). |
| tvm-sdk e2e test | `tvm-sdk/tvm_executor/src/transaction_executor.rs::athens_finalize_deposit_reaches_mint` | Loads `TokenBridge.tvc` + `/tmp/deposit_e2e/live_finalize_msg.boc`, drives finalize → opcode → mint. Skips if artifacts missing. |

## e2e artifacts (machine-local, in `/tmp/deposit_e2e/`)

- `deposit_proof_input.json` / `live_deposit_proof_input.json` — `DepositProofInput` (used for keygen + proving).
- `deposit_vk_blob.bin` / `live_deposit_vk_blob.bin` — v2 RLC VkBlob (embed into `TokenBridge.sol`'s `VK_BLOB`).
- `deposit_public_inputs.bin` — `N × 32` LE Fr operand.
- `deposit_proof_blake2b.bin` — raw Blake2b SHPLONK proof.
- `live_finalize_msg.boc` — signed external message that calls `finalizeDeposit`.
- **SRS = Hermez (since 2026-07-23). Forget the chain ceremony for deposits.**
  Deposit proofs are keyed on the **Hermez Perpetual Powers of Tau** SRS
  `acki-nacki-bridge/deposit-prover/data/kzg_params_18.srs` (k=18, `s_g2` head
  `92 8f af b3 …`, tail `… 5c 69 00`). Get it via `./download_trusted_setup.sh`
  (downloads the 33 MB Hermez file). Do **not** downsize / use the chain ceremony
  `params/kzg_bn254_{18,19}.srs` (`c6 02 8a cf …`) for deposit proofs anymore.
  The tvm-sdk `ZKHALO2VERIFYWITHVK` opcode was switched to the Hermez `[s]·G2` on
  2026-07-23 (`tvm_vm/src/executor/zk_halo2_utils.rs::KZG_S_G2_BYTES`) so it now
  accepts Hermez-keyed deposit proofs. (The legacy Dark DEX `ZKHALO2VERIFY` path
  still uses `DARK_DEX_KZG_S_G2_BYTES` = chain ceremony — do not touch it.)

## Regenerate the VK / VkBlob

The VK changes whenever `num_instance` changes. From `deposit-prover/` — first
ensure the **Hermez** SRS is present (`./download_trusted_setup.sh` → `data/kzg_params_18.srs`;
do NOT downsize the chain ceremony):

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

**Witness-independence (2026-06-25):** the VK no longer depends on the deposit's
`contractAddress` or MPT proof depth — `prover.rs` pins keccak capacity
(`FIXED_KECCAK_CAPACITY = 64`) and `circuit_v2.rs` no longer loads the address as a
fixed-column constant (binding preserved via public input #3). Real deposits of
different bridge address / MPT depth AND the synthetic 1-node fixtures all produce
the **byte-identical** production VkBlob. Current production VkBlob (12-PI, Hermez,
5006 B) = **`3e2a2db2…bf0d049c`** — rotated 2026-07-28 by the PR #26 soundness fixes
(receipt bound to the verified MPT root, byte-wise root compare, `depositId`/`amount`
range checks); before that `006cca5d…191dec05`, which itself superseded the 11-PI
chain-ceremony `20cf9018…` on 2026-07-23 when `chainId` was added + the SRS moved to
Hermez. Note that only the constraint system moves — PI layout, count and shape are
stable across the last two rotations. Regenerate the
whole `deposit_10proofs` regression set in one shot with
`cargo run --release --example export_deposit_proof_set -- --set-dir fixtures/deposit_10proofs --count 10 --degree 18 --max-data-byte-len 256 --max-log-num 20`
then sync to tvm-sdk (`acki-nacki-bridge/scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`).
NB: `get_or_create_proving_key` now writes a **shape-fingerprinted** PK filename so a
circuit-shape change can never silently load a stale PK.

## Recompile TokenBridge

TVM-Solidity-Compiler is at `/home/sergey/Pruvendo/gosh/TVM-Solidity-Compiler`.
**The `gosh.zkhalo2VerifyWithVK` builtin only exists on branch `origin/halo2_verify`**
(`c7725d64`, `sold 0.79.3`); the default branch's `sold` will fail with
`Member "zkhalo2VerifyWithVK" not found`. Build that branch (C++ build needs a
`compiler/commit_hash.txt` when building outside a git checkout) and compile from
the `contracts/` root with the base path so relative imports resolve:

```bash
sold --tvm-version gosh --base-path . exchange/USDCBridge.sol -o exchange/
```

Validate the toolchain by recompiling the *unmodified* old source and confirming
its code-hash matches the deployed `b38e934a…898e154` (it does), then trust the
new `.tvc`.

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
