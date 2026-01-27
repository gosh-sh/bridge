# Next Steps for Deposit Prover Implementation

## Current Status

✅ **Phase 1-2 Complete (70% of project)**
- Project structure and CLI
- Ethereum client with event fetching
- MPT proof generation (fully working!)
- RLP encoding (fully working!)
- Circuit design and architecture

🚧 **Phase 3: Circuit Implementation (Next)**
- Integrate axiom-eth components
- Implement ZK circuit logic
- Generate proving/verifying keys

## Immediate Next Steps

### Step 1: Study axiom-eth Examples (1-2 days)

Before implementing the circuit, study these axiom-eth examples:

1. **Receipt proof example**
   ```bash
   git clone https://github.com/axiom-crypto/axiom-eth
   cd axiom-eth/axiom-eth/src/receipt
   # Study receipt.rs for receipt trie verification
   ```

2. **MPT verification example**
   ```bash
   cd axiom-eth/axiom-eth/src/mpt
   # Study mpt.rs for MPT proof verification
   ```

3. **RLP decoding example**
   ```bash
   cd axiom-eth/axiom-eth/src/rlp
   # Study rlp.rs for RLP decoding in-circuit
   ```

4. **Keccak example**
   ```bash
   cd axiom-eth/axiom-eth/src/keccak
   # Study how to use KeccakChip
   ```

### Step 2: Implement Circuit Structure (2-3 days)

Create the basic circuit using `RlcCircuitBuilder`:

```rust
use axiom_eth::rlc::circuit::builder::RlcCircuitBuilder;
use axiom_eth::rlc::circuit::RlcCircuitParams;
use axiom_eth::utils::component::circuit::CoreBuilderParams;

pub struct DepositEventCircuit {
    input: Option<DepositProofInput>,
    params: RlcCircuitParams,
}

impl DepositEventCircuit {
    pub fn new(input: DepositProofInput, params: RlcCircuitParams) -> Self {
        Self {
            input: Some(input),
            params,
        }
    }
    
    pub fn without_witnesses(params: RlcCircuitParams) -> Self {
        Self {
            input: None,
            params,
        }
    }
}
```

### Step 3: Implement Phase 0 (MPT + RLP) (3-4 days)

Implement the first phase of the circuit:

```rust
fn virtual_assign_phase0(
    &self,
    builder: &mut RlcCircuitBuilder<Fr>,
    range: &RangeChip<Fr>,
) -> Result<Phase0Output> {
    let ctx = builder.base.main(0);
    
    // 1. Load receipt proof data
    let receipt_rlp = self.load_receipt_rlp(ctx);
    let proof_nodes = self.load_proof_nodes(ctx);
    let receipt_root = self.load_receipt_root(ctx);
    let tx_index = self.load_tx_index(ctx);
    
    // 2. Verify MPT inclusion using MPTChip
    let mpt_chip = MPTChip::new(rlp_chip, keccak_chip);
    let verified_receipt = mpt_chip.parse_mpt_inclusion_proof(
        ctx,
        proof_nodes,
        receipt_root,
        tx_index,
    )?;
    
    // 3. Decode receipt using RlpChip
    let rlp_chip = RlpChip::new(range, MAX_RECEIPT_LEN);
    let receipt_fields = rlp_chip.decompose_rlp_array_phase0(
        ctx,
        verified_receipt,
        &[STATUS_LEN, GAS_LEN, BLOOM_LEN, LOGS_LEN],
        false,
    )?;
    
    // 4. Extract logs array
    let logs = receipt_fields[3]; // logs is 4th field
    
    // 5. Find our Deposit event log
    let deposit_log = self.find_deposit_log(ctx, logs)?;
    
    // 6. Extract log fields: [address, topics, data]
    let log_fields = rlp_chip.decompose_rlp_array_phase0(
        ctx,
        deposit_log,
        &[ADDRESS_LEN, TOPICS_LEN, DATA_LEN],
        false,
    )?;
    
    let contract_address = log_fields[0];
    let topics = log_fields[1];
    let data = log_fields[2];
    
    // 7. Extract event data from topics and data
    let deposit_hash = self.extract_topic(ctx, topics, 1)?; // topics[1]
    let sender = self.extract_topic(ctx, topics, 2)?;       // topics[2]
    let amount = self.extract_data_field(ctx, data, 0)?;    // data[0:32]
    let timestamp = self.extract_data_field(ctx, data, 1)?; // data[32:64]
    
    Ok(Phase0Output {
        contract_address,
        topics,
        deposit_hash,
        sender,
        amount,
        timestamp,
    })
}
```

### Step 4: Implement Phase 1 (Keccak + Poseidon) (2-3 days)

Implement the second phase with challenge value:

```rust
fn virtual_assign_phase1(
    &self,
    builder: &mut RlcCircuitBuilder<Fr>,
    range: &RangeChip<Fr>,
    phase0_output: Phase0Output,
    challenge: Value<Fr>,
) -> Result<DepositProofOutput> {
    let ctx = builder.base.main(1);
    
    // 1. Verify event signature using KeccakChip
    let keccak_chip = KeccakChip::new(range);
    let event_sig = keccak_chip.keccak_fixed_len(
        ctx,
        b"Deposit(bytes32,address,uint256,uint256)",
    );
    
    // Constrain topics[0] == event_sig
    let topics_0 = self.extract_topic(ctx, phase0_output.topics, 0)?;
    ctx.constrain_equal(&topics_0, &event_sig);
    
    // 2. Verify contract address
    let expected_contract = self.load_contract_address(ctx);
    ctx.constrain_equal(&phase0_output.contract_address, &expected_contract);
    
    // 3. Prove secret knowledge using Poseidon
    let poseidon_chip = PoseidonChip::new(ctx, POSEIDON_SPEC);
    
    let withdrawal_hash = self.load_withdrawal_hash(ctx);
    let nullifier_preimage = self.load_nullifier_preimage(ctx);
    
    let commitment = poseidon_chip.hash_fix_len_array(
        ctx,
        &[withdrawal_hash, nullifier_preimage],
    );
    
    // Constrain commitment == depositHash
    ctx.constrain_equal(&commitment, &phase0_output.deposit_hash);
    
    // 4. Compute nullifier (same as commitment in our design)
    let nullifier = commitment;
    
    // 5. Prepare public outputs
    Ok(DepositProofOutput {
        proof: vec![], // Will be filled by proof generation
        nullifier: nullifier.value().to_bytes(),
        recipient: phase0_output.sender.value().to_bytes(),
        amount: phase0_output.amount.value().as_u64(),
        contract_address: phase0_output.contract_address.value().to_bytes(),
    })
}
```

### Step 5: Implement Proof Generation (1-2 days)

```rust
pub fn prove(
    input: DepositProofInput,
    params: &ParamsKZG<Bn256>,
    pk: &ProvingKey<G1Affine>,
) -> Result<DepositProofOutput> {
    // Create circuit
    let circuit = DepositEventCircuit::new(input, params.clone());
    
    // Generate proof
    let proof = gen_proof(params, pk, circuit)?;
    
    // Serialize proof
    let proof_bytes = serialize_proof(&proof)?;
    
    Ok(DepositProofOutput {
        proof: proof_bytes,
        // ... other fields from circuit public outputs
    })
}
```

### Step 6: Generate Keys (1 day)

```rust
pub fn setup(params: RlcCircuitParams) -> Result<(ProvingKey, VerifyingKey)> {
    // Create circuit without witnesses
    let circuit = DepositEventCircuit::without_witnesses(params);
    
    // Generate KZG params
    let kzg_params = gen_kzg_params(K)?;
    
    // Generate proving and verifying keys
    let (pk, vk) = gen_keys(&kzg_params, &circuit)?;
    
    Ok((pk, vk))
}
```

### Step 7: Test with Real Data (1-2 days)

1. Deploy test contract on Sepolia
2. Make a test deposit
3. Fetch the transaction receipt
4. Generate MPT proof (already working!)
5. Generate ZK proof
6. Verify proof

```bash
# Deploy contract
cd contracts/ethereum
forge script script/Deploy.s.sol --rpc-url sepolia --broadcast

# Make deposit
cast send $BRIDGE_ADDRESS "deposit(bytes32,uint256)" \
  $COMMITMENT 1000000000000000000 --value 1ether

# Generate proof
cd ../../deposit-prover
./target/release/deposit-prover prove \
  --withdrawal-hash $WH \
  --nullifier-preimage $NP \
  --tx-hash $TX_HASH \
  --rpc-url https://sepolia.infura.io/v3/YOUR_KEY \
  --contract-address $BRIDGE_ADDRESS \
  --output proof.json
```

### Step 8: Generate Solidity Verifier (1 day)

```rust
pub fn generate_solidity_verifier(vk: &VerifyingKey) -> Result<String> {
    // Use snark-verifier to generate Solidity code
    let verifier_code = gen_evm_verifier(vk)?;
    Ok(verifier_code)
}
```

## Timeline Estimate

| Task | Duration | Dependencies |
|------|----------|--------------|
| Study axiom-eth examples | 1-2 days | None |
| Implement circuit structure | 2-3 days | Study complete |
| Implement Phase 0 (MPT+RLP) | 3-4 days | Structure done |
| Implement Phase 1 (Keccak+Poseidon) | 2-3 days | Phase 0 done |
| Implement proof generation | 1-2 days | Phase 1 done |
| Generate keys | 1 day | Proof gen done |
| Test with real data | 1-2 days | Keys generated |
| Generate Solidity verifier | 1 day | Testing done |
| **Total** | **12-18 days** | |

## Resources

### Documentation
- [axiom-eth README](https://github.com/axiom-crypto/axiom-eth/blob/main/axiom-eth/README.md)
- [halo2-lib docs](https://github.com/axiom-crypto/halo2-lib)
- [Component Framework](https://github.com/axiom-crypto/axiom-eth/blob/main/axiom-eth/src/utils/README.md)

### Example Code
- [axiom-eth receipt chip](https://github.com/axiom-crypto/axiom-eth/blob/main/axiom-eth/src/receipt/mod.rs)
- [axiom-eth MPT chip](https://github.com/axiom-crypto/axiom-eth/blob/main/axiom-eth/src/mpt/mod.rs)
- [axiom-query examples](https://github.com/axiom-crypto/axiom-eth/tree/main/axiom-query/examples)

### Community
- [Axiom Discord](https://discord.gg/axiom)
- [Telegram](https://t.me/axiom_discuss)

## Current Blockers

None! All infrastructure is in place. Ready to implement the circuit.

## Success Criteria

The circuit implementation is complete when:

1. ✅ Circuit compiles without errors
2. ⏸️ Can generate proofs for real Sepolia deposits
3. ⏸️ Proofs verify correctly
4. ⏸️ Solidity verifier can verify proofs on-chain
5. ⏸️ Gas costs are reasonable (<500k gas for verification)
6. ⏸️ Full deposit → proof → withdrawal flow works

## Notes

- The MPT and RLP work is already done off-chain, which is the hardest part
- The circuit just needs to verify the proofs we already generate
- axiom-eth provides all the chips we need
- The main work is wiring everything together correctly

