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

Check that keygen with the historical canonical Sepolia witness (block 11025192,
five receipt-MPT nodes) produces the same key as these single-leaf fixtures:

```sh
work=$(mktemp -d)
cargo run --release --example export_vk_blob -- \
  --input fixtures/chain_binding_sepolia_dep0/input.json \
  --output "$work/vk.bin" --config-out "$work/params.json" \
  --degree 18 --max-data-byte-len 2048 --max-log-num 20
cmp "$work/vk.bin" fixtures/deposit_10proofs/deposit_vk_blob.bin && rm -rf "$work"
```

This is a keygen-only check of witness independence. The historical receipt
uses the old event ABI and cannot produce a valid proof for the current ABI.
The byte comparison passed for this build; a live deposit through a newly
deployed pair remains a separate end-to-end check.
