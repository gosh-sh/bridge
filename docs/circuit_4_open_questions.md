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
| gnark wrapper skeleton (`crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/`) | ✅ builds (identity stub mirror of `circuit-2`) |

Phase A is intentionally **non-paying**: a successful `verifyEvent` only emits `BridgeEventVerified(tokenId, msg.sender)`. No ETH moves.

## Phase B blockers — five open questions for partner

### Q-CIRC4-1 — Make `amount` and `recipient` public inputs

> The circuit's `EVENT_LAYOUT_COMPARISON.md` lists `amount` and `recipient` as private witnesses. Without these as public inputs the bridge cannot pay anything to anyone.

Proposed extension of the public-input layout:

```
[0]            tokenId        (uint32)
[1]            amount         (uint128)        ← new
[2]            recipient_hi   (uint256 high 16 bytes; recipient ∈ 20-byte EVM addr)
[3]            recipient_lo   (uint256 low  16 bytes)
[4]            dstChainId     (uint256)        ← new (or keep private if we only ever target chainId == 1)
[5]            dappFr
[6]            accFr
[7..=106]      layerHashes    (100 candidates)
```

- Alternative: pack `(amount, recipient)` into one Fr element if their bit-widths fit (amount 128 bits + recipient 160 bits = 288 bits > Fr 254 bits — doesn't fit. Two slots needed).
- Decision needed from partner: which slot order matches the partner's preferred witness encoding.

### Q-CIRC4-2 — Nullifier

> Circuit 4 is currently replayable. Anyone can resubmit a valid `verifyEvent` (proven by `test_verifyEvent_isReplayable_byDesign_inPhaseA`). For a real `withdraw()` this is fatal — one valid AN-side event would let an attacker drain the bridge by replaying the proof.

The standard fix is a **circuit-side nullifier**: a public Fr input derived from the event's binding witnesses such that two distinct withdrawals produce two distinct nullifiers, while two replays of the same withdrawal produce the same one.

Proposed nullifier formula (partner's call):

```
nullifier := Poseidon( envelope_hash || block_id || tokenId || amount || recipient || sender_dapp || sender_acc )
```

— or alternatively just `nullifier := envelope_hash` if `envelope_hash` is already globally unique on AN (envelope_hash already binds to a specific block; replaying the same envelope_hash *is* the replay we're guarding against).

Bridge-side: an `nullifierUsed[bytes32] public` mapping that `withdraw()` consults + writes after a successful proof. Estimated cost: 1 SSTORE per withdraw.

### Q-CIRC4-3 — `dstChainId` semantics

> The circuit currently witnesses `dstChainId` (uint256) but doesn't enforce anything about it. For a multi-chain bridge we'd want the verifier to assert `dstChainId == THIS_CHAIN_ID` so a proof targeting "withdraw to BSC" cannot be replayed on Ethereum.

Two options:
- **Per-chain bridge instance**: hard-wire `dstChainId` as an immutable on the Ethereum bridge and assert in `verifyEvent`. Simple but doubles deployment count when new chains arrive.
- **`dstChainId` public**: lift to public input; the verifier asserts `dstChainId == address(this).chainId`. Single circuit, multiple deployments.

Recommended: lift to public (option 2). It composes better with the existing `dappFr`/`accFr` immutability pattern.

### Q-CIRC4-4 — Variable-length `recipient`

> `EVENT_LAYOUT_COMPARISON.md` §5.6 mentions a variable-length recipient path, but the current circuit hardcodes `RECIPIENT_LEN_FIXED = 20` (Ethereum 20-byte addresses). Solidity bridge can require `recipient.length == 20` and reject anything else.

Decision: defer variable-length to Phase C. Phase B uses fixed 20-byte recipient ⇒ a single public-input layout works for all Ethereum withdrawals.

### Q-CIRC4-5 — Trusted setup ceremony

> The Phase A gnark wrapper is an identity stub (same trust model as Circuits 1A/1B/2). Once Phase B circuit-side changes land, **every wrapper needs a fresh trusted setup**. We need to decide whether to bundle Circuit 4's ceremony with the existing four-circuit ceremony (Phase 8 of `integration_plan.md`) or run a separate one.

Bridge-side cost is identical either way; the question is logistical.

## What needs to land before flipping `withdraw()` on

1. Partner publishes Circuit 4 v2 with public `amount`/`recipient`/`dstChainId` and a nullifier (Q-CIRC4-{1,2,3}).
2. `bridge-prover-orchestrator` grows a `bridge_event_prover` module (mirror of `layer_hashes_prover`).
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
- gnark wrapper Phase A skeleton: `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/README.md`
- Decision Log: `docs/an_partner_integration_plan.md` entry 2026-05-17 (Phase A — Circuit 4 scaffolding)
