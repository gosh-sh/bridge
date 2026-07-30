# Partner note — `USDCBridge.sol` deposit: `chainId` PI + Hermez VkBlob (2026-07-23)

**To:** AN contracts / node team (write access to `gosh-sh/acki-nacki`)
**From:** Pruvendo (bridge / deposit-prover / tvm-sdk side)
**Repo you own:** `gosh-sh/acki-nacki` → `contracts/exchange/USDCBridge.sol` (we have pull-only, so this is your merge + shellnet redeploy)

## TL;DR

The deposit circuit now exposes **12 public inputs** (adds a **proven `chainId`**) and is keyed on the **Hermez** trusted setup. Our side (deposit-prover, deposit-relayer, tvm-sdk `ZKHALO2VERIFYWITHVK` opcode + fixtures) is done and green. To finish the deposit path you need three changes in `USDCBridge.sol` + a recompile/redeploy:

1. Swap `VK_BLOB` → the new 12-PI Hermez VkBlob (`7322fb82…`).
2. Fix `_parsePublicInputs` for the new 12-slot offsets (everything after `contractAddr` shifts by one).
3. **Add a `(chainId → expected bridge Fr)` allowlist** — this is the security point: `chainId` and the bridge address are *proven in-circuit*, and the contract must **reject** any proof whose `(chainId, contractAddr)` pair is not allow-listed, instead of trusting config.

## Why

`chainId` (and the emitting bridge address) must be **provably bound to the event**, not read from the contract. The circuit now:
- extracts `chainId` from the enclosing **EIP-1559 tx (RLP field 0), MPT-bound under `transactionsRoot`**, and exposes it as public input #4;
- binds `contractAddress` (PI #3) to the event emitter via `constrain_equal(tx.to, contractAddress)`.

So the proof *attests* `(chainId, bridge)`. The contract's job is to check that pair against an allowlist.

## New public-input layout (12 × 32-byte LE `Fr`)

```
[0]  depositId
[1]  sender
[2]  amount
[3]  contractAddress     <- L1 bridge that emitted the Deposit (proven = tx.to)
[4]  chainId             <- NEW: proven EIP-1559 chainId (MPT-bound)
[5]  dappIdHigh
[6]  dappIdLow
[7]  anAccountHigh
[8]  anAccountLow
[9]  blockHashHigh
[10] blockHashLow
[11] promiseCommit
```

(Was 11: `chainId` is inserted at slot #4, so `dappId` / `anAccount` shift by one.)

## Change 1 — `VK_BLOB`

Replace the embedded `VK_BLOB` with the new blob:

| | value |
|---|---|
| file | `deposit-prover/fixtures/deposit_10proofs/deposit_vk_blob.bin` (byte-identical to `tvm-sdk/tvm_vm/halo2_test_data/deposit_10proofs/deposit_vk_blob.bin`) |
| size | 5006 B |
| sha256 | `7322fb8257a3ab9024a6cff2317452b91dbf5c5b7dcd7584564eabc293f92541` |
| shape | v2 RLC, `circuit_shape=1`, k=18, `num_advice_per_phase=[17,13]`, 12 PI |
| SRS | **Hermez** Powers of Tau (see "Opcode" below) |

> **Blob rotated twice since the first copy of this note.** If you picked up
> `006cca5d…` or `3e2a2db2…`, discard both. Two circuit changes landed:
> in-circuit soundness fixes (`gosh-sh/bridge` PR
> [#26](https://github.com/gosh-sh/bridge/pull/26) — receipt bound to the MPT
> root the chip actually verified, byte-wise root comparison, `depositId` /
> `amount` range checks), then Prague header support (21-field header table
> including EIP-7685 `requestsHash`, wider `gasLimit` for Arbitrum — without it
> the circuit cannot prove a deposit on Base, Mantle, World Chain, OP Mainnet or
> Sepolia, all of which produce Prague headers today). Public-input layout, size
> and shape are identical across all three — only the VK points and the 10
> regression proofs differ, so nothing below this line changes.

## Change 2 — `_parsePublicInputs` offsets

Current (11-PI):
```solidity
f.depositId    = fr[0];
f.amount       = uint128(fr[2]);
f.contractAddr = fr[3];
f.dappId       = (fr[4] << 128) | fr[5];
f.anAccount    = (fr[6] << 128) | fr[7];
```

New (12-PI — `chainId` at fr[4], everything after shifts +1):
```solidity
f.depositId    = fr[0];
f.amount       = uint128(fr[2]);
f.contractAddr = fr[3];
f.chainId      = fr[4];                    // NEW
f.dappId       = (fr[5] << 128) | fr[6];   // was fr[4]|fr[5]
f.anAccount    = (fr[7] << 128) | fr[8];   // was fr[6]|fr[7]
```
Add `uint256 chainId;` to the `DepositPI` struct.

## Change 3 — `(chainId → expected bridge Fr)` allowlist (the enforcement)

After a successful `gosh.zkhalo2VerifyWithVK(...)`, require the proven `(chainId, contractAddr)` pair is allow-listed. Sketch:

```solidity
// deploy-time config: chainId => expected L1 bridge address as Fr (fr[3] form)
mapping(uint256 => uint256) _expectedBridgeFr;

// in finalizeDeposit, after verify:
uint256 expected = _expectedBridgeFr[f.chainId];
require(expected != 0 && expected == f.contractAddr, ERR_UNKNOWN_SOURCE);
```

Without this, exposing `chainId`/bridge as public inputs buys nothing — the contract would still be implicitly trusting whatever chain/bridge the relayer points at. The allowlist is what makes "proven, not taken from the contract" actually hold on-chain.

> `chainId` values (EIP-155): Sepolia `11155111`, OP `10`, Base `8453`, Arbitrum `42161`, Mantle `5000`, Blast `81457`, World `480` — see `deposit-relayer-daemon/src/supported_chains.rs`.

## Change 4 — recompile + redeploy

- Recompile with the `gosh.zkhalo2VerifyWithVK` builtin (`TVM-Solidity-Compiler` branch `halo2_verify`, `sold 0.79.3`):
  `sold --tvm-version gosh --base-path . exchange/USDCBridge.sol -o exchange/`
- Redeploy to shellnet with the new `.tvc`.

## Opcode / SRS (our side — already done)

- tvm-sdk PR **[tvmlabs/tvm-sdk#279](https://github.com/tvmlabs/tvm-sdk/pull/279)** (branch `pruvendo/deposit-chainid-12pi-hermez-srs`, tip `e6d8c3dd` — carries the rotated `7322fb82…` fixtures; opcode change itself is `dff52fd1`, base `full_dex_and_bridge_test_with_final_halo2_circuit`).
- `ZKHALO2VERIFYWITHVK` verifier params (`build_shared_kzg_params` → `KZG_S_G2_BYTES`) switched from the AN chain ceremony to the **Hermez** `[s]·G2` (`92 8f af b3 …`). Deposit proofs are now keyed on `deposit-prover/data/kzg_params_18.srs` (Hermez), NOT the chain ceremony.
- Legacy Dark DEX `ZKHALO2VERIFY` path is **unchanged** (`DARK_DEX_KZG_S_G2_BYTES` = chain ceremony). The two constants now differ by design.
- Verified: `cargo +nightly test -p tvm_vm --features gosh deposit_rlc` → **3/3 green** (`round_trip_deposit_rlc_real_proof_returns_true`, flipped-byte reject, cache reuse).

## Cross-check (so you can confirm you have the right blob)

```bash
sha256sum tvm_vm/halo2_test_data/deposit_10proofs/deposit_vk_blob.bin
# 7322fb8257a3ab9024a6cff2317452b91dbf5c5b7dcd7584564eabc293f92541
cargo +nightly test -p tvm_vm --features gosh deposit_rlc   # 3/3
```
The 10 proof/PI pairs in `deposit_10proofs/proof_00..09/` are the 12-PI regression set (each `public_inputs.bin` = 384 B = 12 × 32).
