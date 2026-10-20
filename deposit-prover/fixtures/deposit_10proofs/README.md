# Deposit proof fixtures

These ten witnesses are synthetic single-leaf transaction and receipt tries for
`Deposit(uint256,address,uint256,bytes32,uint256)`. Their headers commit to those
tries; they are not canonical Sepolia blocks. The source chain ID is Sepolia
(`11155111`), and deposit IDs run from 0 to 9.

Regenerate from `deposit-prover/` with the production Hermez SRS in
`data/kzg_params_18.srs`:

```sh
cargo run --release --example generate_deposit_fixtures -- \
  --set-dir fixtures/deposit_10proofs --count 10
cargo run --release --example export_deposit_proof_set -- \
  --set-dir fixtures/deposit_10proofs --count 10
```

The exporter checks every generated proof against one shared verification key.
`public_inputs.bin` contains twelve little-endian BN254 scalars.
The same key is embedded in `contracts/an/exchange/eccUSDCBridge.sol` using
`scripts/embed_deposit_vk_blob.py` from the repository root.
