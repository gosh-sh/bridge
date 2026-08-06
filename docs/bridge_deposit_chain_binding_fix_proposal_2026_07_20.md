# Deposit-direction chain-binding fix proposal

**Date:** 2026-07-20
**Audience:** Sergey Gorelyshev (contract), Sergey Egorov (circuit)
**Scope:** ETH → Acki Nacki deposit path

---

## Problem

`USDCBridge.finalizeDeposit(bytes proof, bytes publicInputs)` on Acki Nacki verifies a Halo2 receipt-inclusion proof over an Ethereum `Deposit(...)` event but **never checks which Ethereum L1 emitted it, nor which EVM chain the tx ran on**.

Two attack shapes result:

1. **Wrong-L1 emitter.** Any contract on any EVM chain can emit the same `Deposit(...)` signature. The circuit only proves "some contract at some address emitted this event, and that address was included in the block header via the receipts trie." Nothing binds the emitter to *our* mainnet bridge.
2. **Wrong-chain replay / Sepolia spoofing.** Even if we pin the L1 emitter address, an attacker who deploys a look-alike bridge at the same address on Sepolia (or any testnet) can produce a valid proof-of-inclusion against a Sepolia block. The circuit has no notion of `chain_id`.

Concrete gap:

- `deposit-prover/README.md` (line 47-50) and `deposit-prover/src/circuit_v2.rs` (lines 559-573) both document an intended AN-side check `publicInputs[3] == ETH_BRIDGE_ADDRESS_FR`. It is not wired.
- `USDCBridge.sol` (lines 343-375) reads `f.contractAddr` and `f.dappId` off the public inputs into an anti-replay hash but never `require`s them equal to any expected value.
- No `chain_id` is present in the deposit circuit's public inputs or witness at all.

Public-input layout today (`circuit_v2.rs`, 11 fields):
`[depositId, sender, amount, contractAddress, dappIdHigh, dappIdLow, anAccountHigh, anAccountLow, blockHashHigh, blockHashLow, promiseCommit]`

---

## Track 1 — Contract side (Sergey Gorelyshev)

### 1a. Add two `require`s in `USDCBridge.finalizeDeposit`

In `acki-nacki/contracts/exchange/USDCBridge.sol`, after `_parsePublicInputs` populates `f`:

```solidity
require(f.contractAddr == EXPECTED_L1_BRIDGE_ADDR_FR, "wrong L1 emitter");
require(f.dappId       == EXPECTED_AN_DAPP_ID,        "wrong AN dappId");
```

- `EXPECTED_L1_BRIDGE_ADDR_FR` = mainnet `AckiNackiBridge` L1 address, encoded as an Fr element the way the circuit encodes `contractAddress` (LE Fr repr of 20-byte address, as in `withdrawal::fr_hex_to_u256`).
- `EXPECTED_AN_DAPP_ID` = this deployment's `dapp_id`. Prevents the same proof being cross-relayed into a sibling `USDCBridge` on a different AN dApp and inflating unrelated user balances (anti-replay hash on line 362 currently mixes an *unconstrained* `f.dappId`, so cross-dApp replay isn't naturally deduped).

### 1b. Deploy `AckiNackiBridge` with plain CREATE + retire the deployer key

**Why this is not just a nice-to-have.** 1a pins the L1 emitter *address*. It does not pin the *chain*. If a look-alike bridge at the same L1 address is ever mintable on Sepolia, 1a is bypassable.

- **EOA** = externally-owned account, controlled by a private key (as opposed to a contract account).
- **CREATE** places a new contract at `keccak256(rlp([deployer_eoa, nonce]))[12:]`. To reproduce that address on another chain, an attacker needs the deployer EOA's private key at the same nonce.
- **CREATE2** places a new contract at `keccak256(0xff ‖ deployer ‖ salt ‖ keccak256(bytecode))[12:]`. The deployer here is often a *universal factory* (e.g. Arachnid's Deterministic Deployment Proxy at `0x4e59b44847b379578588920cA78FbF26c0B4956C`, present at the same address on every major chain including Sepolia). With CREATE2 through a universal factory, anyone with the same bytecode + salt can deploy at the same address on Sepolia. 1a collapses.

**Rule for the mainnet deploy:**
1. Fund a brand-new EOA. Use it *only* to deploy `AckiNackiBridge` via plain CREATE.
2. After deploy, destroy the key material. Never re-use it, never re-deploy through a factory.
3. Document the deployer address + nonce in an ops runbook so future ops can audit that the invariant is preserved.

This makes reproducing the L1 address on Sepolia require compromising a destroyed key — cryptographically infeasible.

---

## Track 2 — Circuit side (Sergey Egorov)

### Bind Ethereum mainnet `chain_id = 1` cryptographically inside the ZK proof.

Track 1 is *operational* security: it depends on the deploy-time discipline of 1b holding forever. Track 2 makes the chain-binding a property of the proof itself, so the bridge stays safe even if that discipline is ever broken (e.g. a future ops team redeploys via CREATE2 for "convenience").

Mechanism: for each deposit event, additionally MPT-include the enclosing transaction into the block's `transactionsRoot`, RLP-decode it as an EIP-1559 typed tx, and constrain the `chain_id` field equal to a hardcoded circuit constant. Ethereum tx signatures cover `chain_id` (EIP-155 / EIP-1559), so a Sepolia tx cannot be forged to decode as `chain_id = 1`.

### Files & changes

**`deposit-prover/src/types.rs`**
- Add `TransactionProof { key: Vec<u8>, proof: Vec<Vec<u8>>, tx_bytes: Vec<u8> }` alongside `ReceiptProof`.
- Add `pub tx_proof: TransactionProof` to `DepositProofInput`.

**`deposit-prover/src/ethereum_fetcher.rs`**
- Add `fetch_tx_proof(block_number, tx_index) -> TransactionProof` using `eth_getTransactionByBlockNumberAndIndex` + `eth_getProof`-style MPT walk of the transactions trie (mirrors the existing receipt-proof fetch — the tx index is already available on the receipt).

**`deposit-prover/src/circuit_v2.rs`**

Add constants near the existing `MAX_BLOCK_HEADER_BYTES`:

```rust
pub const EXPECTED_L1_CHAIN_ID: u64  = 1;      // Ethereum mainnet
pub const TX_PF_MAX_DEPTH:      usize = 10;    // transactions-trie depth cap
pub const MAX_TX_WIRE_BYTES:    usize = 512;   // padded typed-tx byte length
```

Extend `block_header_max_field_lens` (already sized for it — index 4 = `transactionsRoot`, 32 bytes; see current code).

In `virtual_assign_phase0`, after the existing receipt-inclusion block, add a chain-binding block:

- **A.** Load `tx_bytes` as fixed-length `MAX_TX_WIRE_BYTES` witness with a real-length register.
- **B.** MPT-include `tx_bytes` under `key = rlp(tx_index)` against `transactionsRoot` extracted from the same block header (reuse the header parse — `transactionsRoot` is `block_hdr_fields[4]`).
- **C.** Constrain `tx_bytes[0] == 0x02` (EIP-1559 type byte). Reject legacy / 2930 / 4844 / 7702 for now; extend later if needed.
- **D.** RLP-decode `tx_bytes[1..real_len]` with an `eip1559_max_field_lens` schema. Field 0 is `chain_id`.
- **E.** `constrain_equal(chain_id_field, Fr::from(EXPECTED_L1_CHAIN_ID))`.
- **F.** `constrain_equal(tx_from_field, sender_phase0)` where `sender_phase0` is the existing witness bound to the receipt's `Deposit(...)` `sender` topic. This closes the loop: the *same* tx we're chain-binding is the *same* tx that produced our deposit receipt.
- **G.** `constrain_equal(tx_to_field, contract_address_phase0)` — same principle for the emitter.

No change to the public-input layout (chain_id is hardcoded in the VK, not a public input — the invariant is baked in, not selectable at verify time).

### Why this removes the Track-1b dependency

- 1a's `require(contractAddr == EXPECTED_L1_BRIDGE_ADDR_FR)` alone is safe **iff** the L1 address is unreachable on any other chain — which is exactly what 1b buys us operationally.
- Track 2's `constrain_equal(chain_id_field, 1)` inside the circuit means: even if an attacker deploys a look-alike bridge at the same L1 address on Sepolia (i.e. even if 1b ever breaks), their Sepolia txs are signed with `chain_id = 11155111`. The RLP-decoded field cannot equal `1`, so the proof will not verify.
- After Track 2 lands, 1b becomes defence-in-depth hygiene, not load-bearing security.

### Deployment note

The chain_id constant is baked into the verifying key. Deploying `USDCBridge` on a different chain (or against a different L1) requires generating a new VK with the new constant and swapping `VK_BLOB` in `USDCBridge.sol`. This is intentional — it means the on-chain VK itself is a witness to which (L1 chain, AN dApp) pair the bridge serves.
