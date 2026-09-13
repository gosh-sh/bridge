# M-audit scope (PR #36)

External / internal audit of the Ethereum beacon light-client stack **includes
this list**. It is not a later milestone.

| Surface | Where | Must check |
|---|---|---|
| Step circuit (k=19, 10 PI) | `eth-light-client-prover/src/step.rs` | BLS subgroup, 2/3 bits, SSZ finality + execution branches, committee Poseidon commitment |
| Live witness | `src/live_witness.rs` | Real 512 keys + `sync_aggregate`; synthetic OsRng is VkBlob-only |
| Recursive rotate | `examples/rotate_tree_n8.rs`, `ROTATE_VK_BLOB` | `accumulator_limbs = 12`; pairing of inner snarks |
| Opcode decider | tvm-sdk#284 (co-deploys with this contract) | `submitRotate` is emission-sound only with the KZG accumulator pairing; `disableOwnerRotation` is the intended flip |
| AN contract | `contracts/an/EthBeaconLightClient.sol` + `EthKeccak.sol` (standalone variant; shellnet runs `acki-nacki` `contracts/exchange`) | committee gate, monotonic head, `acceptBlockHashFromLightClient`, `submitAncestry` |
| Relayer | `crates/eth-light-client-relayer` | real committee fetch, no silent mock in systemd |
| Ancestry | `submitAncestry` + `src/header_rlp.rs` + `contracts/an/EthKeccak.sol` | keccak256(header RLP) + parentHash chain, ≤ 31, writes `_acceptedBlockHash` |
| Deposit flip | `eth-lc-relayer` `flip-owner` / daemon after first `submitUpdate` | `setLightClient` + `disableOwnerAnchors` + `disableOwnerRotation` (one-way; relayer keys = owner) |

Scripts already pinning the rotate blob: `scripts/check_rotate_vkblob_accumulator.sh`.
Opcode fixture: `scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh`.
