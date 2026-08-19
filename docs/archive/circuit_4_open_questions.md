> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Circuit 4 (Bridge Event Prove) — open design questions (2026-05-17)

## TL;DR

Partner's `bridge-event-prove-circuit` ([repo](https://github.com/gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits/tree/main/bridge-event-prove-circuit)) is the AN→ETH **withdrawal-event** proof: it witnesses that the AN-side `TokenBridge` contract emitted a `WithdrawalInitiated(tokenId, dstChainId, amount, recipient, sender)` whose hash chain anchors into the bridge's stored layer-hash window.

We **cannot deploy a real `withdraw()` on Ethereum yet**, because the circuit as published keeps `dstChainId`, `amount`, `recipient`, `sender` as **private witnesses** — so the bridge contract has nothing actionable to consume.

This document is the running checklist of design questions blocking Phase B (real `withdraw()`). Phase A (the contract scaffolding landed in this commit) is **complete** and lives behind `AckiNackiBridge.verifyEvent` + the `LayerWindowPushed` storage.

## What's in Phase A (this commit)

| Surface | Status |
|---|---|
| `AckiNackiBridge.sol` rolling `_layerWindow[100]` + `layerWindowHead` + `LayerWindowPushed` event | ✅ landed |
| `AckiNackiBridge.bridgeEventDappFr` / `bridgeEventAccFr` immutables | ✅ landed |
| `AckiNackiBridge.verifyEvent(proof, tokenId) → bool` | ✅ landed (mock-tested) |
| `IBridgeEventVerifier.sol` + `BridgeEventVerifier.sol` adapter (103 public inputs) | ✅ landed |
| `IBridgeEventGroth16Verifier.sol` interface (against gnark-generated output) | ✅ landed |
| Foundry tests: 16 in `AckiNackiBridgeVerifyEventTest` (constructor, ring buffer wrap, verifyEvent paths, identity/window forwarding) | ✅ 16/16 green |
| gnark wrapper skeleton (`crates/bridge-snark-utils/gnark-wrappers/circuit-4/`) | ✅ builds (identity stub mirror of `circuit-2`) |

Phase A is intentionally **non-paying**: a successful `verifyEvent` only emits `BridgeEventVerified(tokenId, msg.sender)`. No ETH moves.

## Phase B blockers — five open questions for partner

### Q-CIRC4-1 — Make `amount` and `recipient` public inputs

> The circuit's `EVENT_LAYOUT_COMPARISON.md` lists `amount` and `recipient` as private witnesses. Without these as public inputs the bridge cannot pay anything to anyone.

Proposed extension of the public-input layout:

```
[0]            tokenId        (uint32)
[1]            amount         (uint128)        ← new
[2]            recipientFr    (Fr-encoded 20-byte EVM address; same `bytes_to_fr` convention as dappFr/accFr) ← new
[3]            dstChainId     (uint256)        ← new (or keep private if we only ever target chainId == 1)
[4]            dappFr
[5]            accFr
[6..=105]      layerHashes    (100 candidates)
```

Total: 106 public Fr.

Notes:
- BN254 `Fr` is 254 bits (≈31.75 bytes); a 20-byte EVM address fits in a
  single Fr, so a hi/lo split is unnecessary — same encoding the partner
  already uses for `account_dapp_id`/`account_id` via `bytes_to_fr`.
- Packing `(amount, recipient)` into one Fr does **not** work
  (128 + 160 = 288 bits > 254-bit Fr); they must be separate slots.
- Decision needed from partner: which slot order matches the partner's preferred witness encoding.
- Either layout is a hard ABI break for `BridgeEventVerifier.sol` (slots
  0..2 are hard-coded against the legacy `[tokenId, dappFr, accFr]`); we
  rerun gnark setup and patch the adapter once v2 is published. A softer
  alternative is to append the new fields at the tail
  (`[103]=amount, [104]=recipientFr, [105]=dstChainId, [106..]=layerHashes`),
  preserving the existing 103-input prefix; free on the circuit side, saves
  us one re-spin.

### Q-CIRC4-2 — Nullifier

> Circuit 4 is currently replayable. Anyone can resubmit a valid `verifyEvent` (proven by `test_verifyEvent_isReplayable_byDesign_inPhaseA`). For a real `withdraw()` this is fatal — one valid AN-side event would let an attacker drain the bridge by replaying the proof.

The standard fix is a **circuit-side nullifier**: a public Fr input derived from the event's binding witnesses such that two distinct withdrawals produce two distinct nullifiers, while two replays of the same withdrawal produce the same one.

**Preferred (cheapest) option**: expose the existing `block_leaf =
Poseidon96(block_id, envelope_hash, ext_out_root)` (computed at
`bridge_event_prove_circuit.rs:719-744` as an internal witness for the
events Merkle proof) as a public Fr. Zero new constraints, zero new
Poseidon caps — the value is already in the circuit, we just need it
exposed. Alternatively `Poseidon(block_id, envelope_hash)` works the
same way and avoids `ext_out_root` if it's inconvenient to expose.

Fallbacks (if `block_leaf` isn't a fit):
- 7-field Poseidon
  `Poseidon(envelope_hash || block_id || tokenId || amount || recipient || sender_dapp || sender_acc)` —
  all ingredients already SHA-256-bound, collision-safe, but more
  Poseidon caps than `block_leaf`.
- Plain `nullifier := envelope_hash` — **unsafe**: two identical
  `WithdrawalInitiated` events from the same account in the same block
  produce the same `repr_hash`, so the second withdraw would silently
  collide. `block_id` must be in the formula.

Bridge-side: an `nullifierUsed[bytes32] public` mapping that `withdraw()` consults + writes after a successful proof. Estimated cost: 1 SSTORE per withdraw (~22 100 gas cold, ~5 000 warm).

### Q-CIRC4-3 — `dstChainId` semantics

> The circuit currently witnesses `dstChainId` (uint256) but doesn't enforce anything about it. For a multi-chain bridge we'd want the verifier to assert `dstChainId == THIS_CHAIN_ID` so a proof targeting "withdraw to BSC" cannot be replayed on Ethereum.

Two options:
- **Per-chain circuit + VK**: ship one circuit + one VK per target chain, partner hard-asserts `dstChainId == EXPECTED` inside the circuit body (with `EXPECTED` baked in as a circuit constant per VK). Simple but multiplies trusted-setup ceremonies and VKs by N target chains.
- **`dstChainId` public**: lift to public input; the verifier asserts `dstChainId == block.chainid`. Single circuit, single VK, multi-chain.

Recommended: lift to public (option 2). It composes better with the existing `dappFr`/`accFr` immutability pattern, and `block.chainid` is exact on mainnet, testnets, L2s, and Anvil.

### Q-CIRC4-4 — Variable-length `recipient`

> `EVENT_LAYOUT_COMPARISON.md` §5.6 mentions a variable-length recipient path, but the current circuit hardcodes `RECIPIENT_LEN_FIXED = 20` (Ethereum 20-byte addresses). Solidity bridge can require `recipient.length == 20` and reject anything else.

Decision: defer variable-length to Phase C. Phase B uses fixed 20-byte recipient ⇒ a single public-input layout works for all Ethereum withdrawals.

### Q-CIRC4-5 — Trusted setup ceremony

> The Phase A gnark wrapper is an identity stub (same trust model as Circuits 1A/1B/2 — internal risk **R15**). A trusted-setup ceremony (**R14** / Phase 9) is **strictly downstream** of the Phase 8 R&D that replaces the stub with a real Halo2-in-gnark verifier — running the ceremony before that would produce a perfectly-secured stub. Once Phase 8 lands, we decide whether to bundle Circuit 4's ceremony with the existing four-circuit ceremony (Phase 8 of `integration_plan.md`) or run a separate one.

Bridge-side cost is identical either way; the question is logistical.

## What needs to land before flipping `withdraw()` on

1. Partner publishes Circuit 4 v2 with public `amount`/`recipient`/`dstChainId` and a nullifier (Q-CIRC4-{1,2,3}).
2. `bridge-snark-utils` grows a `bridge_event_prover` module (mirror of `layer_hashes_prover`).
3. `gnark-wrappers/circuit-4/` reruns `setup` against the v2 R1CS; output `BridgeEventGroth16VerifierGenerated.sol` lands at `contracts/ethereum/src/`.
4. `AckiNackiBridge` gains:
   - `withdraw(proof, tokenId, amount, recipient, dstChainId, nullifier)` — 6 args + 256-byte proof bytes.
   - `mapping(bytes32 => bool) public nullifierUsed;`
   - Reentrancy guard already present.
   - Treasury / AAVE liquidity rebalance call same shape as the existing `_pullFromAave`.
5. Phase B Foundry tests: happy-path, replay rejection, wrong-chain rejection, oversized-recipient rejection, AAVE-shortfall path.

## Cross-references

- Partner circuit source: `acki-nacki-to-eth-bridge-halo2-circuits/bridge-event-prove-circuit/src/bridge_event_prove_circuit.rs`
- Partner event layout: `bridge-event-prove-circuit/src/EVENT_LAYOUT_COMPARISON.md`
- Bridge Phase A code: `contracts/ethereum/src/AckiNackiBridge.sol` (search `verifyEvent` / `_layerWindow`)
- Bridge Phase A tests: `contracts/ethereum/test/AckiNackiBridgeVerifyEvent.t.sol`
- gnark wrapper Phase A skeleton: `crates/bridge-snark-utils/gnark-wrappers/circuit-4/README.md`
- Decision Log: `docs/an_partner_integration_plan.md` entry 2026-05-17 (Phase A — Circuit 4 scaffolding)
