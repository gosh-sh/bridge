# Verifying an Ethereum Proof on the Acki Nacki Side

> **Updated 2026-07.** Deposit verification runs on Acki Nacki via `ZKHALO2VERIFYWITHVK` (3-operand ABI). See [integration/an_partner_integration_plan.md](../integration/an_partner_integration_plan.md) Decision Log for Phase 4.3 history.

This document is the operational guide for **verifying that an Ethereum-side proof of a `Deposit` event is correct** before the Acki Nacki side credits the corresponding tokens to the user.

It is the mirror image of docs/operations/verifying_an_proof.md (which covers AN → ETH state proofs). The deposit flow is the **other** direction:

```
[ Ethereum ] AckiNackiBridge.deposit() → Deposit event (off-chain proof input)
                  │
                  ▼
       deposit-prover (Halo2 SHPLONK, K=20, Keccak transcript)
                  │
                  ▼
[ Acki Nacki ]  USDCBridge.finalizeDeposit(proof, publicInputs)
                  │
                  ▼
       ZKHALO2VERIFYWITHVK TVM opcode (deployed)
                  │
                  ▼
       proof verified natively → mint / credit user
                  │
                  ▼
       nullifier on `depositId` tracked to prevent replay
```

Compared to the v1 flow, two layers are gone:

- The off-chain Go gnark wrapper that wrapped the Halo2 SHPLONK proof into a 256-byte Groth16 BN254 proof.
- The on-chain `Groth16Verifier.sol` + `Groth16DepositVerifier.sol` adapter that lived on the ETH side (in v1 their role was a refund-style "I deposited, I want my ETH back" path; v2 has no such path — see Phase 4.3 in the integration plan).

The doc serves three audiences:

- **Acki Nacki BK / verifier node**: you receive a proof from a relayer and must decide whether to credit the user.
- **Auditor**: you want to confirm a published proof would have been accepted.
- **Relayer (producer)**: you generate the proof and want to confirm it before submitting.

---

## 0. State of Implementation

| Component | Status |
| --------- | ------ |
| `deposit-prover/` — Halo2 SHPLONK circuit (Keccak transcript, 11 PI) | ✅ Live |
| `ZKHALO2VERIFYWITHVK` TVM opcode (`0xC7 0x4A`) | ✅ Landed 2026-05-22; 3-operand stack ABI (vk, publicInputs, proof) |
| AN-side `USDCBridge.finalizeDeposit(proof, publicInputs)` | ✅ Shellnet E2E green (2026-07-02) |
| `deposit-relayer-daemon` | ✅ Production path: eth_getLogs → prove → submit |
| Production VkBlob | ✅ Witness-independent `20cf9018…647a39` — see [deposit_vk_witness_independence.md](../zk/an-side/deposit_vk_witness_independence.md) |

Wire format reference: [zkhalo2verifywithvk_reference.md](../zk/an-side/zkhalo2verifywithvk_reference.md).

---

## 1. What Each Proof Asserts

A single deposit proof commits to **11 BN254 Fr public inputs**:

| # | Field | Type | Source |
|---|---|---|---|
| 0 | `depositId` | uint256 | Indexed value from `Deposit(depositId, sender, amount, anWorkchain, anAccount, timestamp)` |
| 1 | `sender` | uint256 (uint160 cast) | `msg.sender` of `deposit()` |
| 2 | `amount` | uint256 | USDC amount (6 decimals) |
| 3 | `contractAddress` | uint256 (uint160 cast) | Bridge contract address |
| 4 | `dappIdHigh` | uint128 | Upper 128 bits of AN dApp id (config-supplied) |
| 5 | `dappIdLow` | uint128 | Lower 128 bits of AN dApp id |
| 6 | `anAccountHigh` | uint128 | Upper 128 bits of 256-bit AN recipient |
| 7 | `anAccountLow` | uint128 | Lower 128 bits of 256-bit AN recipient |
| 8 | `blockHashHigh` | uint128 | Upper 128 bits of block hash |
| 9 | `blockHashLow` | uint128 | Lower 128 bits of block hash |
| 10 | `promiseCommit` | uint256 | Keccak coprocessor commitment (Poseidon) |

The AN side reconstructs the recipient as `makeAddrStd(0, (anAccountHigh << 128) | anAccountLow)`. The `anWorkchain` field is in the Ethereum event for indexing but is **not** a separate public input — binding uses `dappId` + `anAccount` halves.

A valid proof certifies, given the witness, that:

1. The block whose keccak256 hash equals `blockHashHigh ‖ blockHashLow` was the producer of the receipt referenced.
2. A `Receipt` exists in that block's receipts trie at some transaction index (MPT inclusion).
3. Inside the receipt's logs, there is a `Log` entry whose:
   - `address == contractAddress` (the bridge);
   - `topics[0] == keccak256("Deposit(uint256,address,uint256,uint256)")`;
   - `topics[1] == bytes32(depositId)`, `topics[2] == bytes32(uint256(uint160(sender)))`;
   - `data` decodes to `(amount, timestamp)`.
4. All keccak256 calls inside the circuit are bound to `promise_commit` via the Poseidon coprocessor pattern (~500× constraint savings; see `docs/keccak_coprocessor_flowchart.mmd`).

**It does not certify**:

- That `blockHashHigh ‖ blockHashLow` is the *canonical* Ethereum block hash. That's the verifier's job (V1 below).
- That `depositId` has not already been credited. That's the AN-side nullifier's job (V5 below).

---

## 2. Verification Stages

A complete acceptance flow has 5 stages (V1–V5). Stages V1–V3 can be exercised **today** with off-chain tooling; V4 is the on-chain native verification that lands with the opcode; V5 is the AN-side replay prevention.

### V1 — Ground-truth Ethereum cross-check (off-chain)

**Before** running the cryptographic verifier, independently confirm against an Ethereum RPC quorum (≥ 2 distinct providers) that:

- `eth_getBlockByNumber(blockNumber)` returns a block whose `hash == blockHashHigh ‖ blockHashLow`.
- `eth_getTransactionReceipt` for some transaction in that block contains a `Log` with the `Deposit` topic and matching `depositId`, `sender`, `amount`.
- `contractAddress` matches the deployed `AckiNackiBridge` for the target chain.

If any RPC disagrees, abort. This guards against the cryptographic verifier accepting a proof tied to a phantom block hash.

### V2 — MPT cross-check (off-chain)

Re-fetch the same receipt + MPT path the relayer used (`deposit-prover/src/ethereum_fetcher.rs`) and confirm:

- The MPT inclusion path reconstructs the block's `receiptsRoot`.
- The log decode produces the same 4 public-input field elements.

This is *not* the ZK verification — it's just an independent re-derivation of what the witness must have been. Useful when triaging a failed Halo2 verification.

### V3 — Halo2 SHPLONK verification (off-chain, native Rust)

Run the deposit-prover's verifier against the proof bytes + public inputs + VK:

```rust
use deposit_prover::prover::{verify_proof, CircuitConfig};

let cfg = CircuitConfig::default();
let ok = verify_proof(&cfg, &vk, &proof_bytes, &public_inputs)?;
assert!(ok, "Halo2 verifier rejected the proof");
```

This is the same verification logic the AN-side opcode will execute. If V3 passes, the proof is cryptographically valid; whether AN credits the user is then a function of V1 (ground truth) and V5 (no double-credit).

### V4 — AN-side native verification via `ZKHALO2VERIFYWITHVK` (deployed)

Production `USDCBridge.finalizeDeposit` on shellnet:

```solidity
function finalizeDeposit(bytes proof, bytes publicInputs) public {
    gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof);
    // Parses 11 × 32 B LE Fr from publicInputs; deploys voucher → confirmDeposit
}
```

Three stack operands (frozen ABI): `vk_cell`, `public_inputs_cell` (11 × 32 B LE Fr), `proof_cell`. The VK is embedded in `USDCBridge` as `VK_BLOB` (immutable per deployment).

Until the opcode is live, this stage is **deferred** — the BK consensus on V1 + V3 acts as a (centralised) substitute.

### V5 — Nullifier check [planned]

Per `depositId`, the AN-side `TokenBridge` keeps a `mapping(uint256 => bool) nullifier`. The first valid V4 transaction sets it; subsequent attempts revert. This makes replay attacks (re-submitting the same proof) impossible at the contract level.

---

## 3. Producer (relayer) checklist

When generating a proof to submit:

1. **Pick a finalised block**. The deposit must be ≥ 12 blocks old to avoid Ethereum re-orgs (`deposit-prover/src/ethereum_fetcher.rs::FINALITY_CONFIRMATIONS`).
2. **Fetch ground truth from ≥ 2 RPCs**. Disagreements halt the producer.
3. **Build the Halo2 witness** with `deposit-prover::generate_proof`. The witness includes the MPT path + receipt RLP + log decode.
4. **Run mock prover first** (`test_circuit_mock`) — fastest way to catch a witness-side bug.
5. **Generate the SHPLONK proof**. Verify it locally with V3 before submitting.
6. **Submit to AN-side** via `TokenBridge.finalizeDeposit(proof, publicInputs, vk)`.

---

## 4. Risk Summary

| ID | Risk | Mitigation |
| --- | --- | ---------- |
| ETH-AN-1 | Producer feeds a proof tied to a non-canonical Ethereum block hash (re-org, alt-chain) | V1 RPC quorum; producer waits 12 finalisations. |
| ETH-AN-2 | Producer omits MPT proof step and forges receipt | V3 catches it — the in-circuit MPT inclusion is a hard binding. |
| ETH-AN-3 | Replay of an already-credited deposit | V5 nullifier check. |
| ETH-AN-4 | Wrong-bridge spoofing (`contractAddress` of an attacker contract) | V4 enforces `publicInputs[3] == ETH_BRIDGE_ADDRESS_FR`. |
| ETH-AN-5 | Halo2 forgery via VK tampering | VK is set immutable at AN-side `TokenBridge` deployment; rotation requires a contract upgrade. |
| ETH-AN-6 | `ZKHALO2VERIFYWITHVK` opcode soundness bug | Mitigation: the opcode is tested in `tvm-sdk` against malformed proofs / mutated inputs / wrong VK (`test_halo2_with_vk.rs` covers positive round-trip, flipped-proof rejection, tweaked-instance rejection, bad-magic `FatalError`, LRU cache reuse); production deployment is gated on a partner-side review of the opcode implementation. |

---

## 5. References

- `deposit-prover/src/circuit_v2.rs` — Halo2 circuit definition.
- `deposit-prover/src/prover.rs` — `generate_proof`, `verify_proof`, `CircuitConfig`.
- `deposit-prover/src/ethereum_fetcher.rs` — RPC + MPT proof construction.
- `docs/keccak_coprocessor_flowchart.mmd` — Keccak coprocessor architecture.
- docs/integration/an_partner_integration_plan.md — Decision Log 2026-05-17 explains the Phase 4.3 demolition and the pivot to native AN-side verification.
- docs/operations/verifying_an_proof.md — Mirror flow for AN → ETH state attestation.
