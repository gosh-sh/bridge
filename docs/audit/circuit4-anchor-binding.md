# Circuit 4 — `finalRoot` binding (ETH-15)

**Status:** written property for the flat `_isKnownAnchor` scan (NB-Q1).  
**Option A** (`anchorLayer` public input + per-window scan) remains the Circuit 4 re-keygen target (owner, 2026-09-08). It is also what closes ETH-09 (single-window scan). Until re-keygen, this file is the interim answer.  
**In circuit-audit scope:** this file + `crates/an-bridge-prover/bridge-event-prover-lib/src/prover.rs` + partner `bridge-event-prove-circuit`.

## What the contract checks

`withdrawByProof` accepts `pub.finalRoot` if it appears in **any** of the ten layer rings (`_isKnownAnchor`). Every ring slot was written by a successful `verifyBlock`, so an attacker cannot invent an anchor. The contract no longer asserts *which* layer the event was meant to sit in.

## What Circuit 4 must supply instead

1. The AN `WithdrawalInitiated` event is bound into the events-tree / block-tree witness (token, amount, recipient, sender, dapp, acc).
2. The nullifier is computed from those event fields plus `block_id` — **not** from `finalRoot`. Re-proving the same event against a later descendant keeps the same nullifier (WD-7).
3. `PUB_FINAL_ROOT` is the Poseidon root obtained by climbing the dense chain (`MAX_CHAIN_LEN = 11`) from the layer hash that contains the event. The daemon copies that value from `anchor.layer_hash_hex`.
4. A colliding `finalRoot` in a *different* layer window would require two `verifyBlock`-written Poseidon roots to be equal — a collision on the hash the circuits already use.

The missing on-chain statement is only “this event lives in layer L”. That is Option A. Until re-keygen, correctness of *layer identity* is this Circuit 4 climb, not a Solidity index.

## Tests

| Gate | What it pins |
|------|----------------|
| `WithdrawAnchorEviction.test_reproveAgainstLaterInWindowAnchor_succeeds` | Contract pays against any still-in-window descendant (mock Circuit 4). |
| `bridge-event-prover-lib::reprove_against_later_layer_hash_keeps_nullifier` | Same event, later `layer_hash_hex` → new `final_root`, same nullifier and identity slots. |
| Partner MockProver (`test_bridge_event_prove_circuit_for_all_collected_events_mock_prover`) | Constraint satisfaction on real Poseidon chains (circuits crate). |
