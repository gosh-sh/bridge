# Circuit 4 — design notes

## 1. Why the sender's dApp-id is not a public input

**Question.** The PIs expose the *destination* dApp-id (`PUB_DAPP_FR = 6`) and the *sender's*
`account_id` (`PUB_SENDER_ACC_FR = 5`) — but there is no `PUB_SENDER_DAPP_FR`. Why the
asymmetry?

**Answer.** The sender's dApp-id is not in any bytes the circuit already sees. Adding it as
a PI would require a whole new proof branch, for a field this bridge does not use.

Concretely:

- **The destination's dApp-id is essentially free.** Slots 6-7 are loaded straight from the
  witness (`self.account_dapp_id` / `self.account_id`, `bridge_event_prove_circuit.rs:690-691`)
  and consumed by `ext_msg_leaf = Poseidon96(dapp_id, account_id, repr_hash)` (`:689`), which
  the events Merkle proof already needs. They bind *which contract's* ext-out root we are
  verifying — they are the destination TokenBridge's identity.
- **The sender's `account_id` is available from the message itself.** Slot 5 is decoded
  algebraically from the sender cell inside the event body (`:806-819`); those bytes are
  already SHA-256-bound to the body cell, so no extra witness is trusted.
- **The sender's `dapp_id` is not.** A TVM `MsgAddrStd$10` message address carries only
  `anycast + workchain_id + address` — no dApp-id (`tvm-sdk/tvm_block/src/messages.rs`). A
  sender's `dapp_id` lives in `ShardAccount.dapp_id`, i.e. account state, reachable only via
  a **second Merkle path proof** into the shard-account tree plus another SHA-256 chain. Too
  much circuit for a field the current withdrawal flow does not consume.

**If it is ever needed** the right fix is on the AN side: put `senderDappId` into the
`WithdrawalInitiated` event body so it can be parsed from the BOC the same way `tokenId` /
`amount` / `dstChainId` already are. Then decoding it becomes a trivial addition to the
existing body-cell parse, and no shard-account proof is required.

Related open flag on the same decoder path: the sender-cell prefix bits (`std_addr$10` tag /
`anycast` / `workchain`) are **not** currently constrained. Acceptable for a bridge where the
sender is always a TIP-3 contract on workchain 0. **Revisit if a multi-workchain bridge is
ever needed.**


## 4. Anchor sweep on W=128 — open mitigation menu

`withdrawByProof` accepts `finalRoot` if it appears in **any** `_layerWindows[L]`
(`AckiNackiBridge.sol:1044-1057`, flagged as an open soundness gap in
`EVM-contracts-spec.md:751` — "Anchor layer is not asserted"). Worst-case sweep on
`W = 128, MAX_LAYERS = 10` is 1 280 SLOADs.

Three options were discussed; none has landed:

1. **Bloom / hash-table anchor set** — maintain a `mapping(bytes32 => bool) _activeAnchors` in
   parallel with `_layerWindows`, insert on append, delete on eviction. Membership becomes
   O(1). Highest storage cost, best worst-case gas.
2. **Prover-supplied `anchorLayer` hint** — add a PI slot; contract scans only that layer's
   `W` entries. Small info leak (which layer) — acceptable given no anonymity goal.
3. **Flat storage array** — repack all layer hashes into one contiguous `bytes32[]` for
   warm-slot locality. Least invasive, only constant-factor improvement.

Recommendation from the 2026-05 review was **(1) or (2)**, with a mild preference for (2)
because it also closes the "anchor layer is not asserted" gap the current spec flags.

## 5. Cross-repo invariant: BridgeState mirror is the reference

The AN-side prover crate (`crates/an-bridge-prover/bridge-prover-lib/src/bridge_state.rs`)
maintains an off-chain `BridgeState` whose `HistoryWindow` **must** stay byte-for-byte
equivalent to the on-chain `_layerWindows`. The verifier daemon
(`crates/an-bridge-prover/bridge-verifier-daemon/src/main.rs`) checks anchor membership
against `state.flatten_layer_hashes()` — this is the reference implementation for
`_isKnownAnchor` in Solidity.

If those diverge (append order, eviction policy, zero-hash filtering), Solidity will accept
proofs the daemon rejects, or vice versa. Any change to `HistoryWindow` semantics on either
side must be mirrored on the other; a divergence is a bug even if both sides individually
"look right".

---

## Circuit shape (reference)

- `K = 19`, `num_advice_per_phase = vec![16]`, `lookup_bits = Some(18)`
  (`bridge-event-prove-circuit/src/test_helpers.rs:38-49`).
- `TOTAL_PUBLIC_INPUTS = 10` (`bridge_event_prove_circuit.rs:123`).
- `RECIPIENT_LEN_FIXED = 20` — hard-coded Ethereum-address size
  (`bridge_event_prove_circuit.rs:128`). Non-EVM targets would need a circuit change and
  fresh VK.
