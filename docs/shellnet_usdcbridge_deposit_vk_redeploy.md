# Shellnet `USDCBridge` deposit VK redeploy — partner checklist

> **⚠️ STATUS UPDATE (2026-06-23): the redeploy described below is DONE.**
> We verified it ourselves (no partner ping needed) by comparing the deployed
> shellnet account code-hash against the compiled `.tvc` on `gosh-sh/acki-nacki`
> branch `poseidon_dex`:
>
> - Deployed bridge (`0:1a1a…1a1a`, dapp_id `00…00`) code-hash =
>   `b38e934a3c1e23d42c158dd9dbdb785a3739a449d6059646c391f4d93898e154` —
>   **byte-identical** to `contracts/0.79.3_compiled/exchange/USDCBridge.tvc`
>   (branch `poseidon_dex`, updated 2026-06-22).
> - That contract embeds the **correct 11-PI deposit VkBlob** (`circuit_shape=1`
>   RLC, 3597 B, SHA-256 `147efe1425709ade5792469a87eaebc6f0c97dd105e5a4f1633ce7ed1068abaf`)
>   — identical to `deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin`.
> - The **ABI changed** to `finalizeDeposit(bytes proof, bytes publicInputs)`
>   (the contract parses every field out of the operand; §3/§5 below describing a
>   12-arg signature and `_buildPublicInputs` are SUPERSEDED). Canonical ABI:
>   `scripts/ursus/USDCBridge.abi.json`. The relayer submitter forwards the two
>   raw blobs (`crates/deposit-relayer-daemon/src/submitter.rs`).
> - Canonical branches: contract = `poseidon_dex` (NOT the never-existent
>   `halo2_circuit_with_vk`); `tvm_vm` opcode = `tvm-sdk` branch
>   `full_dex_and_bridge_test_with_final_halo2_circuit`.
>
> **Remaining unknown:** live `finalize-one` against shellnet currently returns
> `Message queue is full. Please try to send the message later.` on every attempt
> (node-side queue backpressure, returned *before* execution — not a VK/opcode
> rejection), so opcode-level ACCEPT is not yet confirmed. Retry when shellnet is
> less congested, or raise the bridge-thread queue state with AN ops. The bridge
> account shows `last_trans_lt = 0x0` (no transaction has ever executed on it).
>
> ---
>
> *Original checklist below (pre-redeploy, 2026-06-08) retained for the artefact
> generation + acceptance-criteria recipes, which are still valid.*

## 1. Executive summary

| Layer | Shellnet today | Required |
|-------|----------------|----------|
| **Opcode** | `ZKHALO2VERIFYWITHVK` v1 Base-only reader | v2 RLC reader (`circuit_shape=1`, `EthCircuitImpl<Fr,Noop>`) |
| **Node SRS** | Chain ceremony `kzg_bn254_19.srs` embedded G1/G2 | Deposit VK/proof must be keyed on **same ceremony** (not Hermez) |
| **Contract VK** | ~6308 B Base VkBlob (Circuit 1B fallback) | ~3725 B v2 RLC deposit VkBlob |
| **Public inputs** | 4: `[depositId, sender, amount, srcDappId]` | **11** (see §3) |
| **`finalizeDeposit` ABI** | 6 metadata fields + `proof` | Must passthrough proof-binding fields the contract cannot recompute |

**Smoke already green on shellnet:** fallback fixtures (`circuit_1b_fallback/`) verify
against the *current* contract — proves relayer keys, gas, ABI encoding, and
`tvm-sdk` 3.0 `dapp_id::account_id` wiring are correct. Only the deposit VK/PI
layout blocks the real Sepolia deposit (`depositId=0`).

## 2. Public-input layout (must match everywhere)

Canonical order from `deposit-prover` / `deposit-relayer-daemon` (11 × 32-byte LE `Fr`):

```
[0]  depositId
[1]  sender            (EVM address as 32-byte big-endian → Fr)
[2]  amount
[3]  contractAddress   (Sepolia bridge address, 32-byte padded)
[4]  dappIdHigh        (high 128 bits of configured AN dApp id)
[5]  dappIdLow         (low 128 bits)
[6]  anAccountHigh     (high 128 bits of AN recipient from Deposit event)
[7]  anAccountLow      (low 128 bits)
[8]  blockHashHigh
[9]  blockHashLow
[10] promiseCommit     (keccak coprocessor promise)
```

**Not the same as** the interim `TokenBridge.sol` 7-input layout on branch
`poseidon_dex_with_verify` (that predates the 2026-06-02 dappId + anAccount binding
bump to 11 inputs). Port that work forward — do not deploy the 7-input contract as-is.

### What shellnet `USDCBridge` does today (wrong)

`_buildPublicInputs` returns only 4 elements:

```
[srcDepositId, _srcSenderToFr(srcSender), amount, srcDappId]
```

The embedded `VK_BLOB` is the Circuit 1B fallback attestation VK (`circuit_shape=0`,
4 instances). A deposit proof (11 instances, RLC shape) cannot verify.

## 3. Prerequisites (all must be true before redeploy)

- [ ] **AN node** built from a `tvm_vm` rev that includes:
  - `ZKHALO2VERIFYWITHVK` (`0xC7 0x4A`) 3-operand stack ABI
  - VkBlob v2 header (`version=2`, `circuit_shape` byte at offset 10)
  - `read_rlc_vk` branch (`EthCircuitImpl<Fr, Noop>` + carried `EthCircuitParams`)
  - Unified gosh halo2 backend (`bump-halo2-lib-v0.4.1` or merged equivalent)
  - **Canonical `tvm-sdk` branch: `halo2_circuit_with_vk`** (native Halo2 SHPLONK
    opcode line for shellnet). Do **not** use `serhii/node-3406-vergrth16-with-vk` —
    that is a superseded umbrella branch from the Groth16 era (`VERGRTH16*` opcodes),
    not the current Halo2 deposit path.
  - **Nightly toolchain** for the `gosh` feature (RLC stack needs `trait_alias`)
- [ ] **`sold` fork** with `gosh.zkhalo2VerifyWithVK` builtin (`sold --tvm-version gosh`)
- [ ] **Chain SRS alignment:** deposit keygen/proving uses the node's embedded ceremony
  (`kzg_bn254_19.srs`, downsized to `k=18` for the deposit circuit) — **not**
  `data/kzg_bn254_18.srs` (Hermez). See `deposit-prover/examples/downsize_srs.rs`.
- [ ] **Sepolia bridge address** for `contractAddress` PI binding agreed and constant on
  shellnet (current test: `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82`).

## 4. Artefact generation (partner or joint)

Run on a host with the deposit-prover tree and chain SRS. Example for the pending
Sepolia deposit (`depositId=0`):

```bash
cd deposit-prover

# 1. Fetch witness input (Sepolia RPC + bridge address)
cargo run --release --example fetch_deposit_data -- \
  --rpc-url "https://ethereum-sepolia-rpc.publicnode.com" \
  --bridge-address 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 \
  --deposit-id 0 \
  --tx-hash 0x9ac341666f70d55780f289187c11a0537a52f7381e5c4b1ebe6f671314552adf \
  --log-index 0 \
  --dapp-id-high 0x1a1a1a1a1a \
  --dapp-id-low 0 \
  --output /tmp/deposit_e2e/deposit_proof_input.json

# 2. Export v2 RLC VkBlob (embed into USDCBridge.sol)
cargo run --release --example export_vk_blob -- \
  --input /tmp/deposit_e2e/deposit_proof_input.json \
  --output /tmp/deposit_e2e/deposit_vk_blob.bin \
  --config-out /tmp/deposit_e2e/deposit_eth_circuit_params.json \
  --degree 18 --max-data-byte-len 256 --max-log-num 20

# 3. Export Blake2b SHPLONK proof + 11×32 LE public inputs
cargo run --release --example export_blake2b_proof -- \
  --input /tmp/deposit_e2e/deposit_proof_input.json \
  --vk-blob /tmp/deposit_e2e/deposit_vk_blob.bin \
  --proof-out /tmp/deposit_e2e/deposit_proof_blake2b.bin \
  --public-inputs-out /tmp/deposit_e2e/deposit_public_inputs.bin \
  --degree 18 --max-data-byte-len 256 --max-log-num 20

# 4. In-process opcode reproduction (must print ACCEPTED)
cargo run --release --example verify_opcode_triple -- \
  --vk-blob /tmp/deposit_e2e/deposit_vk_blob.bin \
  --public-inputs /tmp/deposit_e2e/deposit_public_inputs.bin \
  --proof /tmp/deposit_e2e/deposit_proof_blake2b.bin
```

**Deliverables to embed in the contract:**

| File | Size (approx) | Action |
|------|---------------|--------|
| `deposit_vk_blob.bin` | ~3725 B | Replace `USDCBridge` `VK_BLOB` constant |
| `deposit_eth_circuit_params.json` | — | Archive alongside contract source (opcode cache key) |

Record SHA-256 of the VkBlob in the redeploy PR for reproducibility.

**Circuit params that MUST stay pinned across keygen / prove / verify / opcode:**

- `degree = 18` (k=18)
- `max_data_byte_len = 256`
- `max_log_num = 20`
- Blake2b transcript (`transcript_kind = 0x00`)

## 5. Contract changes (`USDCBridge.sol`)

Start from the `TokenBridge.sol` / `USDCBridge.sol` work on acki-nacki branch
`halo2_circuit_with_vk` (the live Halo2 integration line). Older branches
(`poseidon_dex_with_verify`, `pruvendo/deposit-rlc-e2e`) may still have useful
fragments but are not the shellnet source of truth. Extend to **11 public inputs**
if the branch still has the interim 7-input layout.

### 5.1 Replace `VK_BLOB`

- Remove the Circuit 1B fallback blob (~6308 B, `circuit_shape=0`, 4 instances).
- Embed the chain-SRS `deposit_vk_blob.bin` from §4 (~3725 B, `circuit_shape=1`).

### 5.2 Extend `_buildPublicInputs`

Build **11** strict-bounded LE field elements (each `< FR_MODULUS`), in the order
from §2. Suggested mapping from `finalizeDeposit` call arguments:

| PI index | Source |
|----------|--------|
| `depositId` | `srcDepositId` (call arg) |
| `sender` | `_srcSenderToFr(srcSender)` — **require `srcSender.length == 32`** |
| `amount` | `amount` (call arg) |
| `contractAddress` | `_srcSenderToFr(srcContract)` — 32-byte padded Sepolia bridge |
| `dappIdHigh` | call arg or `USDCBridge` configured constant (must match proof) |
| `dappIdLow` | call arg or constant |
| `anAccountHigh` | high 128 bits of `recipient_an` |
| `anAccountLow` | low 128 bits of `recipient_an` |
| `blockHashHigh` | **passthrough** call arg (relayer cannot forge — bound in proof) |
| `blockHashLow` | **passthrough** call arg |
| `promiseCommit` | **passthrough** call arg |

Fields marked **passthrough** cannot be recomputed on-chain; the relayer must
forward them from the decoded `public_inputs` operand. The contract should
`require` they match what the proof commits to (the opcode enforces this, but
explicit checks improve debuggability).

### 5.3 Extend `finalizeDeposit` signature

Minimum additional parameters beyond today's shellnet ABI:

```solidity
function finalizeDeposit(
    bytes proof,
    uint256 srcDepositId,
    bytes srcSender,        // 32-byte padded EVM depositor
    uint128 amount,
    uint32 tokenId,
    bytes srcContract,      // 32-byte padded Sepolia bridge address  [NEW]
    uint256 dappIdHigh,     // [NEW] or sole source if low half is 0
    uint256 dappIdLow,      // [NEW]
    uint256 recipient_an,   // full 256-bit AN account (split to high/low in builder)
    uint256 blockHashHigh,  // [NEW] passthrough
    uint256 blockHashLow,   // [NEW] passthrough
    uint256 promiseCommit   // [NEW] passthrough
) public { ... }
```

Exact ordering can follow existing shellnet conventions; the relayer ABI JSON will
be updated to match. **Do not** keep `srcDappId` as a single scalar in PI slot 3 —
that was the pre-2026-05-28 bug (`contractAddress` and `srcDappId` were swapped).

### 5.4 On-chain binding checks (recommended)

Before calling `gosh.zkhalo2VerifyWithVK`:

- `require(dappIdHigh/Low == configuredShellnetDappId)` — proof targets this bridge dApp.
- `require(recipient_an != 0)` — mirrors EVM-side `InvalidAnAccount`.
- `require(!usedDepositIds[srcDepositId])` — nullifier (already present).
- Optional: `require(contractAddress_fr == expectedSepoliaBridgeFr)` — defense in depth.

### 5.5 Recompile and publish

```bash
sold --tvm-version gosh contracts/exchange/USDCBridge.sol \
  -o contracts/0.79.3_compiled/exchange/
```

Deliver updated `USDCBridge.tvc` + `USDCBridge.abi.json` to the shellnet deployer.

## 6. Shellnet deployment

- [ ] Deploy upgraded `USDCBridge.tvc` (in-place upgrade or new address — if new,
      update `AN_TOKEN_BRIDGE` in relayer env).
- [ ] Confirm shellnet node version exposes RLC opcode (smoke with
      `verify_opcode_triple` artefacts against a local/shellnet executor).
- [ ] Fund relayer multisig if needed (`AN_SENDER` on ursus:
      `20c2db9c…::20c2db9c…`).
- [ ] Publish the Sepolia `contractAddress` constant the contract enforces.

**Current shellnet bridge address (zero dapp_id):**

```
0000000000000000000000000000000000000000000000000000000000000000::1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a
```

## 7. Relayer alignment (Pruvendo side — after ABI lands)

Once the new `USDCBridge.abi.json` is published, we update
`crates/deposit-relayer-daemon/src/submitter.rs::build_finalize_deposit_params`
to forward the passthrough fields:

- `srcContract` — 32-byte padded `event.source_contract`
- `blockHashHigh` / `blockHashLow` — from `bundle.parsed`
- `promiseCommit` — from `bundle.parsed`
- `dappIdHigh` / `dappIdLow` — from `bundle.parsed` (not a merged `srcDappId`)

No change needed to the proof pipeline itself (already emits 11 PI).

## 8. Post-deploy verification

### 8.1 Contract-level (partner)

Re-run the Python harness pattern from `acki-nacki/tests/exchange/test_usdcbridge_finalize.py`
but with **deposit** fixtures instead of `circuit_1b_fallback/`:

1. Load `deposit_vk_blob.bin` bytes into the deployed contract's `VK_BLOB` (or use
   live deployment).
2. Call `finalizeDeposit` with `deposit_proof_blake2b.bin` + decoded scalars.
3. Assert `exit_code == 0` and USDC balance credited to `recipient_an`.

### 8.2 Live E2E (joint)

| Step | Command / check | Expected |
|------|-----------------|----------|
| 1 | Sepolia `Deposit` already mined (`depositId=0`) | Event visible |
| 2a | **Production path:** `BRIDGE_DEPLOY_BLOCK=11025180 deposit-relayer prove-one --deposit-id 0 …` on Ursus | `eth_getLogs` discovery → `proof` + `public_inputs` written |
| 2b | **Operator fast path** (known tx; skips `getLogs`): `--tx-hash 0x9ac34…2adf --log-index 2` | Same operands; use for reprove only, not sign-off |
| 3 | `deposit-relayer daemon` (no `--dry-run`, `BRIDGE_DEPLOY_BLOCK` set) | `finalizeDeposit` **exit_code 0** |
| 4 | Query recipient ECC balance on shellnet | +1 USDC (token id `3`) |

**prove-one examples** (env from `scripts/ursus/deposit-relayer.env.example`):

```bash
# Production discovery (acceptance gate — needs production/paid Sepolia RPC)
BRIDGE_DEPLOY_BLOCK=11025180 AN_DAPP_ID=0x1a1a1a1a1a \
  deposit-relayer prove-one \
  --rpc-url "$RPC_URL" \
  --bridge-address 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 \
  --deposit-id 0 \
  --deposit-prover-dir "$DEPOSIT_PROVER_DIR" \
  --out-dir /tmp/deposit-prove-one-0

# Operator fast path (rate-limited RPCs; does not exercise getLogs)
AN_DAPP_ID=0x1a1a1a1a1a deposit-relayer prove-one \
  --rpc-url "$RPC_URL" \
  --bridge-address 0x99c37fb75326ae6953ebbbdcd261ec331df4ce82 \
  --deposit-id 0 \
  --tx-hash 0x9ac341666f70d55780f289187c11a0537a52f7381e5c4b1ebe6f671314552adf \
  --log-index 2 \
  --deposit-prover-dir "$DEPOSIT_PROVER_DIR" \
  --out-dir /tmp/deposit-prove-one-0
```

**Offline regression** (no RPC): `cargo test -p deposit-relayer-daemon --test log_index_mapping` — Sepolia fixture asserts block `logIndex` 271 → receipt position 2.

**Pending test vectors:**

| Field | Value |
|-------|-------|
| Sepolia bridge | `0x99c37fb75326ae6953ebbbdcd261ec331df4ce82` |
| Deposit tx | `0x9ac341666f70d55780f289187c11a0537a52f7381e5c4b1ebe6f671314552adf` |
| `depositId` | `0` |
| AN recipient | `beef504cfac7c8a8728d9c0a00deca826fd4e8168e5cc9c5f49fd066b5e2a5b1` |
| `AN_DAPP_ID` | `0x1a1a1a1a1a` |
| `AN_TOKEN_ID` | `3` |
| `BRIDGE_DEPLOY_BLOCK` | `11025180` (shellnet Sepolia bridge; required for production `eth_getLogs` scans) |
| Receipt log index | `2` (receipt-local; block-global `logIndex` = 271) |

## 9. Acceptance criteria (sign-off)

- [ ] `verify_opcode_triple` **ACCEPTED** on chain-SRS artefacts (§4 step 4).
- [ ] `cargo test -p deposit-relayer-daemon --test log_index_mapping` green (block → receipt log-index mapping).
- [ ] `live_log_discovery` green on Ursus/production RPC with `BRIDGE_DEPLOY_BLOCK` set (production `eth_getLogs` path).
- [ ] On-node executor test green (`round_trip_deposit_rlc_real_proof_returns_true` or
      equivalent on the shellnet node build).
- [ ] `USDCBridge.tvc` embeds the new VkBlob (SHA-256 recorded).
- [ ] `_buildPublicInputs` returns **11** elements in §2 order.
- [ ] `finalizeDeposit` with real Sepolia `depositId=0` proof returns **exit_code 0**
      on shellnet.
- [ ] Recipient USDC balance increases by deposited amount.
- [ ] Duplicate `finalizeDeposit` for same `srcDepositId` rejected (nullifier).

## 10. References

| Doc / path | Contents |
|------------|----------|
| `docs/deposit_finalize_vk_gap_2026-05-28.md` | VK gap analysis, Option 2 RLC opcode plan, chain SRS lesson |
| `docs/zkhalo2verifywithvk_reference.md` §15 | VkBlob v2 `circuit_shape`, RLC reader |
| `.cursor/skills/evm-an-deposit-e2e/SKILL.md` | Cross-repo map (update PI count to 11 after redeploy) |
| `deposit-prover/examples/export_vk_blob.rs` | VkBlob producer |
| `deposit-prover/examples/export_blake2b_proof.rs` | Blake2b proof producer |
| `deposit-prover/examples/verify_opcode_triple.rs` | Pre-flight opcode check |
| `crates/deposit-relayer-daemon/src/types.rs` | `NUM_PUBLIC_INPUTS = 11` |
| `crates/deposit-relayer-daemon/src/source.rs` | `EthLogSource`, `receipt_log_index_from_block_log`, `BRIDGE_DEPLOY_BLOCK` |
| `scripts/ursus/deposit-relayer.env.example` | Ursus env template (`BRIDGE_DEPLOY_BLOCK`, shellnet GraphQL) |
| `acki-nacki/tests/exchange/test_usdcbridge_finalize.py` | Shellnet finalize harness (fallback fixtures today) |

## 11. Open questions for partner

1. **Upgrade path:** in-place `USDCBridge` code upgrade on shellnet vs deploy fresh
   address (affects `AN_TOKEN_BRIDGE` config on ursus).
2. **`dappId` source:** hardcode in contract vs relayer-supplied call args (relayer
   already proves the configured tag).
3. **Node merge:** confirm shellnet tracks `tvm-sdk` branch `halo2_circuit_with_vk`
   (Halo2 `ZKHALO2VERIFYWITHVK`, not the retired Groth16 `VERGRTH16*` line).
4. **Gosh halo2 fork:** merge `bump-halo2-lib-v0.4.1` to public `main` so consumers
   can drop local `[patch]` paths.

---

*Prepared from ursus shellnet E2E session 2026-06-07/08. Relayer host:
`ubuntu@ursus-tools.dev:/home/ubuntu/bridge-e2e/`.*
