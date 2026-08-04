# Verifying an Ethereum Proof on the Acki Nacki Side

> **v2 status (rewritten 2026-05-17 after Phase 4.3).** This doc was previously a 5-stage operational guide that included an on-chain ETH-side `Groth16DepositVerifier` adapter, a gnark Groth16 wrapper around the Halo2 deposit proof, and an "AN-side TVM Groth16 verifier" roadmap. Phase 4.3 (Decision Log 2026-05-17 in `docs/an_partner_integration_plan.md`) retired both the ETH-side adapter and the gnark wrapper, on the grounds that the AN side has no EIP-170 ceiling and can verify Halo2 SHPLONK natively through a new TVM opcode (`VERHALO2SHPLONK`, work-in-progress in `tvm-sdk`). This rewrite reflects the post-demolition flow.
>
> The pre-demolition doc is preserved in git history (`git log -- docs/verifying_eth_proof_on_an.md`) for anyone reproducing legacy proofs against `Groth16DepositVerifier.sol`.

This document is the operational guide for **verifying that an Ethereum-side proof of a `Deposit` event is correct** before the Acki Nacki side credits the corresponding tokens to the user.

It is the mirror image of `docs/verifying_an_proof.md` (which covers AN → ETH state proofs). The deposit flow is the **other** direction:

```
[ Ethereum ] AckiNackiBridge.deposit() → Deposit event (off-chain proof input)
                  │
                  ▼
       deposit-prover (Halo2 SHPLONK, K=20, Keccak transcript)
                  │
                  ▼
[ Acki Nacki ]  TokenBridge contract calls VERHALO2SHPLONK (TVM opcode, WIP)
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

The deposit-side native verifier is **not yet deployed on Acki Nacki** as of 2026-05-17.

| Component | Status |
| --------- | ------ |
| `deposit-prover/` — Halo2 SHPLONK circuit (Keccak transcript) | ✅ Live; round-trips against Sepolia historical data. |
| Halo2 verification kit (Rust crate) | ✅ Live in `deposit-prover/src/prover.rs::verify_proof`. |
| `ZKHALO2VERIFYWITHVK` TVM opcode (`tvm-sdk`) | ✅ Landed 2026-05-22 at dispatch byte `0xC7 0x4A` via PR #240; takes a single `Halo2TvmBundle` cell (magic `HALO2TVM`, Blake2b SHPLONK). |
| `gosh.zkHalo2VerifyWithVK(bundle)` compiler support (`TVM-Solidity-Compiler`) | 🚧 In development, depends on the opcode. |
| AN-side `TokenBridge.finalizeDeposit(...)` wiring | 🚧 Pending — will call the new opcode with the on-chain VK. |
| AN-side nullifier mapping on `depositId` | 🚧 Pending. |

Until the opcode lands, deposit verification on the AN side runs **off-chain** in the producer pipeline + BK consensus.

This doc covers the verification *as it will work once the opcode is live* (sections 1–5). What you can already exercise today is the off-chain Halo2 round-trip (sections 1, 2 V1, 2 V2, 2 V3).

---

## 1. What Each Proof Asserts

A single deposit proof commits to **12 BN254 Fr public inputs**. The layout is
generated from one place in code — `circuit_v2::DEPOSIT_PUBLIC_INPUT_LAYOUT`,
pinned by `deposit_circuit_declares_twelve_public_inputs` and
`num_instance_matches_the_declared_layout` — because it has moved three times
(7 → 10 → 11 → 12) and `USDCBridge._parsePublicInputs` reads it by fixed offset:

| # | Field | Type | Source | Bound by |
|---|---|---|---|---|
| 0 | `depositId` | uint256, `< 2^248` | `Deposit` topic 1 | Phase 1 == log topic |
| 1 | `sender` | uint256 (uint160 cast) | `Deposit` topic 2 | Phase 1 == log topic |
| 2 | `amount` | uint256, `< 2^128` | `Deposit` data word 0 | Phase 1 == log data |
| 3 | `contractAddress` | uint256 (uint160 cast) | log emitter address | Phase 1 == log address |
| 4 | `chainId` | uint256 | EIP-1559 typed-tx RLP field 0 | tx MPT @ same `tx_idx` |
| 5 | `dappIdHigh` | uint128 | **config**, not the event | range-checked bytes only |
| 6 | `dappIdLow` | uint128 | **config**, not the event | range-checked bytes only |
| 7 | `anAccountHigh` | uint128 | `Deposit` data word 2, high half | Phase 1 == log data |
| 8 | `anAccountLow` | uint128 | `Deposit` data word 2, low half | Phase 1 == log data |
| 9 | `blockHashHigh` | uint128 | `keccak(header)` high half | `keccak_var_len` |
| 10 | `blockHashLow` | uint128 | `keccak(header)` low half | `keccak_var_len` |
| 11 | `promiseCommit` | uint256 | keccak coprocessor commitment | appended by `EthCircuitImpl` |

`chainId` replaced a VK-baked constant on 2026-07-23 (Track 2): one VK now serves
every allowlisted chain, and the AN side binds `(chainId → expected bridge Fr)`.
`dappId` took the slot `anWorkchain` used to occupy on 2026-06-02; it is a
config tag the AN-side `TokenBridge` compares against its own configured value,
**not** anything read off Ethereum.

**Payload format**: Halo2 SHPLONK proof bytes + 12 public-input Fr elements (LE).
The native verifier consumes proof + inputs + VK directly; no Groth16 layer. The
AN side reconstructs the recipient as `0:(anAccountHigh<<128 | anAccountLow)`.

A valid proof certifies, given the witness, that:

1. `blockHashHigh ‖ blockHashLow` is `keccak256` of a byte string that RLP-decodes
   as a 16–21 field block header, and the keccak'd length equals the length the
   RLP list prefix declares (BC-D03).
2. A `Receipt` exists at transaction index *i* under that header's `receiptsRoot`,
   verified against the root the MPT chip itself derived — not a separately
   supplied witness (Alina review finding #1).
3. An EIP-1559 transaction exists at the **same** index *i* under the same
   header's `transactionsRoot`, and `chainId` is its RLP field 0. Sharing the
   `tx_idx` cell is what makes the receipt and the transaction the same
   transaction without an in-circuit `ecrecover`.
4. Inside the receipt's logs, the log at `log_index` has:
   - `address == contractAddress`;
   - exactly 99 bytes of topics and 128 bytes of data (BC-D04), so the fixed
     offsets below read event bytes rather than zero padding;
   - `topics[0] == keccak256("Deposit(uint256,address,uint256,int8,bytes32,uint256)")`
     = `0x8d5d0606…3d37ee`;
   - `topics[1] == bytes32(depositId)`, `topics[2] == bytes32(uint256(uint160(sender)))`;
   - `data` decodes to `(amount, anWorkchain, anAccount, timestamp)`, of which
     `amount` and `anAccount` are bound to public inputs and `anWorkchain` /
     `timestamp` are deliberately not.
5. All keccak256 calls are bound to `promiseCommit` via the Poseidon coprocessor
   pattern (see `docs/keccak_coprocessor_flowchart.mmd`).

**It does not certify**:

- That `blockHashHigh ‖ blockHashLow` is the *canonical* Ethereum block hash —
  the prover supplies the header and every trie node itself, so a
  self-consistent but entirely fabricated chain satisfies items 1–4 above
  (**BC-D01**). Canonicality is a fact about Ethereum consensus and is asserted
  from outside the proof: since `acki-nacki` `7992ce26`, `finalizeDeposit`
  requires the reassembled hash to sit in `_acceptedBlockHash[chainId]`, an
  anchor set the owner populates via `setAcceptedBlockHash` — mirroring what
  `AckiNackiBridge._knownAnchors` does for the opposite direction. So the owner
  key, not the proof, is what certifies canonicality today; M-of-N attesters are
  the planned replacement. See `docs/reviews/deposit_circuit_audit_2026-08-03.md`
  §1, and use `scripts/deposit_anchor_params.py --verify` to derive an anchor and
  check it against an independent node before admitting it.
- That the *enclosing transaction* called the bridge directly. The `tx.to ==
  contractAddress` constraint was removed (BC-D02) so that Safe / multisig,
  ERC-4337, EIP-7702 and router-mediated deposits remain provable; the emitter
  is pinned by item 4 regardless.
- That `dappIdHigh`/`dappIdLow` mean anything on their own — they are
  range-checked to be genuine 128-bit halves and nothing more.
- That `depositId` has not already been credited. That's the AN-side nullifier
  (V5 below).

> **`verify_block_header_rlp` is a native check, not a constraint.**
> `rlp_utils::verify_block_header_rlp` asserts `keccak(encoded_header) ==
> block.hash` while *building* the witness, which is what stops us from silently
> shipping a header with a consensus field dropped (PR #20 finding R2). It runs
> in the prover process, so it constrains honest witness generation only — it is
> not part of what the proof asserts, and a malicious prover simply skips it.

---

## 2. Verification Stages

A complete acceptance flow has 5 stages (V1–V5). Stages V1–V3 can be exercised **today** with off-chain tooling; V4 is the on-chain native verification that lands with the opcode; V5 is the AN-side replay prevention.

### V1 — Ground-truth Ethereum cross-check (off-chain)

**Before** running the cryptographic verifier, independently confirm against an Ethereum RPC quorum (≥ 2 distinct providers) that:

- `eth_getBlockByNumber(blockNumber)` returns a block whose `hash == blockHashHigh ‖ blockHashLow`.
- `eth_getTransactionReceipt` for some transaction in that block contains a `Log` with the `Deposit` topic and matching `depositId`, `sender`, `amount`.
- `contractAddress` matches the deployed `AckiNackiBridge` for the target chain.

If any RPC disagrees, abort. This guards against the cryptographic verifier accepting a proof tied to a phantom block hash.

Since `acki-nacki` `7992ce26` this stage has an on-chain counterpart: the outcome of V1 is what the owner records with `setAcceptedBlockHash`, and `finalizeDeposit` will not credit a deposit whose block hash is absent from that set. `scripts/deposit_anchor_params.py --verify` performs the hash half of the check above (canonical at its number, ≥ `--min-confirmations` deep) and prints the setter arguments only if it passes.

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

### V4 — AN-side native verification via `VERHALO2SHPLONK` [planned]

Once the opcode lands, AN-side `TokenBridge.finalizeDeposit(...)` will compile to:

```solidity
function finalizeDeposit(
    bytes calldata halo2Proof,
    uint256[10] calldata publicInputs,  // see the 10-input table above
    bytes calldata vk
) external {
    // Compile-time: gosh.verHalo2Shplonk(...) → VERHALO2SHPLONK TVM opcode
    require(gosh.verHalo2Shplonk(halo2Proof, publicInputs, vk), "invalid deposit proof");
    require(publicInputs[3] == ETH_BRIDGE_ADDRESS_FR, "wrong bridge contract");
    require(!nullifier[publicInputs[0]], "already credited");
    nullifier[publicInputs[0]] = true;

    // Credit the PROVEN Acki Nacki recipient (not an EVM address).
    int8 anWorkchain = int8(int256(publicInputs[4]));
    uint256 anAccount = (publicInputs[5] << 128) | publicInputs[6];
    uint256 amount = publicInputs[2];
    _mintTo(anWorkchain, anAccount, amount);
}
```

The VK passed in is bound to the canonical deposit-prover circuit on the AN side (set once at deployment time, immutable thereafter).

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
| ETH-AN-1 | Producer feeds a proof tied to a non-canonical Ethereum block hash (re-org, alt-chain) | On-chain: `finalizeDeposit` requires the hash in `_acceptedBlockHash[chainId]` (BC-D01). Off-chain: V1 RPC quorum before admitting the anchor, ≥ 64 confirmations. Residual: the owner key can admit a hash from a chain that does not exist — M-of-N attesters are the planned replacement. |
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
- `docs/an_partner_integration_plan.md` — Decision Log 2026-05-17 explains the Phase 4.3 demolition and the pivot to native AN-side verification.
- `docs/verifying_an_proof.md` — Mirror flow for AN → ETH state attestation.
