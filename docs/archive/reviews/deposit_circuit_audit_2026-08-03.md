> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/EVM-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Deposit-circuit audit (2026-08-03) — verification and resolution

Audit source: `BUG_SUMMARY_DEPOSIT_CIRCUIT.md` against branch
`pruvendo/deposit-circuit-soundness-fixes` (9 findings + a documentation note).

All nine findings reproduce. Eight are fixed in this commit; **BC-D01 is an
architectural gap that no code change in this repository can close** — decided
2026-08-04 and closed on the AN side, see §1.

| ID | Severity | Status |
|----|----------|--------|
| BC-D01 | blocker to launch | **closed, one operational step outstanding** — anchor-set gate (`7992ce26`) + M-of-N attesters (`1fb5b28c`) + per-chain mint cap (`993af815`) on AN; the trust root is still the owner key until `disableOwnerAnchors()` is called (§1) |
| BC-D02 | P1 | fixed — constraint removed |
| BC-D03 | P2 | fixed — constraint added, negative test |
| BC-D04 | P3 | fixed — both field lengths pinned |
| BC-D05 | P3 | fixed — 32 bytes range-checked |
| BC-D06 | P3 | fixed — measured, then slots widened |
| BC-D07 | P3 | fixed — field and constant deleted |
| BC-D08 | P2 | fixed — test rewritten witness-free, now runs |
| BC-D09 | P2 | fixed — slices guarded, exhaustive short-input test |
| docs | — | fixed — §1 of `verifying_eth_proof_on_an.md` rewritten |

Every circuit-level fix landed in **one** batch so they cost **one** VK rotation.

---

## 1. BC-D01 — no binding to the canonical Ethereum chain (decided 2026-08-04)

### Why the circuit cannot fix this

The prover supplies the header and every trie node itself. There is nothing in
the witness that a fabricated chain could not also satisfy: build a header,
build a receipts trie containing a `Deposit` log whose `address` is the real
bridge, build a transactions trie with a type-2 tx at the same index carrying an
allowlisted `chainId`, and every constraint holds. Confirmed by the audit's
MockProver PoC, with a control run showing that corrupting the receipt root
*does* get rejected — so the Alina-review-#1 binding works, and what is missing
is simply not expressed in the circuit at all.

No in-circuit constraint can close it. "This header is canonical" is a statement
about Ethereum's consensus, not about the witness; proving it in-circuit means
verifying a header chain or a sync-committee signature, i.e. an Ethereum light
client.

### Why the existing check does not count

The check is written — `deposit-relayer-daemon`'s `check_binds_to` — but it sits
off the trust path:

- `USDCBridge.finalizeDeposit` is documented "Permissionless submission", so
  using the relayer is optional;
- `_parsePublicInputs` reads 8 of the 12 `Fr` and comments the `blockHash` slots
  "ignored on the AN side";
- `bridge_verification.md` DEP-N-4 already labels it "producer responsibility,
  **not contract**".

Neither existing guard helps. The `(chainId, contractAddr)` allowlist does not,
because the attacker writes the *real* bridge address into the fake log. The
nullifier does not, because a fabricated deposit carries a fresh `depositId`.

### The asymmetry worth naming

The **AN→ETH direction already solved this exact problem**:
`AckiNackiBridge.withdrawByProof` will not pay out unless `pub.finalRoot` is in
`_knownAnchors`, a set populated only by `verifyBlock`. The proof says "there is
a withdrawal under root R"; the contract decides whether R is a root it accepts.

ETH→AN has no counterpart. Nothing on AN maintains a set of accepted Ethereum
block hashes, and the two public inputs that would be checked against it are
explicitly ignored. **The fix is to mirror the anchor-set pattern in the other
direction**, and the options below differ only in who is trusted to populate it.

### Options

| | Approach | Trust | Effort | Multi-chain story |
|---|---|---|---|---|
| **A** | ETH light client on AN (sync committee / header chain) | none beyond Ethereum consensus | large | needs per-L2 settlement reasoning; an L2 header is not self-certifying |
| **B** | Permissioned submitter runs V1 off-chain (multi-RPC quorum) | one relayer key | none — this is the status quo *if* the gate stays | uniform across chains |
| **C** | Timelock + challenge window, bonded watchers | ≥1 honest watcher online | medium, needs a dispute path | uniform, but adds latency to every deposit |
| **D** | M-of-N attesters sign `(chainId, blockHash)`; contract checks a threshold and stores the anchor | M-of-N honest | small–medium | uniform across chains |

### Decision (2026-08-04)

**Land D's mechanism now with the owner as its writer; move to M-of-N attesters
next; keep A as the long-run target for L1.** C was rejected because its latency
lands on every user for a risk D removes outright.

One correction to the framing above: B was never available as an interim.
"Keeping the gate closed" presumes a gate, and there was none —
`finalizeDeposit` is `public` with no modifier in both the deployed contract and
the 12-PI patch, and `_parsePublicInputs` stopped reading at `fr[8]`. Since the
prover and its SRS are public, the ETH→AN direction was forgeable by anyone,
including on shellnet where the path has been live since 2026-07-01. The interim
had to *add* something, not withhold it.

### What landed — `acki-nacki` `7992ce26`

`USDCBridge` gained the mirror of `AckiNackiBridge._knownAnchors`:

```solidity
mapping(uint256 => mapping(uint256 => bool)) _acceptedBlockHash;  // chainId → blockHash → ok

// in finalizeDeposit, after accept + verify + the allowlist checks:
require(_acceptedBlockHash[f.chainId][_parseBlockHash(publicInputs)], ERR_UNKNOWN_BLOCK);
```

plus `setAcceptedBlockHash(chainId, blockHash, accepted)` (owner), the
`isAcceptedBlockHash` view, and `_parseBlockHash`, which reassembles PI[9]/PI[10]
as `hi << 128 | lo` exactly as `an_account` is reassembled. Notes:

- The block hash is parsed **after** `tvm.accept()`, so the pre-accept gas
  profile of the already-working path does not move.
- **Fail-closed, and not carried through `onCodeUpgrade`** (same as
  `_expectedBridgeFr`): after deploy or upgrade, no deposit finalizes until its
  block is admitted. This will look like a broken bridge to anyone who misses
  the step — including the partner's `tests/exchange/test_usdcbridge_*.py`.
- Compile-verified with `sold 0.79.2` (differential against the parent commit;
  the only error either side is the older compiler not knowing the
  `gosh.zkhalo2VerifyWithVK` builtin, so the check was repeated with that call
  stubbed to get a clean `.tvc` and confirm the new ABI entries).

The gate deliberately takes a **hash, not a header**, so moving to M-of-N meant
adding a second writer and changing nothing in `finalizeDeposit`. That landed the
same day — see "M-of-N attesters" below.

### M-of-N attesters — `acki-nacki` `1fb5b28c`

`attestBlockHash(chainId, blockHash)`, called as an external message signed by an
attester key, so the vote is attributed to `msg.pubkey()`. A key outside the set
cannot vote; a key inside it cannot vote twice; once `_attesterThreshold` distinct
keys agree, the anchor is admitted. `finalizeDeposit` is untouched, as intended.

The call that actually changes the trust assumption is **`disableOwnerAnchors()`**.
Until it is made, the attester set is decoration — the owner can still admit any
hash alone, so the effective root is still one key. It is one-way, with no
re-enable, because a switch back would leave the owner key on the path regardless
of where it sits in the sequence; it requires a satisfiable attester set first so
it cannot brick the only working writer. `getAttesterConfig()` returns
`(threshold, attesterCount, ownerAnchorsEnabled)` — read the third field before
believing the first.

Thresholds are kept satisfiable at both ends: `setAttesterThreshold` rejects 0
(admits on one vote) and anything above the key count, and removing a key
re-checks the bound, since otherwise a removal silently freezes every future
anchor.

What the threshold does **not** buy: independence. N keys held by one operator, or
N attesters all polling the same RPC provider, is one key wearing N hats. Each
attester still owes the two obligations below in its own right.

### What the anchor writer is trusted for

The trust root for source-chain canonicality is whoever can write the anchor set —
the owner key while `ownerAnchorsEnabled`, a threshold of attesters after
`disableOwnerAnchors()`. Either way this is a trust assumption to state rather
than a design: the writers can mint by admitting a hash from a chain that does not
exist. Two obligations come with it that no contract can enforce:

- **Independence** — read the hash from a source the party that produced the
  proof does not control. Verifying against the same RPC that built the witness
  proves nothing.
- **Reorg depth** — an honest attestation of a block that later reorgs out loses
  funds exactly like a dishonest one.

`scripts/deposit_anchor_params.py` exists so discharging both is one command
rather than a judgement call: it decodes `(chainId, blockHash)` out of a
`public_inputs.bin` and, with `--verify`, refuses to print the call arguments
unless an independent node agrees the block is canonical (its number maps back to
the same hash) and buried at least `--min-confirmations` deep (default 64).
`--call attestBlockHash` prints an attester's vote instead of the owner setter;
each attester should run it against its own endpoint rather than copy a peer's
output, which is the only way the threshold means anything.

```
$ scripts/deposit_anchor_params.py deposit-prover/fixtures/deposit_10proofs/proof_00 --verify
chainId=11155111 blockHash=0x34fbf8176bf9d318a360f2106568446a81ba2a460e9f4a32b9b263baa25d5436
    number=11025192 confirmations=392065 latest=11417257
    setAcceptedBlockHash {"chainId": "11155111", "blockHash": "0x34fb…5436", "accepted": true}
```

It doubles as a check on the R2 fix from the PR-20 review: all ten regression
fixtures now carry real canonical Sepolia hashes, so `--verify` passes on them.
Flipping one byte of PI[10] is rejected as "node does not know this block hash".

### Per-chain mint cap — `acki-nacki` `993af815`

`setMintCap(chainId, cap)`, 0 = unlimited, checked in `finalizeDeposit` and
tallied in `confirmDeposit`. Orthogonal to everything above: those guards try to
make forgery impossible, this one bounds what a forgery is worth if one of them
turns out to be wrong anyway.

The split across the two calls is deliberate. Checking before the voucher is
deployed keeps an over-cap deposit **retryable** once the cap is raised, where
checking at mint time would consume the voucher and strand that deposit forever;
tallying only where a mint lands keeps replays from eating headroom. The
consequence, documented at the call site: N in-flight deposits can overshoot by
their combined amount. It is a bound on damage, not an exact invariant.

### Still open

- **`disableOwnerAnchors()` has not been called on any deployment.** Until it is,
  the trust root is the owner key and the attester set is decoration. It needs an
  attester set with independent operators and independent RPC providers first —
  otherwise it trades one key for a quorum that fails together.
- **L2 canonicality.** An L2 block hash is only settled once its output root is
  posted to L1, so option A does not generalise: L1 gets a light client, the L2s
  in the allowlist stay on attesters unless someone builds per-L2 settlement
  verification.

---

## 1b. Found while fixing BC-D01: the deposit identity did not separate chains

Not in the audit, and worth more than most of what is: the anti-replay identity
was `(depositId, contractAddr, dappId)`, which does not include the source chain.

- `dappId` is one constant per deployment — identical for every chain.
- `depositId` is a per-chain counter that every chain starts at 0.
- One bridge address routinely serves several chains, whether by CREATE2 or just
  the same deployer nonce.

So the first deposit on a second allowlisted chain that shares a bridge address
with the first collides: the voucher address is already occupied, its constructor
is a no-op, `confirmDeposit` never fires, nothing mints. With the ETH-side refund
path retired in Phase 4.3 the user's funds have no way out. It needs no attacker —
it fires on ordinary multi-chain operation, which is exactly what the 12-PI
`chainId` work was for.

Fixed in `acki-nacki` `21a781e7`: `chainId` (already public input #4, already in
scope at the hash site) joins the identity, and rides through the voucher callback
and `DepositFinalized` so per-chain accounting and monitoring can tell chains
apart. The hash now lives in one helper, `_depositIdentity`.

**This changes `DepositVoucher`'s ABI**, which is the failure mode from
2026-07-02: bridge and voucher must agree byte-for-byte, and a mismatch is not a
compile error — the voucher aborts on cell underflow (`exit_code 9`) before
reaching `confirmDeposit`, so deposits just silently stop minting. Both artefacts
must be recompiled and redeployed as a pair, with the bridge re-embedding the
fresh `_depositVoucherCode`.

`scripts/check_voucher_abi_consistency.py` now makes that class of bug detectable
rather than a war story: it compares all three copies of the signature (the
`new DepositVoucher` call, the constructor, `confirmDeposit`) in the sources and in
the compiled ABIs. Run against the tree today it fails, and usefully so — the
artefacts tracked in `acki-nacki/contracts/0.79.3_compiled/exchange/` are
**pre-#2271**: they still carry `int8 anWorkchain`, so they predate the 256-bit
recipient fix, the source allowlist and the anchor gate. Anything deployed from
that directory as-is would ship known-broken logic.

> The audit read the AN side from a checkout of `acki-nacki` @ `history_cursor`
> (11 PI, pre-allowlist). Re-confirm against whatever ships to deploy.

---

## 2. BC-D02 — smart-wallet deposits were unprovable (fixed)

`constrain_equal(to_as_fr, contract_address_field)` required the enclosing
transaction's `to` to be the bridge. False for a Safe or any multisig (`to` =
wallet), ERC-4337 (`to` = EntryPoint), EIP-7702, and every router. `deposit()`
accepts those calls, so funds entered and could not be proven out — and the
refund path was retired in Phase 4.3, leaving only `emergencyWithdrawAll`.

**Removed, and it cost nothing.** I added the constraint as "defence-in-depth on
the emitter" (it is item (d) in Alina's `chain_id_deposit_circuit_v2_analysis.md`
inventory), and the defence was already provided twice over:

- the emitter is pinned by Phase 1: the log's RLP-decoded `address` is
  constrained equal to the `contractAddress` public instance;
- `chainId`'s binding never involved `to`: the transaction is the *same*
  transaction as the receipt because both MPT proofs open at the same `tx_idx`
  cell against roots from the same header, so its RLP field 0 is the chain the
  receipt lives on either way.

The `len == 20` sub-constraint went with it. It only mattered for reading
`field_bytes[0..20]` soundly, and nothing reads `to` any more.

This finding is strictly wider than PR #20's R4: R4 rejects obsolete transaction
*types*, while this rejected a growing class of *senders*. R4 stands as before —
type-2-only, diagnosed early in `generate_transaction_proof`.

## 3. BC-D03 — `block_header_len` was not tied to `rlp_len` (fixed)

The length fed to `keccak_var_len` and the length the RLP decoder derives from
the list prefix were independent numbers. Appending a byte inside the
zero-padded witness and moving the length past it shifts `blockHash` while the
prefix-delimited list still yields the same `receiptsRoot`, so one event could
carry unboundedly many valid `blockHash` public inputs.

Fixed with `ctx.constrain_equal(&block_header_len, &block_header_array.rlp_len)`.

Not exploitable on its own — none of those hashes is a real block — but it voids
the item-1 claim in `verifying_eth_proof_on_an.md` §1, and under any BC-D01
option that checks `blockHash` against an anchor set, "prover picks the
`blockHash`" is exactly the freedom that must not exist.

Verified both directions with a new harness:

```bash
cd deposit-prover
cargo run --release --example mock_fixture -- fixtures/deposit_10proofs/proof_00/input.json
#   ✓ all constraints satisfied
cargo run --release --example mock_fixture -- fixtures/deposit_10proofs/proof_00/input.json \
  --mutate header-pad
#   ✓ rejected as expected: circuit not satisfied (2 failures)
```

`prover::test_circuit_mock` now calls `MockProver::verify()` instead of
`assert_satisfied()`; the latter panicked, which made "circuit rejected the
witness" indistinguishable from a crash and defeated the `Result` it returns.

## 4. BC-D04 — log field lengths were not constrained (fixed)

Phase 1 reads `topics_rlp[1..33]`, `[34..66]`, `[67..99]` and `data_bytes[0..32]`,
`[64..80]`, `[80..96]` — fixed offsets into arrays zero-padded to
`log_max_field_lens`. A log with one topic decoded fine and `depositId` /
`sender` came out of the padding. The `topics[0]` signature check does not help:
it only says *something* at offset 1..33 equals the signature.

Fixed by pinning both payload lengths: topics `field_len == 99`
(3 × (0xa0 + 32)), data `field_len == 128` (4 ABI words).

## 5. BC-D05 — `dappIdHigh` / `dappIdLow` were unconstrained (fixed)

Unlike every other 32-byte input, `dappId` has no Phase-1 counterpart, and
`bytes_to_field` does not range-check — so the 32 "bytes" were arbitrary field
elements and the two halves were not 128-bit halves at all, while the AN side
reassembles them as `(fr[5] << 128) | fr[6]`.

Fixed with an 8-bit `range_check` on each of the 32 bytes. As the audit notes,
an attacker gains nothing (`dappId` is freely chosen and compared against the
configured value), but a public input should mean what the layout says.

## 6. BC-D06 — `gasUsed` 4 bytes vs `gasLimit` 8 bytes (fixed, with data)

I measured what the audit asked for. Sampling 32 Arbitrum One blocks spread
across the chain's history (2026-08-03, `arb1.arbitrum.io/rpc`):

```
max gasUsed  =         2 719 399  (3 bytes)   ← 1579× inside the 4-byte slot
max gasLimit = 1 125 899 906 842 624 (7 bytes, = 2^50)
```

So the slot is not in practical danger. **I widened it anyway**, along with
`number` and `timestamp`, from 4 to 8 bytes each: slots 8, 10, 11 now match slot
9. Reasons:

- The failure mode is **liveness, not soundness**: too narrow a slot makes the
  RLP decode fail, so *no* deposit in such a block is ever provable — and, with
  `withdraw()` retired, its funds are stuck. Bounding by "observed" is the wrong
  test when the protocol may legally emit more.
- The cost is 12 witness bytes: `MAX_BLOCK_HEADER_BYTES` 705 → 717, and both
  need the same 6 keccak rounds, so the keccak cost is literally unchanged.
- A VK rotation was already required for BC-D02..D05. Discovering later that a
  slot was too narrow costs another rotation *plus* a shellnet redeploy.

`timestamp` at 4 bytes would also have overflowed in 2106, and `number` covered
2^32 blocks against Arbitrum's current 4.9 × 10^8.

Two tests keep this honest: `integer_header_slots_cover_their_own_gas_limit`
(every integer slot ≥ the `gasLimit` slot) and `sample_headers_fit_every_integer_slot`
(no slot narrower than live fixture data).

## 7. BC-D07 — dead constant with an invalid value (fixed)

`EXPECTED_L1_CHAIN_ID = 1` fed `CircuitConfig::expected_chain_id`, which the
circuit discarded (`let _ = config.expected_chain_id`), and `1` is not in the
allowlist — `supported_chains.rs` asserts exactly that. Both the constant and
the field are deleted, along with the four assignments in the examples. The
surviving `--chain-id` flag is an RPC selector, cross-checked against the
witness by `DepositProofInput::require_chain_id` (PR #20 R7).

## 8. BC-D08 — the public-input layout test never ran (fixed)

Dead twice: `#[ignore]`, and an early `return` on
`../e2e_attack_test_data/valid_proof.json`, which is `.gitignore`d and so absent
from any clone. Its doc comment described the **7**-input layout while its assert
demanded 12 — never updated across 7→10, 10→11 or 11→12. Meanwhile the count was
written out by hand in three places (`circuit_v2::num_instance`,
`types::NUM_PUBLIC_INPUTS`, and the relayer's own copy).

Rewritten as two tests with no external fixture, both running by default:

- `deposit_circuit_declares_twelve_public_inputs` — pins the new
  `DEPOSIT_PUBLIC_INPUT_LAYOUT` table (names *and* slots, so `chainId` cannot
  drift off slot 4 and `promiseCommit` cannot stop being last);
- `num_instance_matches_the_declared_layout` — pins what `keygen_vk` will
  actually bake in against that table.

`types::NUM_PUBLIC_INPUTS` is now derived from the table rather than restated.
The relayer keeps its own `NUM_PUBLIC_INPUTS = 12` in a separate cargo tree —
same duplication class as PR #20's R6 (`supported_chains`), and worth the same
treatment (a shared leaf crate) as a follow-up.

## 9. BC-D09 — `typed_tx_chain_id` panicked instead of erroring (fixed)

Two unguarded slices. Both audit reproducers confirmed: `[0x02, 0xff]` claims an
8-byte list-length header that is not there, `[0x02, 0xc0, 0x85]` claims a 5-byte
`chain_id` string that is not there. Reachable from
`DepositProofInput::require_chain_id`, i.e. from loading any witness off disk.

Applied the audit's patch (`get(..)` + `ok_or_else` on both sites). Two tests:
the four known reproducers, and an exhaustive sweep of every 2- and 3-byte typed
payload (65 792 inputs) asserting the decoder terminates rather than unwinds.

## 10. Documentation (fixed)

`verifying_eth_proof_on_an.md` §1 is rewritten: 12 inputs with a per-slot "bound
by" column, the corrected event signature (it still listed
`Deposit(uint256,address,uint256,uint256)`), the BC-D02/D03/D04 consequences, and
an explicit callout that `verify_block_header_rlp` is a **native** witness-time
check — it closes PR #20's R2 for honest witness generation and constrains a
malicious prover not at all.

## 11. Found while fixing: `--chain-id` made every non-Sepolia chain unprovable

Not in the audit — surfaced while removing the dead `expected_chain_id` (BC-D07).

PR #20's R7 made `--chain-id` meaningful by cross-checking it against the
witness. But the flag carried `default_value = "11155111"` and the check ran
unconditionally, **and the relayer never passes the flag**. So on the production
path `export_vk_blob` and `export_blake2b_proof` compared every witness against
Sepolia: a valid Base, Arbitrum, OP, Mantle, World Chain or Blast deposit failed
with "witness proves chain_id 8453, but --chain-id says 11155111". The
multi-L2 support that PR #20 claims was unreachable through the daemon for six
of the seven allowlisted chains — the same shape as R5, where `daemon` skipped
the allowlist check that `watch` and `prove-one` performed.

Fixed on both sides:

- `DepositProofInput::resolve_chain_id(Option<u64>)` replaces the pair of calls.
  With a flag it cross-checks (R7's intent); without one it reads the chain from
  the witness. Either way the result goes through the allowlist. A pre-Track-2
  witness with no `tx_bytes` now says to re-fetch with `fetch_deposit_data`
  instead of silently passing.
- `--chain-id` became `Option<u64>` in `export_vk_blob`, `export_blake2b_proof`
  and `test_with_real_data`, so there is no wrong network to guess.
  `export_deposit_proof_set` keeps it required — for a committed fixture set,
  being explicit is the point.
- `SubprocessProofGenerator` now passes `--chain-id <event.source_chain_id>`, so
  the relayer gets R7's cross-check rather than opting out of it.

Four tests in `types.rs` cover flagless resolution, explicit cross-check both
ways, allowlist rejection, and the pre-Track-2 message.

---

## Reproducing

```bash
cd deposit-prover
cargo test --lib                       # 26 tests, includes the 4 new ones
cargo run --release --example mock_fixture -- fixtures/deposit_10proofs/proof_00/input.json
cargo run --release --example mock_fixture -- fixtures/deposit_10proofs/proof_00/input.json \
  --mutate header-pad                  # must reject
```
