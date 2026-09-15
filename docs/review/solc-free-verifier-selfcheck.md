# Dropping `solc` from the withdrawal path — proposal

**Status:** implemented. The withdrawal verifier's reference `.sol` was regenerated from its key
and compiles to the committed `.bin`. A compiled `.bin` ends with `solc`'s CBOR metadata, whose
hash commits to the source's keccak256, so compiling to the identical `.bin` proves a `.sol` is
exactly the source of that `.bin` — which also holds for the Primary, Fallback and LayerHashes
sources, even though those three were not regenerated from their keys. What that leaves
unconfirmed is only whether the generator at the current `snark-verifier` pin still reproduces
that same source from their keys, which the relayer's former bytecode self-check already
established on every aggregation it ran before this change; watch the first relayer cycle of
each kind after upgrading. The `.sol → .bin` link is checked by
`.woodpecker/verifier_sources.yaml`. The text below is the proposal as written.

Verified against commit `dc3d91e`. Paths are relative to the repository root, except the
`snark-verifier` sources, which are cited at the pinned tag `v0.1.7-git` (`4b733e0`,
`crates/bridge-evm-aggregator/Cargo.lock`).

---

## Goal

Stop requiring `solc 0.8.19` on the machine that runs `ackinacki-bridge withdraw`, without
weakening any check. This is not "remove stage 5" — see "What must not be done".

## What happens today

Stage 5 (`crates/ackinacki-bridge/src/orchestrator.rs:909`) runs the `aggregate-proof`
subprocess. `aggregate-proof` calls `aggregate_and_prove_cached`
(`crates/bridge-evm-aggregator/src/bin/aggregate_proof.rs:100`), which **unconditionally** calls
`gen_evm_verifier_shplonk` (`crates/bridge-evm-aggregator/src/evm_export.rs:199`). In the SDK that
ends in `compile_solidity` (`snark-verifier-sdk/src/evm.rs:151`), i.e. `Command::new("solc")` with
`.spawn().unwrap()` (`snark-verifier/src/loader/evm/util.rs:107-114`). The resulting bytecode is
compared with the committed `contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin`
(`aggregate_proof.rs:109-139`); a mismatch is `aggregator VK drift`, and the proof is refused
before submission.

Without the compiler this is a panic inside the subprocess, which the CLI reports as a stage-5
failure, exit 12 (`crates/ackinacki-bridge/src/errors.rs:257`). There is no way around it:
`--allow-bin-drift` only relaxes the comparison, and compilation happens before it. Passing
`artifacts_dir = None` only stops `.sol`/`.bin` from being written to disk, not the generation.

Stage 5 runs after the irreversible burn and after the wait for a covering bundle, so a missing
or wrong `solc` at that point strands a withdrawal that has already cost the user the most. What
currently keeps that from happening is a set of guards around the compiler:

- `scripts/install.sh` downloads `solc 0.8.19` and verifies the version it reports
  (`crates/ackinacki-bridge/scripts/install.sh:235-261`, override `BRIDGE_SOLC_URL`);
- stage 1 refuses a real run if `solc` is not runnable under its bare name or reports another
  version (`check_solc_runnable`, `crates/ackinacki-bridge/src/preflight.rs:787`, `:1240`;
  `REQUIRED_SOLC_VERSION`, `:1221`);
- the manual install is documented as Step 0b in `crates/ackinacki-bridge/README.md:347`, as
  step 3 of the prerequisites in `crates/ackinacki-bridge/docs/advanced_user_withdraw_runbook.md:193`,
  and in `crates/ackinacki-bridge/QUICKSTART.md:27-36`.

The relayer takes the same path: `bridge-relayer-daemon` runs the same `aggregate-proof` binary for
bundle proofs (`crates/bridge-relayer-daemon/src/aggregator.rs:57`), so a relayer host needs `solc`
for the same reason.

## The observation the proposal rests on

`snark-verifier-sdk/src/evm.rs:150-151`, `gen_evm_verifier`:

```rust
let sol_code = loader.solidity_code();       // deterministic from (params, vk, num_instance)
let byte_code = compile_solidity(&sol_code); // the only solc call
```

The self-check exists to catch **VK drift**. The source is already fully determined by the key;
compiling it adds nothing to that check — it turns the same text into bytes. Comparing `sol_code`
is therefore enough.

## Proposal

1. Add `gen_evm_verifier_sol_shplonk(...) -> String` to `bridge-evm-aggregator`: the body of the
   SDK's `gen_evm_verifier` up to `compile_solidity`, specialised to SHPLONK. Everything it needs is
   public: `snark_verifier::loader::evm::EvmLoader` and its `solidity_code`,
   `snark_verifier_sdk::{CircuitExt, PlonkVerifier, SHPLONK}`,
   `snark_verifier::system::halo2::{compile, Config, transcript::evm::EvmTranscript}`.
   `snark-verifier` is already a direct dependency of the crate.
2. `aggregate_and_prove_cached` returns the source; bytecode is produced only on explicit request,
   on the verifier regeneration path (`export-inner-aggregator`).
3. The self-check in `aggregate_proof.rs` compares the source with a committed
   `BridgeWithdrawalAggregatorVerifier.sol`.
4. The EIP-170 gate (24 576 bytes, `evm_export.rs:212`) moves to regeneration. It is a property of
   the deployed contract, which is already deployed and compared byte for byte at stage 1.
5. Remove the guards listed above: the `solc` step and `BRIDGE_SOLC_URL` in `install.sh`,
   `check_solc_runnable` and `REQUIRED_SOLC_VERSION` in preflight, Step 0b in the README, the
   `solc` step in the runbook, and the `solc` mentions in `QUICKSTART.md` and the root
   `README.md:112`. For the changelog this is a removed prerequisite and a removed environment
   variable.

## Chain of trust before and after

| Link | Today | After |
|---|---|---|
| VK → `.sol` | — | `aggregate-proof`, every withdrawal |
| VK → `.bin` | `aggregate-proof` + `solc`, every withdrawal | — |
| `.sol` → `.bin` | — | verifier regeneration / CI, on key rotation |
| `.bin` → deployed runtime | stage 1 (`preflight.rs:1987-2015`), every withdrawal | unchanged |

Today the middle link is checked on the user's machine at the cost of a compiler; after the change
it is checked where `solc` is present by definition.

## Side benefit

The `0.8.19` pin exists precisely because bytecode is compared, and bytecode depends on the
compiler version. Source does not: the dependency goes, the pin goes, and so does a whole class of
false "VK drift" alarms for anyone with a different compiler. The version check in preflight exists
only because of the bytecode comparison.

## One-time work required

A reference `BridgeWithdrawalAggregatorVerifier.sol`. It is not in the repository, although `.sol`
files are committed for `FallbackAggregatorVerifier`, `LayerHashesAggregatorVerifier` and
`PrimaryAggregatorVerifier` (`contracts/ethereum/verifiers/`). It can be produced with a warm
`pk_cache`, without keygen.

Not verified here: that the three committed `.sol` files are the output of the generator at the
current pin. Before the relayer's self-check switches to them, regenerate each and confirm it
matches both its `.sol` and its `.bin`.

## Risks

- **`.sol` → `.bin` is no longer checked on every withdrawal.** Covered in CI at regeneration; and
  stage 1 keeps comparing `.bin` with the deployed runtime byte for byte.
- **The reference `.sol` has to be updated on VK rotation.** The same discipline that already
  applies to `.bin`; tie the two together in one regeneration script so they cannot diverge.
- **Source formatting.** A text comparison is sensitive to the version of the Yul generator
  (`snark-verifier`), not only to the VK. That is mostly a plus — generator drift is caught too —
  but the reference has to be rebuilt whenever the dependency is upgraded. The error message must
  say so, otherwise a crate upgrade reads as a compromised key.

## Alternative, if a text comparison is not acceptable

Compare a hash of the serialised VK (`vk.write(SerdeFormat::RawBytes)` → sha256) and commit
`*.vk.sha256`. The artifact is smaller and immune to formatting, but catches **only** key drift,
not generator drift.

## What must not be done

**Remove stage 5 from the protocol.** `withdrawByProof` does not pay without a proof
(`contracts/ethereum/src/AckiNackiBridge.sol:1156`): the recipient is a public input of the proof
(`:1181-1191`), the `nullifier` guards against replay (`:1193-1195`), and the identity pair is
checked before the cryptography (`:1165-1167`). Without a proof the EVM side has no way to know the
burn happened, and what remains is a trusted attester — a compromise of its signers drains the
whole treasury. That is a change of security model, not a prerequisites optimisation.

**Move stage 5 into a service** — legitimate and safe (the proof is verified by the chain, not taken
on trust, and whoever produces it cannot redirect the funds), but it is not code removal: it is new
infrastructure, and no prover-as-a-service exists today. It is the only path that also removes the
remaining heavy prerequisites — the ~256 MB ceremony, the ~2.65 GB `event_pk`, ~40 GB of peak RAM
(`crates/ackinacki-bridge/README.md:220`, `:245-250`). It needs a "prove it yourself" fallback,
otherwise an unavailable service locks funds (it does not lose them: the proof is deterministic
for a given event and chain state).

**Wait for the time to shrink.** The long part of a withdrawal is stage 4b, the wait for a covering
bundle — up to ~91 min on L2 (`crates/ackinacki-bridge/QUICKSTART.md:118`). It does not depend on
the prover; it shrinks only with a shorter stride (L1: 1024 seq_no ≈ 5.7 min,
`advanced_user_withdraw_runbook.md:82`) or more frequent bundle publication.
