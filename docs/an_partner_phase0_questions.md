# Acki Nacki Bridge — Pre-Integration Questions for the AN Partner Team

**From**: bridge integration team
**To**: Acki Nacki bridge / circuits team
**Subject**: Phase 0 readiness check before we wire your 4-circuit prover into the Ethereum bridge

Hi team,

Following the new architecture you shipped in `acki-nacki-to-eth-bridge-halo2-circuits` and `acki-nacki-to-eth-bridge-halo2-prover`: thank you, this is great work. Block-size independence, the 8-leaf envelope tree, the four-circuit decomposition with shared `envelope_hash` — exactly the right shape for cheap and composable on-chain verification.

We've drafted a 7-phase plan (`docs/an_partner_integration_plan.md` in our repo) to integrate it end-to-end on the Ethereum side, including a rebuilt `AckiNackiBridge.sol` with native Halo2 SHPLONK Yul verifiers (no gnark step), a relayer service, and the existing AAVE/deposit infrastructure merged in.

Before we start writing code we'd like to lock in eight items. None of them should be controversial — most are factual confirmations or single test vectors — but each gates a downstream phase, so getting them clear up front is a meaningful time saver.

---

## The Eight Questions

### Q1 — Which AN-node branch implements the new 8-leaf envelope hash?

`bridge-prover-daemon/README.md` mentions:

> ```
> git checkout latest_an_to_eth_bridge_test
> make run
> ```

…but in our local mirror of `acki-nacki` we only see `origin/bridge_halo2_tests`; `latest_an_to_eth_bridge_test` does not appear under `git branch -r`.

- Is `latest_an_to_eth_bridge_test` the canonical branch, and if so could you push it / make it visible?
- Or has the work been merged into `bridge_halo2_tests` (or another branch) under a different name?
- Either way: **what commit SHA should we pin for the AN node** for the duration of this milestone?

### Q2 — Has `BlockKeeperSetChangeProofData.transition_hashes` been migrated?

Section 8 of `ENVELOPE_HASH_MERKLE_SPEC.md` says the current node code (`block_producer.rs:1158-1178`) computes:

```rust
PoseidonSponge::hash_bytes_flat(bincode::serialize(&bk_set))
```

…and that this **must** be replaced by the new minimal-Poseidon format `Poseidon(sorted [(signer_index, pubkey_x_limbs)…])` so that the on-chain stored commitment, the circuit's commitment, and the node's `transition_hashes` all agree.

- Has this migration landed in the AN node? If yes, on which branch / SHA?
- If not yet, who owns it — your team or ours? (Our preference is: you own it, since it touches `block_producer.rs` and the validators must independently agree on the new hash. We're happy to provide a Rust reference impl + test vector.)

### Q3 — MockProver status of circuits 1B, 2, and 3

Your README claims `cargo test` passes for all four circuits. Could you:

- Pin a known-good commit SHA on `acki-nacki-to-eth-bridge-halo2-circuits` (the one we should patch our `Cargo.toml` against)?
- Confirm that as of that SHA, `cargo test -p attestation-bls-checker-circuit`, `cargo test -p historical-layer-hashes-movement-checker-circuit`, and `cargo test -p bk-set-update-checker-circuit` all pass clean?
- Are there any feature flags we need (we noticed `small-window` exists on the older `layer-hashes-update-halo2-circuit`)?

### Q4 — Test vector for the BK-set Poseidon commitment

This is the single most important byte-level interop check between your node, your circuits, our relayer, and our Solidity contract — all four must agree on the **exact** 32-byte output for any given BK set.

Could you produce one (or ideally a few) reference vectors with this format:

```json
{
  "description": "deterministic test vector",
  "input": {
    "bk_set": [
      { "signer_index": 0, "compressed_g1_pubkey_hex": "ab12...96 hex chars" },
      { "signer_index": 1, "compressed_g1_pubkey_hex": "cd34..." },
      { "signer_index": 2, "compressed_g1_pubkey_hex": "ef56..." }
    ],
    "max_signers": 300,
    "padding_signer_index": 65535,
    "padding_x_limbs": [0, 0, 0, 0, 0]
  },
  "intermediate": {
    "first_real_entry_fr_inputs": ["0x0", "0x...limb0...", "...", "...limb4..."],
    "first_padding_entry_fr_inputs": ["0xffff", "0x0", "0x0", "0x0", "0x0", "0x0"]
  },
  "output": {
    "poseidon_commitment_hex": "0x...32-byte LE hex...",
    "poseidon_commitment_fr_decimal": "12345...big-decimal..."
  }
}
```

So that we can independently:

1. Reproduce the `intermediate.first_real_entry_fr_inputs` (validates that we decompose the pubkey x-coordinate to CRT limbs identically — `LIMB_BITS=104`, `NUM_LIMBS=5`, little-endian).
2. Reproduce the final `poseidon_commitment_hex` (validates the Poseidon parameters `T=3, RATE=2, R_F=8, R_P=57` match and the input-feeding order is `[index, limb0, limb1, limb2, limb3, limb4]`).

Three vectors would be ideal: one with N=1 (just one real signer + 299 padding), one with N=10, and one with N=300 (no padding). If you have these in any of your test fixtures already, just point us at the file.

**Specifically please confirm**: are you using `PoseidonSponge::hash_bytes_axiom` (or whichever method takes pre-formatted 32-byte LE Fr representations) and **NOT** `hash_bytes_flat` for this? Spec §3.1 calls this out as a sharp edge.

### Q5 — Does shellnet currently produce the new envelope format?

Your daemon already runs against `https://shellnet.ackinacki.org/graphql` and proves Circuit 1A on live blocks. We assume that means shellnet is already on the new envelope-hash format, but want to be explicit:

- Is shellnet today producing blocks where `envelope_hash = MerkleRoot(L0..L7)` (8-leaf SHA-256 tree)? Or is it still on the old `Poseidon(bincode::serialize(envelope))`?
- If shellnet is **still old-format**, where is a node we can target that's running the new format? (Local Docker from the bridge branch is fine.)
- If shellnet is **new-format**, we'd like to also run a local node for reproducibility — could you confirm the docker-compose / `make run` command on the bridge branch produces a node compatible with your daemon?

### Q6 — Status of the four historical-fixture data files

We have four real-data fixtures generated by `circuit-data-exporter` on `bridge_halo2_tests`:

- `circuit_test_data_L2_H16_prevH0_S1.json`
- `circuit_test_data_L2_H32_prevH16_S1.json`
- `circuit_test_data_L5_H12288_prevH1024_S11.json`
- `circuit_test_data_L6_H45056_prevH0_S11.json`

These were the basis of our existing on-chain regression suite (4 fixtures × Groth16 wrap × on-chain verify). They were generated against the **old** envelope format.

- Should we regenerate them against the new envelope format? (`circuit-data-exporter` would presumably need updating.)
- Or are equivalents already in your `test-data-gen/` outputs that we should consume directly?
- Long-term: would you be willing to host a small set of "official" reference fixtures in your repo (a `fixtures/` folder with a few new-format blocks at varying `(num_layers, num_prev_chain_steps)` values), so our Foundry regression suite can pin to known-good data?

### Q7 — Layer-hash chain proofs over GraphQL

Your `bridge-prover-lib/src/gql_client.rs` already pulls block BOC, attestations, and `bkSetUpdates`. What we don't yet see is the **layer-hash chain proofs** (the `Vec<DenseChainLink>` private witness for Circuit 2 → `verify_chain_of_dense_proofs`).

- Is there a GraphQL query that returns the dense-balanced-tree Merkle proof siblings for a given `(prev_max_level_layer_hash, target_layer_root, num_steps)` triple? Or do we need to compute it client-side from raw layer-tree data?
- If client-side: which crate/function do we use? `gosh-dense-balanced-tree` exposes `verify_chain_of_dense_proofs` for in-circuit use, but we need a *prover-side helper* that *generates* the proof siblings given the full layer trees. Is there one, or should we implement it?
- Does the AN node store all layer trees, or just the current top-layer root? (Affects whether we can pull historical data on-demand or need to mirror it ourselves.)

### Q8 — gnark wrappers — duplication check

Our default plan for on-chain verification is **native Halo2 SHPLONK** Yul verifiers, generated via `halo2-verifier-gen` against your VKs. No gnark step. We've already done this for the deposit prover (`Halo2Verifier.sol`, ~1.2M gas), and want to keep the toolchain consistent.

- Have you already produced gnark Groth16 wrappers for any of the four circuits? If yes, we'd like to know — partly to avoid duplicating work, partly because it gives us a fallback if the native Halo2 path runs into trouble.
- If you're not planning to do gnark wrappers, is there anything specific to the `attestation-bls-checker-circuit` (BLS12-381 hash-to-curve, ~22 SHA-256 blocks in-circuit) that you'd flag as a risk for the native-Halo2-on-Ethereum approach? (We'll measure gas in our Phase 3 either way; just curious if you've benchmarked.)

---

## Logistics

**Reply format**: a single document/message addressing Q1–Q8 in order is ideal. For Q4 specifically, attaching the test vector as a JSON file (or pointer to a file in your repo) is preferred over inline.

**SLA**: we'd like to start Phase 1 of our integration plan within 1–2 weeks. If any of these answers will take longer than that to produce, please flag the specific question early so we can plan around it.

**Joint sync**: if it's easier to walk through these on a call than answer in writing, we're happy to set up a 30-min meeting. Bring your `bridge-prover-daemon` running locally and we'll demo what our Foundry pipeline expects to ingest.

Thanks!
— bridge integration

---

## Internal Tracking (not for the partner)

| # | Question | Blocks phase | Owner | Status |
|---|---|---|---|---|
| Q1 | AN-node branch | Phase 0, all live testing | Partner | Open |
| Q2 | `transition_hashes` migration | Phase 5 (relayer needs the right hash) | Partner / TBD | Open |
| Q3 | Circuit MockProver status | Phase 1 (we patch against pinned SHA) | Partner | Open |
| Q4 | Poseidon test vector | Phase 0 acceptance criterion | Partner | Open |
| Q5 | shellnet envelope format | Phase 5 (relayer target) | Partner | Open |
| Q6 | Historical fixtures | Phase 2 acceptance, Phase 4 regression | Partner / us | Open |
| Q7 | GraphQL chain proofs | Phase 2.2 implementation strategy | Partner | Open |
| Q8 | gnark duplication / risk | Phase 3 risk mitigation | Partner | Open |

When responses arrive, update this row plus the corresponding §8 row in `docs/an_partner_integration_plan.md`, and produce `docs/an_integration_phase0.md` (the readiness report).
