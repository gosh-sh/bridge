# Fallback Path (Circuit 1b) — Operational Reference

This note covers how the bridge prover daemon detects whether an Acki
Nacki key block was finalized via the **Primary** or **Fallback** path
and which attestation circuit it routes through.

## Why two paths exist

Acki Nacki finalisation has two routes:

| Path | Trigger | Threshold | Block window |
|---|---|---|---|
| **Primary** | normal liveness | ≥ 2N/3 signers on a single `PRIMARY` attestation | within β blocks of the target |
| **Fallback** | primary deadline β missed | two attestations, each > N/2 signers, **same `block_id`**: one `PRIMARY` prefinalization + one `FALLBACK` target proof | within 2β blocks |

The Fallback path is a liveness escape valve for periods of producer
dissent (network partition, ~33% offline, etc.). It is rare in healthy
operation but **must** be handled — otherwise a single fallback key
block stalls every downstream bundle.

`β = MAX_ATTESTATION_TARGET_BETA = 30` blocks (~10s at 330 ms/block).

## Two circuits, identical public-instance shape

| Circuit | Verifies | Constraint highlights |
|---|---|---|
| 1a (Primary) | single ≥ 2N/3 attestation | one BLS pairing, signer-set commitment binding |
| 1b (Fallback) | two > N/2 attestations | two BLS pairings + same-`block_id` equality across the pair |

Both circuits publish exactly the same 4 public instances:

```
[block_id, bk_set_commitment, block_seq_no, last_seen]
```

So the verifier doesn't need to know the path at the instance layer —
it just picks the matching verifying key (`primary_vk` vs `fallback_vk`)
based on the `attestation_circuit` tag carried in the IPC `ProofRequest`
(schema v3).

## Detection: how the prover daemon picks 1a vs 1b

The classifier lives in `bridge-prover-lib/src/attestation_fetcher.rs`
(`fetch_attestation_evidence`). It is **structural**, not heuristic:

1. GQL query: `Block(seq_no=N).attestations[]` returns every attestation
   the producer committed to that block, with `{ block_id, parent_block_id, target_type, envelope_hash, aggregated_signature, signature_occurrences }`.
2. Partition the array by `target_type`:
   - `target_type == 0x00000000` → `PRIMARY` group
   - `target_type == 0x01000000` → `FALLBACK` group
3. Classify by shape:

| `(PRIMARY_count, FALLBACK_count)` | Result | Circuit |
|---|---|---|
| `(1, 0)` | `AttestationEvidence::Primary(att)` | 1a |
| `(1, 1)` with same `block_id` | `AttestationEvidence::Fallback { primary, fallback }` | 1b |
| anything else (e.g. `(2, 0)`, `(0, 1)`, `(1, 1)` w/ different `block_id`) | hard error | — |

Threshold validation (`≥ 2N/3` for 1a, `> N/2` for 1b) is **not**
performed off-circuit. It is enforced inside the circuit by the BLS
checker — the daemon's job is only to route to the right circuit.

## Operational implications

- **Key materialisation.** Both VKs and PKs are generated on the
  prover's first cold start (see `KeyManager::ensure_primary_keys` +
  `KeyManager::ensure_fallback_keys`). Disk artifacts:
  - `params/primary_vk.bin`, `params/primary_pk.bin`, `params/primary_config_params.json`
  - `params/fallback_vk.bin`, `params/fallback_pk.bin`, `params/fallback_config_params.json`

- **Verifier requirement.** `bridge-verifier-daemon` refuses to start
  unless `fallback_vk.bin` is present — the failure mode is loud, not
  silent, because a missing VK would otherwise cause a fallback-bundle
  to be rejected.

- **Memory.** PKs are loaded on demand per bundle (~3.7 GB envelope is
  preserved). A fallback key block momentarily holds the fallback PK
  instead of the primary one; it's unloaded before Circuit 2 runs.

- **Test-data path.** `bridge_test_data_gen::generator::generate_test_data_fallback_all_sign`
  is used during keygen to derive the fallback circuit's row count and
  config without needing a live fallback attestation.

- **Backwards compatibility.** Proof JSONs are schema v3. v2 files
  (without the `attestation_circuit` field) deserialise as `"primary"`
  via `#[serde(default)]`, so historical bundles still verify.

## What the verifier does with the tag

`bridge-verifier-daemon/src/main.rs` branches on
`request.attestation_circuit`:

```rust
let primary_verified = match request.attestation_circuit {
    ipc::AttestationCircuit::Primary  => verifier::verify_primary_proof(...),
    ipc::AttestationCircuit::Fallback => verifier::verify_fallback_proof(...),
};
```

`verify_primary_proof` and `verify_fallback_proof` share `verify_kzg_proof`
(Blake2b transcript, SHPLONK multiopen, single-strategy, Bn256/KZG) —
the only thing that differs is which VK they pass in. The shared core
is the same one Circuit 4 (`bridge-event-prover-lib`) reuses.

## Shellnet status

In current shellnet operation the cluster is healthy enough that no
fallback path has been observed — every key block has a single
`PRIMARY` attestation with near-full signer set. The classifier still
takes the `Primary` branch and tags every proof `1a`. Once a fallback
event occurs the daemon will automatically route to 1b without any
config change.

## References

- Circuits repo: `attestation-bls-checker-circuit/src/primary_circuit.rs`,
  `attestation-bls-checker-circuit/src/fallback_circuit.rs`
- Detection: `bridge-prover-lib/src/attestation_fetcher.rs`
- Dispatch: `bridge-prover-daemon/src/main.rs` (`match &evidence`)
- VK selection: `bridge-verifier-daemon/src/main.rs`
  (`match request.attestation_circuit`)
- IPC schema: `bridge-prover-lib/src/ipc.rs` (`AttestationCircuit`,
  `ProofRequest`, `PROOF_REQUEST_SCHEMA_VERSION = 3`)
