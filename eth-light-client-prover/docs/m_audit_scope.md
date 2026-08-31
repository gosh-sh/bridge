# M-audit scope (PR #36)

External / internal audit of the Ethereum beacon light-client stack **includes
this list**. It is not a later milestone.

| Surface | Where | Must check |
|---|---|---|
| Step circuit (k=19, 10 PI) | `eth-light-client-prover/src/step.rs` | BLS subgroup, 2/3 bits, SSZ finality + execution branches, committee Poseidon commitment |
| Live witness | `src/live_witness.rs` | Real 512 keys + `sync_aggregate`; synthetic OsRng is VkBlob-only |
| Recursive rotate | `examples/rotate_tree_n8.rs`, `ROTATE_VK_BLOB` | `accumulator_limbs = 12`; pairing of inner snarks |
| Opcode decider | tvm-sdk#284 | Without it `submitRotate` is **not** emission-sound; `disableOwnerRotation` is unsafe |
| AN contract | `EthBeaconLightClient_rotate_decider.patch` | committee gate, monotonic head, `acceptBlockHashFromLightClient` |
| Relayer | `crates/eth-light-client-relayer` | real committee fetch, no silent mock in systemd |
| Ancestry | `src/ancestry.rs` + `ancestry-one` | 31/32 coverage is parent-hash chain, not in the step proof |
| Deposit flip | `scripts/ursus/flip_deposit_to_light_client.md` | `setLightClient` + `disableOwnerAnchors` are one-way |

Scripts already pinning the rotate blob: `scripts/check_rotate_vkblob_accumulator.sh`.
Opcode fixture: `scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh`.
