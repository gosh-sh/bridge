# M5 — rotate ↔ step join: trustless committee chain (`submitRotate`)

**Status: contract wired; rotate VkBlob emit HARDWARE-BLOCKED.** The
`EthBeaconLightClient` now has the `submitRotate(proof, publicInputs)` method and
the one-way `disableOwnerRotation()` switch that together make the sync-committee
chain trustless from the weak-subjectivity checkpoint. The one missing piece is
the **rotate circuit's VkBlob**, whose keygen is memory-bound beyond our largest
box — see §Hardware blocker.

## Why a rotate proof is needed at all

`submitUpdate` (the step path) only trusts a finality update signed by a committee
whose Poseidon commitment equals `_currentCommittee`. That is what stops a prover
from inventing a committee that signs. But it just moves the question one level
up: **how does `_currentCommittee` advance each period without trusting the
owner?** That is the rotate proof's job — it is the only permissionless writer of
`_currentCommittee`.

Trust reduces to a single weak-subjectivity checkpoint: bootstrap
`_currentCommittee` once from a trusted source, then every subsequent committee is
proven the legitimate successor of the previous one. After
`disableOwnerRotation()` the owner key is off the committee-advance path entirely.

## Trust model (R2 — self-contained rotate, chosen)

The rotate proof is **self-contained**, so the contract stays simple and step is
untouched. It establishes, all in-circuit:

1. the committee whose Poseidon commitment is **PI #0** signed an attested beacon
   header with a supermajority (the same BLS aggregate + domain check the step
   circuit does);
2. the committee whose commitment is **PI #1** occupies that *signed* state's
   `next_sync_committee` slot — `htr(SyncCommittee)` (`committee.rs`) +
   `next_sync_committee_branch` @ gindex **87** (`rotate.rs`,
   `NEXT_SYNC_COMMITTEE_GINDEX`) reconstruct the attested `state_root`.

So *proving PI #0 == the current trusted committee is itself proof that PI #1 is
its legitimate successor*. No `state_root` needs to cross the ABI, and the step
VkBlob / fixtures / embed shipped in [`m5_eth_beacon_light_client.md`](m5_eth_beacon_light_client.md)
are **not disturbed**.

> Rejected alternative **R1** (light rotate = SSZ-anchor only, current `rotate.rs`):
> rotate exposes `state_root` and the contract checks it against a step-accepted
> `state_root`. This needs the **step** circuit to also expose its attested
> `state_root` (PI 8 → 10) → a step VkBlob + fixture + embed re-emit. R2 avoids
> that ripple by folding the current-committee signature into rotate. The extra
> BLS cost is moot: rotate is memory-bound on the SSZ committee root regardless
> (§Hardware blocker).

## Public-input layout (3 × 32 B LE Fr)

```text
[0] current_committee_commitment   — Poseidon commitment to the SIGNING committee
[1] next_committee_commitment      — Poseidon commitment to the successor committee
[2] period                         — sync-committee period the hand-off advances into
```

Decoded by `EthBeaconLightClient._parseRotatePublicInputs` (little-endian, same as
the step / deposit decoders). Commitments are full-width Poseidon outputs;
`period` is a `u64`.

## Contract surface (`EthBeaconLightClient.sol`)

```solidity
function submitRotate(bytes proof, bytes publicInputs) public;   // permissionless
function setCommitteeCommitment(uint256 commitment, uint64 period) // owner, bootstrap-only
    public;                                                        //   (gated by _ownerRotationEnabled)
function disableOwnerRotation() public;                           // owner, one-way
function getCommitteeState() external view returns (uint256, uint64, bool);
```

`submitRotate` logic:

1. parse 3 PIs;
2. `require(_currentCommittee != 0)` (fail-closed) and
   `require(currentCommitteeCommitment == _currentCommittee)` — chain to trusted;
3. `require(period > _committeePeriod)` — monotonic anti-replay;
4. `tvm.accept()` then `require(gosh.zkhalo2VerifyWithVK(ROTATE_VK_BLOB, publicInputs, proof))`;
5. advance `_currentCommittee = next`, `_committeePeriod = period`; emit `CommitteeRotated`.

The **trust-reduction switch** mirrors `USDCBridge`'s
`_ownerAnchorsEnabled`/`disableOwnerAnchors`: while `_ownerRotationEnabled` the
owner may `setCommitteeCommitment` directly (checkpoint bootstrap / syncing before
the VkBlob exists); `disableOwnerRotation()` clears it permanently, leaving
`submitRotate` the only writer. One-way on purpose.

## Hardware blocker (why the VkBlob is not emitted yet)

`ROTATE_VK_BLOB` is an **empty placeholder**, so `submitRotate` reverts until it is
populated + the contract redeployed (the blob lives in code). Populating it needs
a rotate `keygen_vk`, which synthesizes the circuit once — and the rotate circuit
contains the **SSZ committee root** (`htr(SyncCommittee)` over 512 pubkeys ≈ 1023
SHA-256 blocks), whose *assignment* (independent of `2^k` padding) is memory-bound:

| Box | RAM | Result at k≈26 |
|-----|-----|----------------|
| n14 (our largest) | 125 GB + 128 GB swap | **thrashes** — assignment exceeds RAM, swaps to death |
| dev host | 125 GB | same |

This is the exact reason `tests/rotate_mock_prover.rs::full_rotate_in_circuit` is
`#[ignore]`. **Chosen unblock: recursive-aggregation split** (no special hardware
needed) — see [`m6_rotate_recursive.md`](m6_rotate_recursive.md). The committee SHA
root factors exactly (balanced tree) into N shard proofs (~127 SHA each, fit @
k20) + a cheap snark-verifier aggregation; **brick #1 (sharding) is green on n14**
(`tests/rotate_shard_mock_prover.rs`, validated byte-for-byte against the
monolithic root incl. live mainnet data). Alternatives considered: a ≥256–512 GB
box (one-shot monolithic keygen — works but needs hardware we don't have) and a
Poseidon-only committee anchor (mainnet doesn't expose one).

Once emitted, wiring is a drop-in: add `examples/export_rotate_vk_blob.rs`
(mirror of `export_step_vk_blob.rs`), `scripts/embed_rotate_vk_blob.py`, a
`tvm-sdk` `rotate_light_client` opcode fixture + test (mirror of
`step_light_client`), then `embed` + redeploy.

## What is done vs pending

| Piece | State |
|-------|-------|
| `submitRotate` + parse + monotonic period + committee chain | ✅ contract |
| `disableOwnerRotation()` trust-reduction switch + `getCommitteeState` | ✅ contract |
| Bootstrap `setCommitteeCommitment(commitment, period)` gated by `_ownerRotationEnabled` | ✅ contract |
| R2 self-contained trust model + PI layout | ✅ designed |
| Rotate circuit full instance exposure (`pack_rotate_instances` + BLS-in-rotate) | ⏳ (rotate.rs is SSZ-anchor-only today) |
| Rotate real proof + `ROTATE_VK_BLOB` emit | ⛔ hardware-blocked (see above) |
| tvm-sdk `rotate_light_client` opcode fixture | ⏳ after emit |

## Compile / deploy

Same as [`m5_eth_beacon_light_client.md`](m5_eth_beacon_light_client.md) §Compile
(operator Mac, `sold` on `origin/halo2_verify`). The `submitRotate` call-site uses
the same `gosh.zkhalo2VerifyWithVK(bytes,bytes,bytes)→bool` builtin already
validated there.
