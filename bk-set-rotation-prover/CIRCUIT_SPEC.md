# BK Set Rotation Circuit — Specification

## 1. Purpose

Prove that the current Block Keeper (BK) committee of Acki Nacki attested to a
block containing a BK set transition, thereby authorizing the bridge to update
its stored `currentBkSetCommitment` without owner intervention.

## 2. Public Inputs (2 BN254 Fr elements)

| Index | Field                    | Description                              |
|-------|--------------------------|------------------------------------------|
| 0     | `old_bk_set_commitment`  | Poseidon commitment to the current BK set |
| 1     | `new_bk_set_commitment`  | Poseidon commitment to the new BK set     |

This matches the interface already defined in the partner's test scaffolding at
`gosh-zk-snark-halo2-utils/tests/test_bk_set_circuit.rs`.

## 3. Private Witness

| Field                  | Type                        | Description                                              |
|------------------------|-----------------------------|----------------------------------------------------------|
| `block_envelope`       | `Vec<u8>` (up to 4096 B)   | Bincode-serialized block envelope containing BK changes  |
| `attestation`          | `Vec<u8>` (~120 B)         | BLS-signed `AttestationData` over the block              |
| `old_bk_set`           | `HashMap<u16, Vec<u8>>`    | Current BK set: signer_index → 48-byte compressed G1     |
| `new_bk_set`           | `HashMap<u16, Vec<u8>>`    | New BK set after applying `block_keeper_set_changes`     |
| `bls_signature`        | `G2Affine`                 | Aggregated BLS12-381 signature                           |
| `signer_entries`       | `Vec<(u16, u16)>`          | (signer_index, count) for active signers                 |

## 4. What the Circuit Proves

### A. SHA-256 Block Binding

Same as the layer-hash circuit:
- Compute SHA-256 of `block_envelope` (up to 4096 bytes, padded)
- Constrain the hash output to equal `attestation.envelope_hash`

Reuse: `gosh-sha256-chip` compression + state-to-bytes + block selector pattern.

### B. Target Type Enforcement

Constrain 4 bytes at `TARGET_TYPE_REL_OFFSET` (offset 116) within the
attestation to zero (Primary attestation only).

### C. BLS Signature Verification (Old BK Set)

- Load all old BK set G1 pubkeys (on-curve check, no subgroup check needed for G1)
- Hash-to-curve on attestation bytes → G2 message hash
- Aggregate signature verification with `ThresholdMode::Primary` (≥ 2/3)
- This proves the **current** committee signed the block

Reuse: `gosh-bls-verification::load_bk_set_pubkeys`, `compute_all_pub_sum`,
`verify_bls_attestation_with_assigned_msghash`, `HashToCurveChip`.

### D. Old BK Set Commitment

- Compute Poseidon commitment over sorted old BK set:
  `Poseidon([idx_0, x_limb_0..4, idx_1, x_limb_0..4, ...])`
- Constrain output == public input `[0]` (`old_bk_set_commitment`)

Reuse: `compute_bk_set_commitment` from
`layer-hashes-update-halo2-circuit/circuit/src/lib.rs`.

### E. New BK Set Commitment

- Compute Poseidon commitment over sorted new BK set (same formula as D)
- Constrain output == public input `[1]` (`new_bk_set_commitment`)

### F. Binding New BK Set to the Attested Block (Design Options)

The circuit must ensure the new BK set is actually authorized by the attested
block. Two approaches:

#### Option 1: In-circuit BK set change parsing (rigorous but complex)

Parse `block_keeper_set_changes` from `block_envelope` at known bincode offsets:
- Locate the field within `CommonSection` (requires computing offset past all
  preceding fields — attestations, acks, nacks, refs, etc.)
- Parse `Vec<BlockKeeperSetChange>` entries: enum discriminant + SignerIndex +
  BlockKeeperData (pubkey extraction)
- Apply additions/removals to old set → derive new set in-circuit
- Constrain derived new set == witnessed new set

**Pros**: Fully trustless — the circuit proves the transition is exactly what
the block contains.

**Cons**: Very complex bincode parsing in-circuit; fragile to node serialization
changes; large constraint count for variable-length structures.

#### Option 2: Witness-based with attestation binding (recommended)

Both old and new BK sets are witnessed directly. The circuit proves:
1. Old BK set commitment matches on-chain state (public input 0)
2. Old BK set signed the block (BLS verification)
3. New BK set commitment is computed correctly (public input 1)

The **binding** between the new BK set and the block is **implicit**: the old
committee (≥ 2/3) signed a block that, by Acki Nacki protocol rules, contains
the correct `block_keeper_set_changes`. The bridge trusts that:
- The BK protocol produces honest transitions
- ≥ 2/3 of the old committee would not sign a block with malicious BK changes

This is the **same trust model** as the layer-hash circuit (finding D-1 from
audit: "relies on BLS attestation honesty").

**Pros**: Much simpler circuit; reuses existing components almost entirely;
no bincode parsing needed.

**Cons**: Does not independently verify that the new set corresponds to the
block's `block_keeper_set_changes` field. A dishonest prover with control of
≥ 2/3 of the old committee could submit any new commitment.

#### Recommendation

**Use Option 2** for the initial implementation. The security reduction is:
- Breaking BK rotation ≡ corrupting ≥ 2/3 of the current BK committee
- This is identical to the assumption already made for layer hash updates
- Option 1 can be added later as defense-in-depth if needed

## 5. Circuit Parameters (Estimated)

| Parameter      | Value | Notes                                       |
|----------------|-------|---------------------------------------------|
| K              | 19    | Same as layer-hash circuit (BLS dominates)  |
| LOOKUP_BITS    | 18    | Standard for K=19                           |
| LIMB_BITS      | 104   | BLS12-381 non-native field arithmetic       |
| NUM_LIMBS      | 5     | 5 × 104 = 520 bits for BLS12-381 Fp        |
| MAX_SIGNERS    | 300   | Maximum BK set size                         |
| MAX_BLOCK_DATA | 4096  | Maximum block envelope bytes for SHA-256    |

## 6. Differences from the Layer Hash Circuit

| Aspect                 | Layer Hash Circuit      | BK Rotation Circuit     |
|------------------------|-------------------------|-------------------------|
| Public inputs          | 13 (commitment + layers + hashes + prev) | 2 (old + new commitment) |
| Layer hash extraction  | Yes (BTreeMap parsing)  | No                      |
| Merkle chain verify    | Yes (dense balanced tree) | No                    |
| BK set commitments     | 1 (current set)         | 2 (old + new)           |
| BLS verification       | 1 call (old set)        | 1 call (old set)        |
| SHA-256 + H2C          | Same                    | Same                    |

The rotation circuit is **structurally simpler** — it's the layer-hash circuit
minus the layer extraction and Merkle chain, plus a second Poseidon commitment.

## 7. Fixture Generation

For testing, fixtures can be generated using the partner's `block_data_gen`
crate which already models `BlockKeeperSetChange` and `apply_bk_set_changes`.

Required fixture JSON format:
```json
{
  "block_envelope_hex": "...",
  "attestation_hex": "...",
  "old_bk_set": { "0": "abcdef...", "1": "123456..." },
  "new_bk_set": { "0": "abcdef...", "2": "789abc..." }
}
```

For live data, the `circuit-data-exporter` (on `bridge_halo2_tests` branch of
`acki-nacki`) needs extension to capture blocks containing BK set transitions.

## 8. Integration with Ethereum

### On-chain contracts (already implemented)

- `IBkSetRotationVerifier.sol` — interface
- `BkSetRotationVerifier.sol` — adapter (assembles 2 public inputs, calls Groth16)
- `BkSetRotationGroth16Verifier.sol` — interface for gnark-generated verifier
- `LayerHashBridge.rotateBkSet()` — permissionless, verifies ZK proof

### Proof pipeline

```
Acki Nacki block with BK changes
    → BkSetRotationCircuit (Halo2, K=19)
        → 2 public inputs: [old_commitment, new_commitment]
    → gnark-wrapper (Go): Halo2 → Groth16
        → 256-byte proof
    → Ethereum: BkSetRotationVerifier → Groth16Verifier
    → LayerHashBridge.rotateBkSet() updates currentBkSetCommitment
```

### Gnark wrapper (already scaffolded)

Located at `bk-set-rotation-prover/gnark-wrapper/`. Adapted from the
layer-hash wrapper with `NumPublicInputs = 2`.

## 9. Partner Coordination

The partner has a private repository `bk-set-change-verifier-halo2-circuit-with-better-sha256`
that is intended for exactly this purpose. A stub exists at `../bk-set-stub/`
with `PrimaryBkSetVerifierCircuit::build` returning `unimplemented!()`.

Test scaffolding in `../gosh-zk-snark-halo2-utils/tests/test_bk_set_circuit.rs`
defines the expected API:
- `PrimaryBkSetVerifierCircuit::<Fr>::new(block_envelope_bytes, attestation_bytes, old_bk_set, ...)`
- Public instances: `[old_bk_set_commitment, new_bk_set_commitment]`

**Action items**:
1. Ask if the partner has a partial implementation or timeline
2. If not available, implement using the components listed in Section 4
3. The Ethereum contracts and gnark wrapper are ready regardless
