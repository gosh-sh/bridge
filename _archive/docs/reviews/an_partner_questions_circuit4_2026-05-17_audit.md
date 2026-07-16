# Audit: `docs/an_partner_questions_circuit4_2026-05-17.md`

**Auditor**: in-repo review pass (2026-05-17)
**Scope**: factual accuracy, crypto/encoding correctness, and clarity of the
five Phase B blocker questions before the doc is sent to Alina.
**Method**: cross-checked every claim against the live codebase
(`AckiNackiBridge.sol`, `BridgeEventVerifier.sol`,
`crates/bridge-prover-orchestrator/`), the partner sibling repo
(`../acki-nacki-to-eth-bridge-halo2-circuits/bridge-event-prove-circuit/`),
the integration plan's risk register, and `docs/circuit_4_open_questions.md`.

---

## Verdict

The doc is structurally sound and the five questions are the right ones. Three
issues are blocking ("Alina will be confused or sent to the wrong place") and
must be fixed before sending. Five more are wording/precision polish that
materially improves the answers we'll get back. One claim that *looked* wrong
on first read (the 103-input layout) actually checks out — the partner's own
`EVENT_LAYOUT_COMPARISON.md §5.9` is the stale source, not our doc.

---

## What's solid

- **Phase A status framing** (lines 4–8): matches reality.
  `AckiNackiBridge.verifyEvent(proof, tokenId)` exists at
  `AckiNackiBridge.sol:629-653`, snapshots `_layerWindow[100]` from on-chain
  (not the caller — important for soundness; an attacker can't substitute a
  known-good window from a different bridge instance), and forwards 103 public
  inputs to `BridgeEventVerifier`.
- **16 Foundry tests**: confirmed — `AckiNackiBridgeVerifyEvent.t.sol` has
  exactly 16 `function test*` definitions.
- **Phase A non-paying claim** (lines 10–14): correct. `verifyEvent` only
  emits `BridgeEventVerified(tokenId, msg.sender)` and does not move ETH.
- **103-input layout assumption** (lines 27–35 baseline): the partner circuit
  (`bridge_event_prove_circuit.rs:813-817`) does push exactly
  `[token_id, dapp_fr, acc_fr, layer_hashes…]` as instance column 0. So our
  adapter is consistent with the partner's *current* implementation, even
  though `EVENT_LAYOUT_COMPARISON.md §5.9` still documents an older
  `[final_root, tokenId, ephemeral_pubkey]` design. Worth a heads-up to Alina
  that her circuit code and her layout doc disagree, but our adapter is
  aligned with the code, which is what matters.
- **Q-C4-4 deferral framing** (lines 89–97): correct.
  `EVENT_LAYOUT_COMPARISON.md §5.6` already calls variable-length recipient
  `TODO: (B)` and out-of-scope.

---

## Blocking issues (fix before sending)

### B1. Wrong file path for `Halo2ProofData` (lines 117–119)

> «см. `crates/bridge-prover-orchestrator/src/layer_hashes_prover.rs`,
> структура `Halo2ProofData` с полями `public_inputs`, `proof_bytes`,
> `protocol`»

`Halo2ProofData` lives at
`crates/bridge-prover-orchestrator/src/proof_export.rs:17-22`.
`layer_hashes_prover.rs` only declares
`LayerHashesProofOutput { proof_bytes, instances }` — Alina won't find the
JSON struct there.

**Fix**: change the cite to `proof_export.rs`.

```17:22:crates/bridge-prover-orchestrator/src/proof_export.rs
pub struct Halo2ProofData {
    pub public_inputs: Vec<String>,
    pub proof_bytes: Vec<u8>,
    pub protocol: ProtocolData,
}
```

### B2. R14 vs R15 conflated (Q-C4-5, lines 99–110)

> «single-party `groth16.Setup` — это уже зафиксировано как R14/R15 в нашем
> integration plan»

R14 and R15 are different problems and the doc only describes R14:

- **R14** = single-party `groth16.Setup` → toxic-waste leak risk
  (the trusted-setup ceremony issue this section is asking Alina about).
- **R15** = wrapper R1CS itself is a no-op identity stub. I checked
  `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/circuit.go:40-46`
  — `Define()` literally is:

  ```go
  for i := 0; i < NumPublicInputs; i++ {
      api.AssertIsEqual(circuit.PublicInputs[i], circuit.PublicInputs[i])
  }
  api.AssertIsDifferent(circuit.DomainSize, 0)
  ```

  Zero real Halo2 verification.

Per `docs/an_partner_integration_plan.md` §5 R14, R14 is **downstream** of
R15: *"Running the ceremony now would produce a perfectly-secured stub"*.

**Fix**: split the single sentence into two and lead with R15:

> Phase A wrapper для Circuit 4 — identity-stub (R15), как и для 1A/1B/2.
> Real Halo2-in-gnark verification — это Phase 8 R&D у нас. Phase 9
> (multi-party trusted setup ceremony / R14) становится осмысленной только
> *после* Phase 8 — иначе мы получаем «идеально защищённую церемонией заглушку».

That's the strategically accurate "where we stand" picture. Without this
correction Alina may think we already have a real-but-untrusted-setup
verifier, which would change her Q-C4-5 answer.

### B3. Q-C4-3 option (a) — `dstChainId` cannot be stuffed into `(dappFr, accFr)` (lines 78–82)

> «фиксируем `dstChainId` как immutable на конкретном экземпляре
> `AckiNackiBridge` и кладём его в `dappFr`/`accFr` triple, чтобы серкут сам
> этим ограничился»

`dappFr` / `accFr` are the **AN-side** `TokenBridge` contract identity —
they're determined by Acki Nacki's deployment of the bridge, and the circuit
binds them via `Poseidon96(account_dapp_id, account_id, ext_message_hash)`
(see `bridge_event_prove_circuit.rs:670-688`). We can't stuff `dstChainId`
into them; that would require Alina to redeploy her bridge on every Ethereum
fork.

**Fix**: rewrite option (a) as

> ship one circuit + one VK per target chain, partner hard-asserts
> `dstChainId == EXPECTED` inside the circuit body (with `EXPECTED` baked
> in as a circuit constant per VK).

The scaling argument against (a) is unchanged; only the mechanism description
is wrong. Recommendation (б) still wins.

---

## Polish (substantive but non-blocking)

### P1. Recipient encoding is over-engineered (Q-C4-1, lines 29–40)

The hi/lo split is unnecessary for Phase B. BN254 `Fr` is 254 bits ≈ 31.75
bytes. A 20-byte (160-bit) EVM address fits in **one** `Fr` — exactly the way
the partner already encodes `account_dapp_id` / `account_id` via
`bytes_to_fr` (32-byte LE input, top bits zero). One slot is enough:

```
[3]   recipientFr   (Fr-encoded 20-byte EVM address; same convention as dapp_fr/acc_fr)
```

The 288-bit "не лезет" argument in lines 37–39 is correct, but it's about
packing `(amount, recipient)` *together*. The correct conclusion is "recipient
alone in one Fr, amount in another — total 2 slots", not "recipient needs 2
slots".

Adapter changes from 107 → 106 inputs. Trivial difference, but it's the
simpler design — and it removes a source of cross-chain encoding ambiguity
(see P2) on the bridge side.

### P2. Recipient parenthetical is contradictory (Q-C4-1, line 29)

> «recipientHi (uint128, верхние 4 байта = 0)»

If we keep the hi/lo split despite P1, the gloss is wrong. A 20-byte address
split into two `uint128` slots has *12* zero-pad bytes in the high slot
(16 − 4 effective), not "верхние 4 байта = 0" (which would imply
`recipientHi` carries 12 effective bytes → 16 effective bytes total →
overshoots a 160-bit address). The companion `docs/circuit_4_open_questions.md`
Q-CIRC4-1 has the same ambiguity ("uint256 high 16 bytes; recipient ∈ 20-byte
EVM addr").

**Fix**: drop the split (P1, preferred), or reword unambiguously to e.g.
`uint32` for the high 4 bytes + `uint128` for the low 16.

### P3. Nullifier formula is over-broad (Q-C4-2, lines 55–59)

The proposed
`nullifier = Poseidon(envelope_hash || block_id || tokenId || amount || recipient || senderDapp || senderAcc)`
has redundancy: the partner's own SHA-256 cell-tree chain
(`EVENT_LAYOUT_COMPARISON.md §5.2-5.3`) already binds `tokenId` / `amount` /
`recipient` / `sender` into `envelope_hash`. So the 7-field hash and
`Poseidon(envelope_hash || block_id)` admit the same set of inputs, just at
very different constraint cost.

Even better: `bridge_event_prove_circuit.rs:719-744` already computes
`block_leaf = Poseidon96(block_id, envelope_hash, ext_out_root)` as an
*internal witness* for the events Merkle proof. Exposing `block_leaf` (or
plain `Poseidon(block_id, envelope_hash)`) as a public Fr is essentially free
— no new Poseidon caps, no new constraints.

**Fix**: lead Q-C4-2 with this proposal; demote the 7-field formula to "если
по каким-то причинам `block_leaf` не подходит, fallback such-and-such":

> Минимально-инвазивный вариант — выставить `block_leaf` (или
> `Poseidon(block_id, envelope_hash)`) public Fr-ом; ты его уже считаешь
> внутри для Merkle-доказательства, новых constraints не добавится.

The "альтернативно: `nullifier := envelope_hash`" branch (line 62) has a
subtle gap: two identical `WithdrawalInitiated` events from the same account
in the same block produce the same `repr_hash`. Binding `block_id` resolves
it. Worth saying so explicitly so Alina doesn't pick the simpler-but-unsafe
option.

### P4. Layout-renumbering breakage isn't called out (Q-C4-1)

The new layout in lines 27–34 bumps `dappFr` from `[1]` to `[5]` and `accFr`
from `[2]` to `[6]`, and grows the array from 103 → 107 inputs (or → 106 with
P1). The doc says "порядок твой — мы подстроим adapter" which is fine, but
Alina may not realise this is a hard ABI break for `BridgeEventVerifier.sol`
(lines 53–59 of the adapter hard-code slot 0/1/2). One sentence helps:

> Это break-ABI на нашей стороне (`BridgeEventVerifier.sol` слоты 0..2 сейчас
> hard-coded под `[tokenId, dappFr, accFr]`); как только опубликуешь v2 — мы
> rerun gnark setup и переписываем adapter.

Optional softer migration to mention as alternative: append new fields at the
tail (`[103]=amount, [104]=recipient, [105]=dstChainId`) so the existing
103-input prefix is unchanged. This costs nothing on the circuit side and
saves one re-spin on ours; worth offering.

### P5. `dstChainId == 1` should be `block.chainid` (Q-C4-3, line 84)

> «ETH-контракт ассертит `dstChainId == 1` (или `block.chainid`)»

The hedge is in the wrong order. For testnets, future L2 redeploys, and
Anvil-based CI the constant `1` is wrong. Lead with `block.chainid` and drop
`1` (it's a lossy oversimplification; `block.chainid` is always exact).

---

## Minor

### M1. `Poseidon96` term is informal but well-grounded

The doc asks "есть ли у тебя в утилитах готовый `Poseidon96`-чип". Confirmed
in partner repo: `bridge_event_prove_circuit.rs:36` and `:670` use
`Poseidon96` to mean Poseidon over 96-byte input (3 × 32-byte Fr) with
`T=3, RATE=2, R_F=8, R_P=57` (see
`bridge-event-prove-circuit/src/poseidon.rs:7-10`). The implicit answer to
the question is "yes, you saw me use it for `block_leaf` and `ext_msg_leaf`"
— but it's still fair to ask, and combined with P3 above it becomes "we'd
like the existing `block_leaf` exposed", which is more concrete.

### M2. Q-C4-2 on-chain cost not stated

Q-C4-2 asks "какой вариант для тебя дешевле" but doesn't tell Alina what
we're paying on our side. `docs/circuit_4_open_questions.md:61` already
estimates "1 SSTORE per withdraw" (~22 100 gas first time, ~5 000 warm).
Quoting that gives symmetry on the cost discussion and signals we're
committed to whatever she picks.

---

## Suggested ordering for revisions

If only three fixes are made, fix **B1** (wrong file path → bounce), **B2**
(R14/R15 conflation → wrong strategic picture), and **P3** (nullifier reuse
of `block_leaf` → free constraints, better formula). Everything else is
wording polish.

## Cross-references

- Subject doc: `docs/an_partner_questions_circuit4_2026-05-17.md`
- Phase A scaffolding: `contracts/ethereum/src/AckiNackiBridge.sol:629-653`,
  `contracts/ethereum/src/BridgeEventVerifier.sol`,
  `contracts/ethereum/test/AckiNackiBridgeVerifyEvent.t.sol`
- Phase A gnark wrapper (R15 stub):
  `crates/bridge-prover-orchestrator/gnark-wrappers/circuit-4/circuit.go`
- `Halo2ProofData` (correct path):
  `crates/bridge-prover-orchestrator/src/proof_export.rs:17-22`
- Partner circuit:
  `../acki-nacki-to-eth-bridge-halo2-circuits/bridge-event-prove-circuit/src/bridge_event_prove_circuit.rs`
- Partner layout doc (note: §5.9 is stale vs the implementation):
  `../acki-nacki-to-eth-bridge-halo2-circuits/bridge-event-prove-circuit/src/EVENT_LAYOUT_COMPARISON.md`
- Risk register R14/R15: `docs/an_partner_integration_plan.md` §5
- Companion English version of the questions:
  `docs/circuit_4_open_questions.md`
