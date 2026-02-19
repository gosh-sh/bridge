# Halo2 Proof Encoding Analysis

## Key Discovery

After studying `snark-verifier/src/verifier/plonk/proof.rs`, I discovered that **the proof is NOT a fixed byte structure**. Instead, it's read sequentially from a **Fiat-Shamir transcript**.

## Proof Reading Sequence

From `PlonkProof::read()`:

```rust
pub fn read<T>(
    svk: &VerifyingKey,
    protocol: &PlonkProtocol,
    instances: &[Vec<Scalar>],
    transcript: &mut T,
) -> Result<Self, Error>
where
    T: TranscriptRead,
{
    // 1. Initialize transcript (optional)
    if let Some(initial_state) = &protocol.transcript_initial_state {
        transcript.common_scalar(initial_state)?;
    }
    
    // 2. Absorb public inputs
    for instances in instances.iter() {
        for instance in instances.iter() {
            transcript.common_scalar(instance)?;
        }
    }
    
    // 3. Read witnesses and squeeze challenges (interleaved)
    for (&num_witness, &num_challenge) in protocol.num_witness.iter().zip(protocol.num_challenge.iter()) {
        let witnesses = transcript.read_n_ec_points(num_witness)?;  // Read from proof
        let challenges = transcript.squeeze_n_challenges(num_challenge);  // Derive via Fiat-Shamir
    }
    
    // 4. Read quotient commitments
    let quotients = transcript.read_n_ec_points(protocol.quotient.num_chunk())?;
    
    // 5. Squeeze evaluation point
    let z = transcript.squeeze_challenge();
    
    // 6. Read evaluations
    let evaluations = transcript.read_n_scalars(protocol.evaluations.len())?;
    
    // 7. Read PCS proof (SHPLONK)
    let pcs = AS::read_proof(svk, &queries, transcript)?;
    
    return PlonkProof { witnesses, challenges, quotients, z, evaluations, pcs };
}
```

## SHPLONK Proof Reading

From `bdfg21.rs`:

```rust
impl Bdfg21Proof {
    fn read<T: TranscriptRead>(transcript: &mut T) -> Result<Self, Error> {
        let mu = transcript.squeeze_challenge();      // NOT in proof bytes!
        let gamma = transcript.squeeze_challenge();   // NOT in proof bytes!
        let w = transcript.read_ec_point()?;          // IN proof bytes (64 bytes)
        let z_prime = transcript.squeeze_challenge(); // NOT in proof bytes!
        let w_prime = transcript.read_ec_point()?;    // IN proof bytes (64 bytes)
        Ok(Bdfg21Proof { mu, gamma, w, z_prime, w_prime })
    }
}
```

## Proof Byte Structure

Based on the reading sequence and our 8224-byte proof:

```
Offset | Size | Content | Source
-------|------|---------|--------
0      | ?    | Witness commitments (phase 0) | read_ec_point × num_witness[0]
...    | ?    | Witness commitments (phase 1) | read_ec_point × num_witness[1]
...    | ?    | Witness commitments (phase 2) | read_ec_point × num_witness[2]
...    | ?    | Witness commitments (phase 3) | read_ec_point × num_witness[3]
...    | ?    | Quotient commitments | read_ec_point × quotient.num_chunk()
...    | ?    | Evaluations | read_scalar × evaluations.len()
...    | 64   | W (SHPLONK opening proof) | read_ec_point
...    | 64   | W' (SHPLONK opening proof) | read_ec_point
```

**Key Insight:** The exact offsets depend on:
- `protocol.num_witness` = [12, 14, 6, 18] (from our analysis)
- `protocol.quotient.num_chunk()` = typically 3
- `protocol.evaluations.len()` = varies

## Calculating Exact Structure

From our proof analysis:
- Witness columns: [12, 14, 6, 18] = 50 total
- Quotient chunks: 3 (typical for degree 18)
- Evaluations: (8224 - 50×64 - 3×64 - 2×64) / 32 = (8224 - 3520) / 32 = 147 field elements

**Proof structure:**
```
Bytes 0-767:     12 witness commitments (phase 0) = 12 × 64 = 768 bytes
Bytes 768-1663:  14 witness commitments (phase 1) = 14 × 64 = 896 bytes
Bytes 1664-2047: 6 witness commitments (phase 2) = 6 × 64 = 384 bytes
Bytes 2048-3199: 18 witness commitments (phase 3) = 18 × 64 = 1152 bytes
Bytes 3200-3391: 3 quotient commitments = 3 × 64 = 192 bytes
Bytes 3392-8095: 147 evaluations = 147 × 32 = 4704 bytes
Bytes 8096-8159: W (opening proof) = 64 bytes
Bytes 8160-8223: W' (opening proof) = 64 bytes
Total: 8224 bytes ✓
```

## Transcript Operations

The transcript alternates between:
1. **Absorb** (common_scalar, common_ec_point) - Update internal state
2. **Read** (read_scalar, read_ec_point) - Read from proof bytes
3. **Squeeze** (squeeze_challenge) - Derive challenge via hash

**Critical:** The order matters! Even a single operation out of order will produce different challenges.

## Implementation Strategy

### Option 1: Implement Full Transcript in Go

**Pros:**
- Exact match with snark-verifier
- Can derive all challenges correctly
- Most flexible

**Cons:**
- Complex implementation
- Need to match exact hash function and encoding

### Option 2: Pre-compute Challenges in Rust

**Pros:**
- Simpler Go implementation
- Guaranteed correct challenges
- Easier to debug

**Cons:**
- Need to pass challenges in JSON
- Larger data transfer

### Recommended: Hybrid Approach

1. **In Rust:** Parse proof, compute challenges, export to JSON
2. **In Go:** Load pre-computed data, verify equations

**Why:** This decouples the complex transcript logic from the circuit implementation, making it easier to debug and verify correctness.

## Updated Rust Parser

We need to enhance `proof_parser.rs` to:

```rust
#[derive(Serialize, Deserialize)]
pub struct Halo2ProofData {
    // Public inputs (already have)
    pub public_inputs: Vec<String>,
    
    // Commitments
    pub witness_commitments: Vec<Vec<String>>,  // Grouped by phase
    pub quotient_commitments: Vec<String>,
    
    // Evaluations
    pub evaluations: Vec<String>,
    
    // SHPLONK proof
    pub w: String,
    pub w_prime: String,
    
    // Challenges (NEW!)
    pub challenges: Challenges,
    pub z: String,  // Evaluation point
    
    // Protocol data (already have)
    pub protocol: ProtocolData,
}

#[derive(Serialize, Deserialize)]
pub struct Challenges {
    pub beta: String,
    pub gamma: String,
    pub alpha: String,
    pub mu: String,
    pub gamma_kzg: String,
    pub z_prime: String,
}
```

## Next Steps

1. ✅ Understand proof encoding (DONE)
2. ⏳ Enhance Rust parser to extract all components
3. ⏳ Export challenges to JSON
4. ⏳ Implement Go structs to load data
5. ⏳ Implement verification equations in gnark

## References

- `snark-verifier/src/verifier/plonk/proof.rs` - Proof reading logic
- `snark-verifier/src/pcs/kzg/multiopen/bdfg21.rs` - SHPLONK proof reading
- `snark-verifier/src/util/transcript.rs` - Transcript implementation

---

**Key Takeaway:** We don't need to implement the full transcript in Go! We can compute everything in Rust and just verify the equations in gnark.

