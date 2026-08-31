# M5 — AN contract `EthBeaconLightClient` + deposit-path anchor wiring

**Status: source landed; compile/deploy on the operator Mac (see §Compile).** The
beacon sync-`step` VkBlob emitted in [`m5_vkblob.md`](m5_vkblob.md) now has an
on-AN consumer: a TVM-Solidity contract that verifies the step proof through the
`ZKHALO2VERIFYWITHVK` opcode and turns each accepted finality update into a
canonical execution-block-hash anchor the deposit path already consults.

This closes the ETH→AN canonicality trust seam that `USDCBridge` left open in its
own comments ("or eventually an Ethereum light client — means adding a writer for
this mapping").

## Files

| File | Repo | Change |
|------|------|--------|
| `contracts/exchange/EthBeaconLightClient.sol` | `acki-nacki` | **new** — verifier + head + anchor registry + push |
| `contracts/exchange/USDCBridge.sol` | `acki-nacki` | **additive** — `_lightClient` slot + `setLightClient`/`getLightClient` + `acceptBlockHashFromLightClient` writer |
| `scripts/embed_step_vk_blob.py` | `acki-nacki-bridge` | **new** — rewrites the `VK_BLOB` literal + sha256 comment from the fixture (mirror of `embed_deposit_vk_blob.py`) |

## `EthBeaconLightClient` — what it does

`submitUpdate(bytes proof, bytes publicInputs)` (permissionless):

1. Parse the 8 LE-Fr public inputs (`step.rs::pack_step_instances` order):
   `[attested_slot, finalized_slot, beacon_root_hi, beacon_root_lo,
   participation, committee_commitment, exec_block_hash_hi, exec_block_hash_lo]`.
2. **Pre-accept guards** (cheap, within the external-message budget):
   - supermajority re-assert `3·participation ≥ 2·512` (already in-circuit; belt-and-braces),
   - **committee gate** `committee_commitment == _currentCommittee` (fail-closed if 0),
   - monotonic head `finalized_slot > _finalizedSlot`.
3. `tvm.accept()` then the opcode: `require(gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof))`.
4. Advance head (`_finalizedSlot/Root/ExecutionBlockHash`, `_updatesApplied++`),
   register `_provenExecutionBlockHash[execHash] = true`.
5. If a sink is configured, push `acceptBlockHashFromLightClient(_l1ChainId, execHash)`
   into the `USDCBridge` so `finalizeDeposit` reads it synchronously from its own storage.

The **committee gate is the crux of trustlessness**: the step proof only shows
*some* committee (whose Poseidon commitment = PI #5) signed with a supermajority —
it says nothing about that committee being the real one. Equality with
`_currentCommittee` pins it to the chain-anchored committee. `_currentCommittee`
is bootstrapped from a weak-subjectivity checkpoint (constructor) and advanced by
`setCommitteeCommitment` (owner) — the seam the **rotate** proof
(`src/rotate.rs`) will later replace with a permissionless `submitRotate`, at
which point the committee chain becomes fully trustless from the checkpoint.

### Admin / getters

`setPubkey`, `setUsdcBridge`, `setCommitteeCommitment` (all owner-pubkey);
`getHead`, `isProvenExecutionBlockHash`, `isAcceptedBlockHash(chainId, hash)`
(drop-in shape matching `USDCBridge`), `getConfig`, `getVersion`.

## Why the anchor is *pushed*, not *pulled*

TVM messaging is asynchronous, so `finalizeDeposit` cannot synchronously call a
getter on the light client. Instead the light client **writes** the proven hash
into `USDCBridge._acceptedBlockHash` (the exact set `finalizeDeposit` already
gates on) via the new authorized writer. The write is authorized solely by being
the configured `_lightClient` — no human asserts canonicality, the proof does.
This is independent of `_ownerAnchorsEnabled` (which governs the owner path); the
owner's one remaining action is granting trust once via `setLightClient`.

`_lightClient` follows the `_expectedBridgeFr` convention: it is **not** threaded
through the `onCodeUpgrade` migration cell, so it must be re-set by the owner
after any code upgrade. No change to `finalizeDeposit`, `updateCode`, or the
migration format.

## Known scope limit (M5)

The step proof anchors the **finalized checkpoint** block's execution hash. A
deposit in a non-checkpoint block is covered only once an ancestry / receipts
proof ties it to an anchored head — a later milestone. Today the relayer submits
deposits whose block the light client has anchored.

## VK_BLOB embed

`bytes constant VK_BLOB` = `eth-light-client-prover/fixtures/step_vkblob/step_vk_blob.bin`
(17 573 B, Base **v1** Blake2b, **10 PI** — 2-level committee commitment at [5] +
attested `state_root` at [8|9], sha256 `bd108c08…7d21bab0`). Rotate with:

```bash
cd eth-light-client-prover && cargo run --release --example export_step_vk_blob   # re-emit
scripts/embed_step_vk_blob.py ../acki-nacki/contracts/exchange/EthBeaconLightClient.sol
scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh                                    # opcode fixtures
# then recompile + redeploy the contract (the blob lives in code)
scripts/embed_step_vk_blob.py --check ../acki-nacki/contracts/exchange/EthBeaconLightClient.sol
```

`bytes constant ROTATE_VK_BLOB` = `fixtures/rotate_vkblob/rotate_vk_blob.bin`
(3 232 B, Base **v1** Blake2b, **15 PI** — 12 KZG accumulator limbs +
[current, next, period], `accumulator_limbs=12` in the header so the opcode runs
the decider, sha256 `037cd274…4c57b81e`). Rotate with
`EMIT_VKBLOB=1 cargo run --release --features aggregation --example rotate_tree_n8`
(big-RAM host), then `scripts/embed_rotate_vk_blob.py` +
`scripts/sync_rotate_opcode_fixtures_to_tvm_sdk.sh`. tvm-sdk#284 **co-deploys
with this contract**; an accepted rotate enforces the folded shard/step proofs.
Call `disableOwnerRotation()` at the deposit flip.

## Compile (operator Mac) + acceptance

The `gosh.zkhalo2VerifyWithVK(bytes,bytes,bytes)→bool` builtin only exists on the
TVM-Solidity-Compiler branch **`origin/halo2_verify`** (`c7725d6`, `sold 0.79.3`)
— verified in-tree:

```
compiler/libsolidity/ast/Types.cpp        : "zkhalo2VerifyWithVK" → {bytes,bytes,bytes}→bool
compiler/libsolidity/codegen/TvmAst.cpp   : {"ZKHALO2VERIFYWITHVK", {3,1,true}}   (3 operands, impure)
compiler/libsolidity/codegen/TVMFunctionCall.cpp : GoshZKHALO2VERIFYWithVK → "ZKHALO2VERIFYWITHVK"
```

so the call-site is confirmed correct against the branch. **Compilation is not
reproducible on this Linux dev host** — the `halo2_verify` `sold` pins a
`tvm_abi` git dep on a tvm-sdk revision (`6f86fba3` on
`full_dex_test_with_final_halo2_circuit_with_vk`) that has since moved, and all
checked-in `sold` binaries are macOS arm64. Compile on the operator Mac (same box
that produces `USDCBridge.tvc`):

```bash
cd acki-nacki/contracts
sold --tvm-version gosh --base-path . exchange/EthBeaconLightClient.sol -o exchange/
sold --tvm-version gosh --base-path . exchange/USDCBridge.sol           -o exchange/
```

Acceptance gate (same protocol the deposit VK swap uses): recompile the
**unmodified** `USDCBridge.sol` first and confirm its code-hash still matches the
deployed one, proving the toolchain, then trust the new `EthBeaconLightClient.tvc`
and the recompiled `USDCBridge.tvc` (ABI gains only `setLightClient` /
`getLightClient` / `acceptBlockHashFromLightClient`; storage gains one non-migrated
`address _lightClient`).

The end-to-end opcode acceptance of the embedded blob is already proven
in-process by the `tvm_vm` test
`round_trip_step_light_client_real_proof_returns_true` (see `m5_vkblob.md`) — the
same VkBlob bytes, same opcode handler.

## Static validation done here

- VK_BLOB header `VKBLOB\0\0` + version 1 + Base shape + embedded `BaseCircuitParams`
  `{k:19, num_advice_per_phase:[132], …}`; sha256 matches the fixture sidecar.
- Builtin arity/signature matched against `origin/halo2_verify`.
- Public-input decode mirrors `USDCBridge._parseBlockHash` byte-for-byte (LE, hi<<128|lo).
- Brace/paren/bracket balance on both files.

## Operational constraints (go / no-go)

These are the rollout limits of the M5 contract + M6 rotate, written so a
reviewer does not have to reconstruct them from comments. Relayer, ancestry
check, E2E runbook, audit scope, and the `finalizeDeposit` flip procedure are
**in this PR**.

**Deposits in non-checkpoint blocks (31/32).** `submitUpdate` records
`finalized_execution.block_hash` — one execution hash per epoch (~6.4 min).
`submitAncestry(bytes[] headerRlps)` walks that checkpoint's execution
parent-hash chain (`keccak256(header RLP)` + RLP `parentHash`, ≤ 31 parents)
and pushes each hash into `USDCBridge._acceptedBlockHash`. Operator:
`eth-lc-relayer submit-ancestry`.

**Missed checkpoints.** The head is skip-*forward*: a later checkpoint may
be submitted without the skipped ones. A skipped checkpoint **of the current
committee** can still be late-registered: `submitUpdate` verifies the proof,
records the exec hash (and pushes it to `USDCBridge`), and does **not** move
the head back. Same-slot replay and already-proven hashes still revert.

- Head liveness does **not** require every 6.4 min update — jumping to the
  latest checkpoint is enough to keep the committee/WS clock moving.
- 1/32 deposit coverage: catch up later in the same period by late-registering
  the skipped checkpoint proofs. After `submitRotate` the previous committee
  is no longer accepted (`ERR_WRONG_COMMITTEE`), so a gap that spans a
  rotation waits for ancestry.
- Intended cadence (M0 §7 / M5 relayer): ≥ 1 update per period (~27 h) for
  weak-subjectivity safety, plus on-demand so a pending deposit's checkpoint
  is anchored.

**Recovery after `disableOwnerRotation()`.** Everyday committee advance is then
only `submitRotate` (unbroken period chain). Catch-up by replaying rotations is
safe for **at most one sync-committee period (~27 h)**. Past that lag the owner
calls `reAnchorCommittee(commitment, period)` — a logged WS hop
(`CommitteeReAnchored`, `reAnchorsApplied++`) that does **not** write exec
hashes. `setCommitteeCommitment` stays disabled. tvm-sdk#284 co-deploys with
this contract, so `disableOwnerRotation()` is part of the deposit flip; the
relayer SLA is what keeps `reAnchorsApplied` at 0.

**Anchor retention.** `_provenExecutionBlockHash` / `_acceptedBlockHash` are
permanent; there is no TTL. At checkpoint cadence that is ~225 entries/day,
~82 k/year, duplicated across the two contracts — accepted. If ancestry
extends coverage to every block (~2.6 M/year) a retention window becomes a
product trade (older deposits stop being claimable) and will be designed
then, not now.

**Relayer / audit.** Relayer crate: `crates/eth-light-client-relayer`
(`eth-lc-relayer` CLI). systemd is the **live** loop (no hardcoded
`--dry-run --mock-prove --no-rotate`). Auto `submitRotate` is **on** by
default; `--no-rotate` is the shadow opt-out. tvm-sdk#284 co-deploys with
this contract. Audit scope: [`m_audit_scope.md`](m_audit_scope.md).
`finalizeDeposit` flip: `scripts/ursus/flip_deposit_to_light_client.md`
(`disableOwnerAnchors` + `disableOwnerRotation`).
E2E: `scripts/ursus/eth_lc_shellnet_e2e.md`.

**`accumulator_limbs = 12`.** Enforced by
`scripts/check_rotate_vkblob_accumulator.sh` (fixture header + sha256 pin +
patch hex identity + byte-11-cleared negative probe), by the `EMIT_VKBLOB`
path in `examples/rotate_tree_n8.rs`, and by `embed_rotate_vk_blob.py` /
`sync_rotate_opcode_fixtures_to_tvm_sdk.sh` refusing a 0. See
[`m6_rotate_recursive.md`](m6_rotate_recursive.md) §Decider.

## Next seams

- **Testnet E2E**: follow `scripts/ursus/eth_lc_shellnet_e2e.md`.
