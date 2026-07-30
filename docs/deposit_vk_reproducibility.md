> # ⚠️ SUPERSEDED BY TRACK-2 CHAIN-BINDING (2026-07-23)
> All "11 PI" / `num_instance = [11]` claims below predate Track 2. The current
> `deposit-prover` circuit exposes **12 PI** (adds `chainId` at slot 4,
> `NUM_PUBLIC_INPUTS = 12` in `deposit-prover/src/types.rs`) and produces VkBlob
> `7322fb82…93f92541` (5006 B, rotated 2026-07-30 by the Prague header fix;
> earlier 12-PI blobs `de1dd3ab…7dd8d1` / `006cca5d…191dec05` / `3e2a2db2…bf0d049c`
> are superseded).
> The reproducibility mechanics below (locked
> `Cargo.lock`, witness-independent circuit, Hermez SRS pin) still apply — only
> the PI count / VkBlob hash change. Canonical Track-2 hash + fixtures live in
> `docs/deposit_max_key_byte_len.md` and `docs/partner_note_usdcbridge_chainid_hermez_2026-07-23.md`.

> # ⚠️ PARTIALLY SUPERSEDED (corrected 2026-06-25)
> The **`Cargo.lock`-is-now-tracked** reproducibility fix (§4.1) is correct and
> retained. But the **`b1e5ce0b…` "canonical VkBlob" is NOT the production VK**:
> it was keyed on the Hermez SRS and is rejected by the opcode, and lock-tracking
> alone did not make a single embedded VK verify real deposits. The actual blocker
> was a redundant `load_constant(contract_address)` in `circuit_v2.rs` + an
> unpinned keccak capacity (both fixed in `deposit-prover`, no `axiom-eth` change).
> The production VkBlob is now the **`20cf9018…`** family (cap=64, witness-independent).
> **See `docs/deposit_vk_witness_independence.md`.**

# Deposit VK reproducibility + USDCBridge redeploy request

**Status (2026-06-25):** the deposit VkBlob embedded in the **currently deployed**
shellnet `USDCBridge` is **not reproducible** from any buildable prover state.
This doc records the root cause, the new reproducible canonical VkBlob, and the
redeploy steps the AN/partner side needs to take so EVM→AN deposits (including
the already-mined Sepolia `depositId=1`) can be finalised.

---

## 1. The problem in one paragraph

`USDCBridge.finalizeDeposit(proof, publicInputs)` verifies the proof with
`gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)`, where `VK_BLOB` is a
**deploy-time constant embedded in the contract**. A proof only verifies if it
was produced against the *exact same* verifying key. The deployed contract
embeds VK `147efe14…068abaf`, but **no currently-buildable `deposit-prover`
reproduces that VK** — so every real deposit proof (depositId=1 included) is
rejected by the opcode, regardless of which depositId it is.

## 2. Evidence

| Prover state | axiom-eth | snark-verifier | VK produced | size |
|---|---|---|---|---|
| `d529b61` (made the on-chain VK) | `path = ../../axiom-eth/axiom-eth` (→ `687a6da`) | axiom-crypto `v0.1.7-git` | **`147efe14…068abaf`** ← **on-chain** | 3597 B (v2) |
| HEAD `11e98f5` (git-pinned) | `gosh-sh/axiom-eth @ 1d61be0` | `gosh-sh/snark-verifier @ 164bd42` | `b1e5ce0b…800498bd` | 3597 B (v2) |
| ursus prebuilt (Jun 9) | path `687a6da` | axiom-crypto `v0.1.7-git` | `5e2490a6…f5779` | 3725 B (**v1**) |

Live shellnet check (2026-06-25): `USDCBridge` account `0:1a1a…1a1a`
(dapp `0:00…00`) → `code_hash = b38e934a3c1e23d42c158dd9dbdb785a3739a449d6059646c391f4d93898e154`,
`acc_type=1`, balance ≈ 1000 tokens. Per the e2e skill this code-hash embeds
VK `147efe14…`.

## 3. Root cause

The on-chain VK came from commit `d529b61` (2026-06-02), whose
`deposit-prover/Cargo.toml` used **local-path dependencies**
(`axiom-eth`, `halo2-base/ecc/zkevm-hashes`) plus a **floating** `halo2-axiom`
branch — and the only thing that froze the exact working dependency closure was
`deposit-prover/Cargo.lock`, **which was `.gitignore`d** (never committed).

After the migration to git-pinned deps, a fresh resolve of the `d529b61`
manifest no longer compiles (`axiom-eth@687a6da` fails against the drifted
transitive crates.io closure: `QuantumCell: From<&AssignedValue>`,
`BaseCircuitParams: Hash`, …). `halo2-axiom` is **not** the culprit — it has
been stable at `1f28ded` since 2026-04-16. The breakage is untracked-lock +
transitive drift. Without the original lock, `147efe14` cannot be rebuilt.

## 4. Fix applied in this repo (reproducibility)

1. **`deposit-prover/Cargo.lock` is now tracked** (removed from `.gitignore`).
   The deposit VK is a deterministic function of the full dependency closure, so
   the lock MUST be pinned. This is the durable fix for the class of bug above.
2. **New canonical VkBlob regenerated from the fully git-pinned HEAD build** and
   committed as the `deposit_10proofs` fixture. The VK is **deterministic**:
   two independent keygens on n14 both produced `b1e5ce0b…800498bd`.

### New canonical build provenance (reproducible)

| Item | Value |
|---|---|
| Repo commit | `11e98f5` (+ this change) |
| `axiom-eth` | `gosh-sh/axiom-eth` `gosh-stable-rlcmanager-assignment` @ `1d61be0` |
| `snark-verifier(-sdk)` | `gosh-sh/snark-verifier` @ `164bd42` |
| `halo2-lib` (`[patch]`) | `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` `bump-halo2-lib-v0.4.1` @ `026e176` |
| `halo2-axiom` (`[patch.crates-io]`) | `gosh-sh/halo2-axiom` `main` @ `1f28ded` |
| SRS | `deposit-prover/data/kzg_params_18.srs` (sha256 `ca97cea5…`, k=18) — ⚠️ **WRONG ceremony.** This is the Hermez SRS; its `s_g2` (`928fafb3…`) does **not** match the AN opcode's embedded `KZG_S_G2_BYTES` (`c6028acf…`), which is why `b1e5ce0b` is opcode-rejected. The production build must use the **chain** SRS `params/kzg_bn254_18.srs` (`s_g2 = c6028acf…`). See `deposit_vk_witness_independence.md`. |
| Circuit params | `num_instance = [11]`, `max_data_byte_len=256`, `max_log_num=20`, `degree=18` |
| **VkBlob (v2 RLC)** | **`b1e5ce0b3cafa697421d39e8f9728ccb7b15d690f2a5e579df12d6af800498bd`** (3597 B) |

Reproduce:

```bash
cd deposit-prover
cargo build --release --locked --example export_vk_blob
./target/release/examples/export_vk_blob \
  --input fixtures/deposit_10proofs/proof_00/input.json \
  --output /tmp/vk.bin --config-out /tmp/params.json \
  --degree 18 --max-data-byte-len 256 --max-log-num 20
sha256sum /tmp/vk.bin   # -> b1e5ce0b…800498bd
```

## 5. The 11 public inputs (unchanged)

`11 × 32-byte LE Fr`, column order (every producer/consumer must agree):

```
[0] depositId      [1] sender        [2] amount         [3] contractAddress
[4] dappIdHigh     [5] dappIdLow     [6] anAccountHigh  [7] anAccountLow
[8] blockHashHigh  [9] blockHashLow  [10] promiseCommit
```

## 6. Redeploy request (AN / partner side)

To make EVM→AN deposits finalisable against a **reproducible** prover:

1. **Embed the new VkBlob** `b1e5ce0b…` into `USDCBridge.sol`'s `VK_BLOB`
   constant (acki-nacki branch `poseidon_dex`,
   `contracts/exchange/USDCBridge.sol`). Source bytes:
   `deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin`.
2. **Recompile** with `sold --tvm-version gosh` → new `.tvc` + `.abi.json`
   (`contracts/0.79.3_compiled/exchange/`).
3. **Redeploy** the `.tvc` to shellnet at `0:1a1a…1a1a` (or a fresh address;
   update `AN_TOKEN_BRIDGE` in the relayer env if it changes).
4. **Re-sync the opcode test fixtures** into `tvm-sdk`:
   `scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh` (the
   `deposit_10proofs/*` set in `tvm_vm/halo2_test_data/` must match the new VK).
5. Confirm via `cargo test -p tvm_vm test_zkhalo2_with_vk_deposit_10_real_proofs
   --features gosh` (all 10 must pass against `b1e5ce0b`).

### ⚠️ SRS alignment — verify before redeploy

The AN-side opcode builds its verifier `ParamsKZG<Bn256>` at runtime from a
small set of **globally-embedded KZG points**. The prover SRS used here is
`data/kzg_params_18.srs` (sha256 `ca97cea5…`, k=18). The redeploy is only sound
if those embedded points correspond to **this** SRS (same ceremony, k=18). If
the opcode currently embeds points from a different ceremony, either (a) the
prover must re-key against the opcode's SRS, or (b) the opcode's embedded points
must be updated to match `ca97cea5…`. This is the one cross-repo invariant that
a VK match alone does **not** guarantee — confirm it explicitly.

## 7. After redeploy — finalise depositId=1 (no new deposit needed)

The Sepolia `Deposit` event for `depositId=1`
(`0x7f2b376dfdae0af77a626d1d14585d5f517c0e072e8c90f93f455a1a6057112c`,
block `11115635`) is permanent, so it can be proven anytime with the new prover:

```bash
# 1. prove against b1e5ce0b
deposit-relayer prove-one --rpc-url <SEPOLIA> --bridge-address 0x99c3…ce82 \
  --deposit-id 1 --from-block 11115630 \
  --deposit-prover-dir deposit-prover --dapp-id 0x1a1a1a1a1a --out-dir ./out/dep1

# 2. finalise live on shellnet (relayer must be rebuilt with `FinalizeOne` +
#    the `tvm-sdk` feature; the Jun-11 ursus binary predates FinalizeOne)
deposit-relayer finalize-one --bundle-dir ./out/dep1 \
  --an-graphql-url https://shellnet.ackinacki.org/graphql \
  --an-keys-path <relayer.keys.json> --an-bridge-abi-path <USDCBridge.abi.json> \
  --an-token-bridge <dapp::acct> --an-sender <AN_SENDER>
```
