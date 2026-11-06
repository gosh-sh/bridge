---
name: evm-an-deposit-e2e
description: >-
  Cross-repo map and runbook for the EVM→Acki Nacki deposit bridge: where the
  receipt-proof builder, relayer, CLI, eccBridge / EthTrie contracts, block-hash
  anchors and test vectors live, plus how to recompile eccBridge. Use when
  working on deposit-receipt, deposit-relayer-daemon, finalizeDeposit / eccBridge,
  EthTrie, the deposit anchors, or the EVM→AN deposit end-to-end flow.
---

# EVM → Acki Nacki deposit e2e

There is no deposit circuit and no deposit proof to generate (bridge-audit#139). The AN-side
`eccBridge` (version 1.17.0) checks the deposit's Ethereum receipt itself, with the VM's
`KECCAK256` instruction and a Merkle-Patricia walk in `contracts/an/exchange/EthTrie.sol`. Off
chain, the only work is collecting the witness from an RPC node.

Gone with that change: `deposit-prover/` (circuit and tools), the deposit VK and
`scripts/embed_deposit_vk_blob.py`, error 220, `scripts/check_deposit_prover_bundle.sh`,
`scripts/sync_deposit_opcode_fixtures_to_tvm_sdk.sh`, the `kzg_params_18.srs` release asset, the
`an-contracts` deposit-VK step and the prover tools in the docker images. `ZKHALO2VERIFYWITHVK`
is still in use — by the Ethereum light client, not by deposits. The VkBlob writer moved to
`crates/bridge-snark-utils/src/halo2_tvm_bundle.rs`.

## `finalizeDeposit`

```
finalizeDeposit(uint256 chainId, bytes header, uint16 txIndex, bytes[] receiptProof, uint16 logIndex)
```

`contracts/an/exchange/eccBridge.sol:949`, the log parsing in `_provenDeposit` (`:1233`). In
order:

1. **Anchor.** `keccak256(header)` must be in `_acceptedBlockHash[chainId]`, else
   `ERR_UNKNOWN_BLOCK` (224). This runs before `tvm.accept()` (with the pause check, `ERR_PAUSED`
   231), so a block the bridge does not know costs it nothing: about 10k gas out of the external
   message's credit.
2. **Accept.** Everything after is paid by the bridge; a wrong proof costs it only its own gas.
3. **Receipt proof.** `EthTrie` checks the header hashes to the block hash, takes its
   `receiptsRoot`, and walks `receiptProof` (root node first, down to the leaf at key
   `rlp(txIndex)`). The receipt's status must be 1 (`308`).
4. **Log.** Log `logIndex` **within that receipt** (not the block-wide index an RPC reports) must
   exist (`307`) and have exactly the `Deposit` shape: the emitter, three topics —
   `topic0 = 0x935bca73ba823197b8e5564da08c2c294de3811fc4a4cb482e6af4fd08623c0e`
   (`Deposit(uint256,address,uint256,bytes32,uint256,address)`), `depositId`, `sender` — and four
   data words: `amount`, `anAccount`, `timestamp`, `token`. Anything else is
   `ERR_NOT_DEPOSIT_LOG` (244); `amount` must fit `uint64` (`ERR_OVERFLOW`).
5. **Emitter.** The emitter must be trusted for the chain, `_trustedL1Bridge[chainId][emitter]`,
   else `ERR_UNSUPPORTED_SRC_CHAIN` (222). A zero amount or recipient is refused too.
6. **Token and limits, then the voucher.** The token must be the one registered for the chain
   (241–243), within the per-transfer cap (234) and the chain's TVL cap (235). Then the bridge
   deploys the `DepositVoucher`, unchanged from before: its code is byte-identical, and its
   address, derived from `(depositId, emitter, dappId = 0, chainId)`, is the replay protection.
   The voucher calls back `confirmTokenDeposit`, which mints.

Every deposit field comes out of the proven log; the caller chooses only which log.

### `EthTrie` errors

| Code | Meaning |
|---|---|
| 300 | the data ends inside an RLP item |
| 301 | an item has the wrong RLP kind or size |
| 302 | a proof node does not hash to its parent's reference |
| 303 | the proof does not follow the key |
| 304 | an inline (under 32 bytes) child node: not supported |
| 305 | the key is not in the trie |
| 306 | the header does not hash to the expected block hash |
| 307 | no log at that index |
| 308 | the transaction of the receipt failed |

### Gas and size

| Block | Gas | `finalizeDeposit` payload |
|---|---|---|
| 40 transactions | 132,569 | 2,140 B |
| 300 transactions | 192,809 | 2,692 B |
| 3,000 transactions | 226,259 | 3,355 B |
| 60 logs before the `Deposit` in its receipt | 377,755 | 11,246 B |

An external message gets a 500,000 `gas_credit` before accept; only step 1 runs inside it. The
Block Manager takes external messages up to 64 KiB; the relayer and the CLI refuse a
`finalizeDeposit` payload over 48 KiB (`MAX_FINALIZE_PAYLOAD_BYTES` in
`crates/deposit-relayer-daemon/src/witness.rs`, `MAX_FINALIZE_PAYLOAD` in
`crates/ackinacki-bridge/src/deposit/limits.rs`).

### Trust model

The receipt proof shows the deposit is in the block whose header was supplied; it cannot show the
block is canonical. That is the anchor's job: `_acceptedBlockHash[chainId]` has three writers —
the owner (`setAcceptedBlockHash`, which `disableOwnerAnchorsOn(chainId)` can switch off), the
Ethereum light client (`acceptBlockHashFromLightClient`) and, for an Arbitrum chain, the
`ArbitrumBlockVerifier` (`acceptBlockHashFromL2Verifier`, only for a block whose parent-chain block
is already accepted). Whoever writes an anchor asserts canonicity; the rest is checked on chain.
`KECCAK256` was already used by `EthBeaconLightClient` and `ArbitrumBlockVerifier`, so no node
change was needed.

## Where each piece lives

| Piece | Location | Notes |
|---|---|---|
| Witness builder | `crates/deposit-receipt/` (root workspace) | `deposit_receipt::fetch` reads `eth_getTransactionReceipt`, `eth_getBlockByHash` and `eth_getBlockReceipts`, checks the header hash, the rebuilt `receiptsRoot` and the `Deposit` log shape the way the contract will, and returns the `finalizeDeposit` arguments. The RPC must serve `eth_getBlockReceipts`. |
| Relayer | `crates/deposit-relayer-daemon/` | Finds the `Deposit` log, builds the witness (`ReceiptWitnessBuilder` in `src/witness.rs`), checks the payload size and submits `finalizeDeposit`. `deposit-relayer --help` lists the subcommands. |
| CLI | `crates/ackinacki-bridge/` (`deposit` subcommand) | Approve, deposit, then the same witness (`deposit_witness` in `src/deposit/evm.rs`) and `finalizeDeposit`. Builds only from `crates/bridge-prover-libraries` with `-p ackinacki-bridge`. |
| Contracts | `contracts/an/exchange/eccBridge.sol`, `contracts/an/exchange/EthTrie.sol`, `contracts/an/exchange/DepositVoucher.sol` | Error codes in `contracts/an/exchange/modifiers/errors.sol` and at the top of `EthTrie.sol`. |
| Compiled artefacts | `contracts/an/0.81.1_compiled/exchange/eccBridge.{tvc,abi.json}`; `contracts/an/0.81.0_compiled/exchange/DepositVoucher.{tvc,abi.json}` | `contracts/an/place.json` places both onto acki-nacki's `contracts/0.81.0_compiled/exchange/`, and the sources (`EthTrie.sol` among them) onto `contracts/exchange/`. Runtime ABI copies are kept in step by `scripts/check_bridge_abi_in_sync.sh`. |
| Anchors | `scripts/deposit_anchor_params.py` | Turns a witness JSON or a `(chain id, block hash)` into `setAcceptedBlockHash` arguments; `--verify` checks against an independent RPC that the block is canonical and buried deep enough. |
| Test vectors | `scripts/deposit-receipt/vectors.py` | `synthetic(...)` builds a self-consistent block and receipt trie with no node; `live --rpc-url <RPC> --tx <hash>` captures a real one into `scripts/deposit-receipt/fixtures/`. |
| Contract tests | `scripts/test_an_bridge_limits.py` | Runs the bridge in a TVM debugger with real receipt proofs from `vectors.py`. Needs `SOLD_0_81_1`, `SOLD_0_81` and a `TVM_DEBUGGER` that runs `KECCAK256`. |
| Chain allowlist | `crates/deposit-chain-ids/` | The source chains the relayer and the CLI accept; on chain the gate is `_trustedL1Bridge` plus the anchors. |

## Recompile eccBridge

Use the compilers pinned by `contracts/an/exchange/Makefile`: sold 0.81.1 for `eccBridge`, the
light client and the Arbitrum block verifier (they call `gosh.keccak256`, which 0.81.1 is the
first release to assemble), sold 0.81.0 for `DepositVoucher`. The Makefile refuses a compiler that
reports another version.

```bash
make -C contracts/an/exchange SOLD_0_81=/path/to/sold-0.81.0 SOLD_0_81_1=/path/to/sold-0.81.1
```

It writes beside the sources and does not touch the tracked artefacts: copy the rebuilt
`eccBridge.tvc` and `eccBridge.abi.json` to `contracts/an/0.81.1_compiled/exchange/` by hand and
commit them together. The voucher must stay byte-identical — its code hash is part of every
voucher address. When the ABI moves, refresh the runtime copies; `check_bridge_abi_in_sync.sh`
tells them apart. The `an-contracts.yaml` pipeline checks the voucher ABI, `place.json` and the
zerostate encoder; `scripts/test_an_bridge_limits.py` is run by hand.

## Docs

[`contracts/an/README.md`](../../../contracts/an/README.md) (the bridge, its anchors and limits),
[`docs/EVM-contracts-spec.md`](../../../docs/EVM-contracts-spec.md) §6 (the `Deposit` event on the
Ethereum side), [`docs/eth-light-client.md`](../../../docs/eth-light-client.md) §3.4 (how anchors
reach the bridge). Anything under `docs/archive/` describes the retired deposit circuit.
