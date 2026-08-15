# EVM → Acki Nacki deposit E2E runbook

Cross-repo operator guide for the deposit bridge. The deposit flow spans **four repositories** (siblings under the Acki Nacki monorepo layout). Changing the public-input layout requires coordinated updates in all of them.

## Public inputs (12)

Instance column order — every producer and consumer must agree:

```
[ depositId, sender, amount, contractAddress, chainId,
  dappIdHigh, dappIdLow, anAccountHigh, anAccountLow,
  blockHashHigh, blockHashLow, promiseCommit ]
```

- `chainId` — EIP-155 source network (public input slot **4**, Track-2 chain binding).
- `dappIdHigh` / `dappIdLow` — 256-bit AN dApp identifier (config-supplied by relayer, not in the Ethereum event).
- `anAccountHigh` / `anAccountLow` — 256-bit AN recipient as two 16-byte halves; reconstruct as `(anAccountHigh << 128) | anAccountLow`.
- `promiseCommit` — slot **11**; circuit commitment appended by `EthCircuitImpl` (not relayer event-bound — TD-21).
- The AN recipient is **bound in-circuit**. The Ethereum `Deposit` event carries `anWorkchain` + `anAccount`; the circuit splits the account into high/low public inputs.

## Where each piece lives

| Piece | Location | Notes |
|-------|----------|-------|
| Deposit circuit | `deposit-prover/src/circuit_v2.rs` | `DEPOSIT_PUBLIC_INPUT_LAYOUT` (12 Fr); `num_instance() == vec![12]` |
| Relayer | `crates/deposit-relayer-daemon/` | `NUM_PUBLIC_INPUTS = 12`; subprocess to `export_vk_blob` + `export_blake2b_proof` |
| USDCBridge / TokenBridge (AN) | `acki-nacki/contracts/exchange/USDCBridge.sol` | `finalizeDeposit(bytes proof, bytes publicInputs)` → `gosh.zkhalo2VerifyWithVK(VK_BLOB, …)` |
| Opcode | `tvm-sdk` branch `full_dex_and_bridge_test_with_final_halo2_circuit` | `ZKHALO2VERIFYWITHVK` (`0xC7 0x4A`), 3-operand stack ABI |
| Opcode reference | [../zk/an-side/zkhalo2verifywithvk_reference.md](../zk/an-side/zkhalo2verifywithvk_reference.md) | Frozen wire format |
| Production VkBlob | [../zk/an-side/deposit_vk_witness_independence.md](../zk/an-side/deposit_vk_witness_independence.md) | Witness-independent VK `9dacd998…` (5006 B, 12 PI) |
| Shellnet redeploy recipe | [shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md](shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md) | Full redeploy steps |

**Status (2026-07):** Shellnet E2E deposit path is green — real Sepolia deposit → proof → `finalizeDeposit` → ECC mint (depositId=5, full 256-bit recipient).

## Regenerate VK / proof set

From `deposit-prover/`:

```bash
cargo run --release --example export_vk_blob -- \
  --input /tmp/deposit_e2e/deposit_proof_input.json \
  --output /tmp/deposit_e2e/deposit_vk_blob.bin \
  --config-out /tmp/deposit_e2e/deposit_eth_circuit_params.json \
  --degree 18 --max-data-byte-len 256 --max-log-num 20
```

Regenerate the full regression set:

```bash
cargo run --release --example export_deposit_proof_set -- \
  --set-dir fixtures/deposit_10proofs --count 10 \
  --degree 18 --max-data-byte-len 256 --max-log-num 20
```

SRS: use `deposit-prover/params/kzg_bn254_18.srs` (Acki Nacki **chain ceremony**, not Hermez). The opcode embeds the chain `s_g2`; Hermez proofs are rejected.

## Recompile USDCBridge (AN)

TVM-Solidity-Compiler must support `gosh.zkhalo2VerifyWithVK` (branch `halo2_verify`). From `acki-nacki/contracts/`:

```bash
sold --tvm-version gosh --base-path . exchange/USDCBridge.sol -o exchange/
```

After VkBlob change, paste new bytes into `USDCBridge.sol`'s `VK_BLOB` constant and redeploy both `USDCBridge` and embedded `DepositVoucher` code.

## Build gotchas

- `deposit-prover` is a **separate Cargo workspace** (axiom halo2-lib v0.4.1, stable Rust).
- tvm-sdk RLC path needs **nightly** Rust (`gosh` feature) and unified halo2 `[patch]` entries (see `acki-nacki/Cargo.toml` pins).

## Related docs

- [verifying_eth_proof_on_an.md](verifying_eth_proof_on_an.md) — verification stages
- [shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md](shellnet/shellnet_usdcbridge_deposit_vk_redeploy.md) — partner redeploy checklist
- [../deposit-prover/README.md](../../deposit-prover/README.md) — circuit details
