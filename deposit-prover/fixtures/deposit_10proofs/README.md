# Deposit proof fixtures

These ten witnesses are synthetic single-leaf transaction and receipt tries for
`Deposit(uint256,address,uint256,bytes32,uint256,address)`. Their headers commit to
those tries; they are not canonical Sepolia blocks. The source chain ID is Sepolia
(`11155111`), deposit IDs run from 0 to 9, and every one of them deposits the same
token, `0x4444…44`.

Regenerate from `deposit-prover/` with the production Hermez SRS in
`data/kzg_params_18.srs`:

```sh
cargo run --release --example generate_deposit_fixtures -- \
  --set-dir fixtures/deposit_10proofs --count 10
cargo run --release --example export_deposit_proof_set -- \
  --set-dir fixtures/deposit_10proofs --count 10
```

The exporter checks every generated proof against one shared verification key.
`public_inputs.bin` contains thirteen little-endian BN254 scalars: `token` is
appended at slot 11, ahead of `promiseCommit`.
The same key is embedded in `contracts/an/exchange/eccUSDCBridge.sol` using
`scripts/embed_deposit_vk_blob.py` from the repository root.

Check that keygen does not depend on the witness it is run from — the key is
the circuit's, not the deposit's:

```sh
work=$(mktemp -d)
cargo run --release --example export_vk_blob -- \
  --input fixtures/deposit_10proofs/proof_09/input.json \
  --output "$work/vk.bin" --config-out "$work/params.json" \
  --degree 18 --max-data-byte-len 2048 --max-log-num 20
cmp "$work/vk.bin" fixtures/deposit_10proofs/deposit_vk_blob.bin && rm -rf "$work"
```

This passed for the current build, and two separate `export_deposit_proof_set`
runs produced the same blob byte for byte, so keygen is reproducible.

**The stronger check is retired for now.** It used to run keygen from
`fixtures/chain_binding_sepolia_dep0/input.json` — a real Sepolia deposit with
five receipt-MPT nodes — to show the key is independent of the deposit's bridge
address and MPT depth as well. That witness carries the five-field
`Deposit` event and has no `token`, so the current tools refuse it
(`missing field token`), and it cannot be regenerated: only a deposit made
through a bridge that emits the six-field event would produce one. Until such a
deposit exists on a test network, witness independence is established only
across the synthetic single-leaf fixtures, which share one MPT shape. Restore
the check with a real six-field witness when one is available.
