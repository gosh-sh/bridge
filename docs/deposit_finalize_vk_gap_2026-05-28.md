# Deposit `finalizeDeposit` ↔ `ZKHALO2VERIFYWITHVK` VK gap + Option 2 plan

> **Status (2026-05-28).** The partner's updated `TokenBridge` (acki-nacki branch
> `poseidon_dex_with_verify`, `contracts/exchange/TokenBridge.sol`) wires
> `finalizeDeposit` to verify a deposit proof natively via
> `gosh.zkhalo2VerifyWithVK(VK_BLOB, publicInputs, proof)`. As shipped this is a
> **placeholder** that cannot validate a real deposit proof. This doc records the
> gap (with empirical evidence), the chosen fix (**Option 2**), the confirmed
> feasibility, and the sequenced implementation plan.

## 1. What the partner shipped

`finalizeDeposit(bytes proof, uint256 srcDappId, bytes srcSender, uint256 recipient_an, uint128 amount, uint32 tokenId, uint256 srcDepositId)`:

- Builds a **4-element** public-input vector `[srcDepositId, _srcSenderToFr(srcSender), amount, srcDappId]`.
- Verifies it against an embedded `VK_BLOB` constant using the 3-operand opcode
  (`VkBlob` magic `"VKBLOB\x00\x00"`, strict 32-byte LE Fr, raw SHPLONK proof).
- The wire framing matches our producer side
  (`crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs::Halo2TvmOperands`)
  exactly.

The embedded `VK_BLOB` is, by its own inline comment, our **Circuit 1B fallback**
VK (BLS attestation, K=20, 4 inputs) taken from the handoff fixture
`halo2_tvm_for_serhii_2026-05-26/fixtures/fallback_vk_blob.bin`.

## 2. The gap

Two independent problems:

1. **Wrong VK.** The fallback VK proves BLS committee attestation, not deposits.
   A real deposit proof can never verify against it.
2. **Wrong public-input layout.** Our `deposit-prover` emits **7** inputs
   (`deposit-prover/src/prover.rs`):
   `[depositId, sender, amount, contract_address, block_hash_high, block_hash_low, promise_commit]`.
   The contract reconstructs only 4, putting `srcDappId` where the circuit expects
   `contract_address` and dropping the block-hash binding + keccak-coprocessor
   `promise_commit`.

But the deeper, blocking issue is that **the opcode physically cannot read our
deposit VK at all**, for two independent reasons:

### Blocker 1 — different `halo2_proofs` backend forks
- Opcode reader (`tvm-sdk/tvm_vm/Cargo.toml`): `halo2-base` from
  `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381`, feature **`halo2-axiom`**
  ⇒ axiom's `halo2_proofs` fork (`halo2-axiom v0.4.5`).
- Deposit prover (`deposit-prover/Cargo.toml`): `axiom-crypto/halo2-lib` v0.4.1,
  feature **`halo2-pse`** ⇒ `privacy-scaling-explorations/halo2` `v2023_04_20`.

These backends have incompatible `VerifyingKey::write/read` byte layouts.

### Blocker 2 — different circuit builder shape
- Opcode rebuilds the constraint system via
  `VerifyingKey::read::<_, BaseCircuitBuilder<Fr>>` (single-phase, no challenge).
- Deposit VK is from `EthCircuitImpl<Fr, DepositEventCircuitV2>` / `RlcCircuitBuilder`
  — multi-phase (`num_advice_per_phase: [50, 25]`), `num_rlc_columns: 3`, with an
  RLC `Challenge`. `BaseCircuitParams` cannot describe that system.

## 3. Empirical confirmation (2026-05-28)

A standalone test built against the **exact opcode backend** (gosh-fork
`halo2-base`, `halo2-axiom`) — see `../vk-compat-check/axiom-reader/` — keygens a
multi-phase RLC-style VK (2 advice phases + 1 challenge, the same family as the
deposit circuit), serialises it with `SerdeFormat::RawBytes`, and runs the
opcode's literal read call:

```text
[producer] multi-phase challenge circuit VK built (phases(advice)=2 num_fixed=1 challenges=1 perm_cols=2)
[control]  same bytes read back with ChallengeCircuit: OK
[reader]   opcode read REJECTED the multi-phase VK as expected: failed to fill whole buffer
RESULT: PASS — ZKHALO2VERIFYWITHVK cannot consume an RLC/multi-phase (deposit-shaped) VK.
```

The control (reading the identical bytes with the correct circuit type succeeds)
proves the rejection is the shape mismatch, not a broken harness. Blocker 2 is
thus empirically proven against the real backend. Blocker 1 is established
statically from the dependency pins (the two stacks don't even share a toolchain:
the PSE stack needs nightly via `poseidon-primitives`'s `#![feature(slice_group_by)]`).

## 4. Chosen fix — Option 2: generalise the opcode to read RLC/EthCircuit VKs

Rather than re-architect the deposit circuit (Option 3) or bolt a snark-verifier
aggregation wrapper with accumulator instances the contract can't reconstruct
(Option 1), we teach `ZKHALO2VERIFYWITHVK` + the `VkBlob` to support the
axiom-eth circuit shape.

### 4.1 Feasibility — CONFIRMED

The constraint system of an axiom-eth circuit is **fully determined by its params**,
independent of the inner circuit logic. From `axiom-eth/src/utils/eth_circuit.rs`:

```rust
impl<F, I: EthCircuitInstructions<F>> Circuit<F> for EthCircuitImpl<F, I> {
    type Params = EthCircuitParams;            // { rlc: RlcCircuitParams, keccak: PromiseLoaderParams }
    fn configure_with_params(meta, params) -> EthConfig { EthConfig::configure(meta, params) }
    // inner `I` (deposit logic) is only used in synthesize(), NOT configure()
}
```

So the opcode can reconstruct the deposit VK's CS **generically** via
`EthCircuitImpl<Fr, NoopInstructions>` + the carried `EthCircuitParams`, WITHOUT
compiling `DepositEventCircuitV2` into the node. A `NoopInstructions`
`EthCircuitInstructions` impl is trivial (its `virtual_assign_phase0` is never
called during VK read).

### 4.2 `VkBlob` v2 format change (producer + consumer)

Add a **circuit-shape discriminator** after the transcript byte:

```text
  off  size  field
    0     8  magic = b"VKBLOB\x00\x00"
    8     1  version           = 2            (bump from 1)
    9     1  transcript_kind   = 0 (Blake2b)
   10     1  circuit_shape     = 0 (BaseCircuitBuilder) | 1 (EthCircuitImpl/RLC)   [NEW]
   11     5  reserved          = 0 × 5         (was 6)
   16     4  config_len  (u32 LE)
   20  cl    config_json       BaseCircuitParams (shape 0) OR EthCircuitParams (shape 1)
  ...     4  vk_len      (u32 LE)
  ...  vl    vk_bytes
```

- `shape == 0`: existing behaviour (fallback / BaseCircuitBuilder circuits).
- `shape == 1`: `config_json` is `EthCircuitParams`; consumer reads with
  `EthCircuitImpl<Fr, Noop>`.

Version stays back-compatible by branch: v1 blobs (no `circuit_shape`) keep working
if we treat absence as shape 0, or we hard-require v2. Recommend hard-require v2 and
regenerate the fallback blob too (single source of truth).

### 4.3 Opcode change (`tvm-sdk/tvm_vm/src/executor/zk_halo2_with_vk.rs`)

```rust
let vk = match bundle.circuit_shape {
    Base => VerifyingKey::read::<_, BaseCircuitBuilder<Fr>>(&mut s, RawBytes, base_params)?,
    Rlc  => VerifyingKey::read::<_, EthCircuitImpl<Fr, Noop>>(&mut s, RawBytes, eth_params)?,
};
```

Requires adding `axiom-eth` (rlc + utils::eth_circuit + keccak PromiseLoader) to
`tvm_vm`, **patched onto the gosh `halo2-axiom` backend**.

### 4.4 Backend unification (the hard part, resolves Blocker 1)

The deposit VK bytes must be produced on the **same** halo2 backend the opcode
reads. Today `deposit-prover` uses `halo2-pse`. To make its VK byte-compatible:

- **Switch `deposit-prover` to the gosh `halo2-axiom` backend** (use the gosh fork
  + axiom-eth patched onto it), regenerate keys, and re-export the VkBlob from
  there. This is the cleanest route and reuses the same patched axiom-eth the node
  uses.

### 4.5 Integration constraints discovered
- **axiom-eth needs nightly** (`pub trait Field = ...` trait alias). The gosh fork
  pins `nightly-2026-02-03`, which builds both.
- **PREREQUISITE — gosh halo2 fork is a version behind (BLOCKING).** Empirically
  tested 2026-05-28 (`../vk-compat-check/rlc-reader/`): building `axiom-eth v0.4.3`
  patched onto the gosh fork yields **27 compile errors** — all API drift because
  the gosh fork is axiom **halo2-lib v0.4.0 / zkevm-hashes 0.2.0**, while axiom-eth
  v0.4.3 (the version the deposit circuit uses) expects **v0.4.1 / 0.2.1**.
  Concrete missing/changed items:
  - `zkevm_hashes::keccak::component::encode::pack_native_input`,
    `...::circuit::shard::{pack_inputs_from_keccak_fs, transmute_keccak_assigned_to_virtual}`
    (added in 0.2.1).
  - `KeccakComponentShardCircuit::{inputs, hasher, base_circuit_builder}` methods.
  - `halo2_base::virtual_region::lookups::basic` module path.
  - `BaseCircuitParams: Hash` derive; `PoseidonHasher::spec`.
  The `halo2-axiom` backend itself is compatible — the issue is purely the fork
  snapshot. **Resolution: bump the gosh fork (`halo2-lib-zkevm-sha256-and-bls12-381`)
  to axiom halo2-lib v0.4.1 / zkevm-hashes 0.2.1**, so axiom-eth (and the deposit
  circuit) and the opcode share one halo2-lib version. This is gosh-maintained and
  affects the existing `ZKHALO2VERIFY*` opcodes that depend on the fork, so it needs
  the gosh team. (Alternative: pin the whole deposit/axiom-eth stack to a v0.4.0-era
  axiom-eth — undesirable, the deposit-prover is already on 0.4.1-based axiom-eth.)
- Resolving deposit-prover's transitive deps on an old nightly needs its (currently
  uncommitted) `Cargo.lock`; several latest patch versions pull `edition2024` /
  rustc ≥ 1.80.

## 5. Sequenced plan

1. **[node feasibility build — DONE, found a prerequisite]** Tried building
   `axiom-eth v0.4.3` patched onto the gosh `halo2-axiom` backend
   (`../vk-compat-check/rlc-reader/`). Result: **27 API-drift errors** — the gosh
   fork is axiom halo2-lib **v0.4.0 / zkevm-hashes 0.2.0**, axiom-eth v0.4.3 needs
   **v0.4.1 / 0.2.1**. The backend is compatible; the fork is one minor version
   behind. See §4.5.
1a. **[gosh-fork bump — DONE 2026-05-28, branch `bump-halo2-lib-v0.4.1` commit
   `08cfb36`]** Bumped `halo2-lib-zkevm-sha256-and-bls12-381` to axiom halo2-lib
   v0.4.1 / zkevm-hashes 0.2.1 by re-basing the gosh SHA-256 + BLS12-381 additions
   onto axiom `v0.4.1-git` (the version axiom-eth v0.4.3 pins). Done in-tree (PR to
   gosh pending) rather than waiting on the gosh team. Key work:
   - `git rebase --onto v0.4.1-git` of the gosh delta (base = `zkevm-hashes-v0.2.0`,
     `5a9f4b1`); only 4 `Cargo.toml` conflicts, all source auto-merged.
   - Ported gosh's SHA-256 component to the v0.2.1 poseidon API
     (`get_poseidon_spec` / `create_native_poseidon_sponge`, `hash()` by-value).
   - Workspace `[patch]` redirecting `halo2-axiom` → `gosh-sh/halo2-axiom` and the
     `halo2-lib.git` crates → local paths, so `snark-verifier-sdk` (now pulled by
     zkevm-hashes 0.4.1) doesn't introduce a second, clashing halo2 backend.
     **Consumers (tvm-sdk, orchestrator) must mirror these patches in their root
     manifests.**
   - **Kept stable-Rust-compatible** (revised 2026-05-29): `VirtualRegionManager::Assignment`
     keeps NO default (the upstream `= ()` default needs nightly
     `associated_type_defaults`, which would break every stable consumer of the fork).
     Every halo2-base impl already names `Assignment` explicitly. The *only* impl that
     relied on the upstream default — axiom-eth's `RlcManager` — is fixed by a one-line
     consumer-side patch (`type Assignment = ();`, branch `gosh-stable-rlcmanager-assignment`),
     so the gosh PR stays a clean, stable-preserving v0.4.1 bump.
   **Validated**: fork builds standalone; gosh SHA-256 + BLS12-381 tests pass;
   axiom-eth v0.4.3 compiles against it; `EthCircuitImpl<Fr, Noop>` VK
   keygen→serialize→`VerifyingKey::read::<_, EthCircuitImpl<Fr, Noop>>` round-trips
   (`../vk-compat-check/rlc-reader/`, RESULT: PASS).
2. **[VkBlob v2 — DONE (producer wire) 2026-05-28]** Implemented the shape
   discriminator + config carriage in
   `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs`:
   - New `CircuitShape { Base = 0, Rlc = 1 }` + `VkConfig { Base(BaseCircuitParams),
     Rlc(Vec<u8>) }` (RLC config carried as **opaque `EthCircuitParams` JSON** so
     the crate needs no `axiom-eth` dep until o3a/o4 land).
   - `VkBlob` is now shape-aware: `from_native` (Base) still emits the **frozen v1**
     wire byte-for-byte; new `from_native_rlc(eth_config_json, vk)` emits **v2** with
     the shape byte in `header[10]`. `read` accepts both (v1 rejects a non-zero shape
     byte; v2 rejects unknown shapes).
   - `Halo2TvmOperands::verify` round-trips Base in-process and returns a clear
     "verify on the AN node opcode instead" error for Rlc (no axiom-eth here yet).
   - 5 new unit tests (v1 byte-stability + v1-tagged, v2 RLC round-trip + shape byte,
     v1-nonzero-shape-rejected, v2-unknown-shape-rejected, bad-JSON guard); all 11
     module tests green. Built/tested in isolation via `../vk-compat-check/vkblob-v2/`
     (the full orchestrator can't link without the private `gosh-halo2-crypto-lib`
     sibling), formatted with the repo's pinned nightly rustfmt.
   - **Still pending in this step**: the *deposit VkBlob export path* (producing the
     real `EthCircuitParams` JSON + deposit VK to feed `from_native_rlc`) needs the
     deposit-prover on the gosh backend → blocked on o3a.
3. **[deposit-prover backend]** Port `deposit-prover` to the gosh `halo2-axiom`
   backend (patched axiom-eth); regenerate keys; add a deposit-VkBlob export path.
4. **[opcode — DONE 2026-05-29, branch `serhii/node-3406-vergrth16-with-vk`]** Added the
   `axiom-eth` dep + `circuit_shape` branch to `tvm_vm`:
   - `zk_halo2_with_vk_bundle.rs`: v2 `circuit_shape` byte (offset 10) + `BundleConfig
     { Base(BaseCircuitParams), Rlc(Vec<u8>) }`; v1 stays byte-compatible (shape pinned 0).
   - `zk_halo2_with_vk.rs`: branch in `get_or_insert_vk` — `read_base_vk`
     (`BaseCircuitBuilder<Fr>`) vs `read_rlc_vk` (`EthCircuitImpl<Fr, Noop>` + empty
     `EthCircuitInstructions`, params from the `EthCircuitParams` JSON).
   - **Backend unification** (resolves Blocker 1): workspace-root `[patch]` points all three
     halo2-base sources (gosh-fork git, axiom `halo2-lib.git`, crates.io) + halo2-ecc /
     zkevm-hashes at ONE git url+rev of the gosh fork (`file://…?branch=bump-halo2-lib-v0.4.1`).
     A shared local *path* can't patch multiple sources — cargo keeps the original for the
     loser → two clashing `halo2-base` copies. Plus `axiom-eth` → local fork (RlcManager patch).
   - **Build requires nightly** for the `gosh` feature: `snark-verifier-sdk` v0.1.7-git uses
     `trait_alias`. The stable gosh BaseCircuitBuilder path is unaffected; only RLC forces nightly.
   - **Tests** (`cargo +nightly test -p tvm_vm --lib`): 6 v2 bundle-parse tests + 3
     `rlc_branch_tests` (real `EthCircuitImpl` VK keygen → byte-for-byte round-trip through the
     Rlc reader; Base reader does not faithfully round-trip an RLC VK; malformed-JSON guard);
     all v1 negatives + the real DarkDex W=8 L0 Base fixture stay green.
   - **Still pending (o5b)**: a *valid-RLC-proof* end-to-end fixture (real deposit proof +
     instances + RLC VkBlob) — needs the deposit-prover exported on the gosh backend (step 3)
     and an agreed final opcode ABI (single-bundle vs 3-operand). See
     `zkhalo2verifywithvk_reference.md` §15.
5. **[partner]** Update `TokenBridge.finalizeDeposit` to the real **7-input** layout
   `[depositId, sender, amount, contractAddr, blockHashHi, blockHashLo, promiseCommit]`
   and embed the real deposit `VK_BLOB`. Decide how the contract sources the ETH
   block hash (AN-side ETH header oracle) and how `promise_commit` is supplied
   (it is an internal coprocessor artefact — likely passed through from the relayer
   alongside the proof, since the contract cannot recompute it).
6. **[e2e]** Relayer → deposit proof → `Halo2TvmBundle` → `finalizeDeposit` on a
   local AN node built from the 3-operand opcode line.

## 6. Open questions for the partner
- Can `finalizeDeposit` accept `promise_commit` (and the block-hash halves) as
  explicit call args, or should the deposit circuit's public-input set be reduced?
- Source of truth for the ETH block hash on the AN side (oracle vs passthrough).
- Confirm the AN node build tracks the 3-operand `ZKHALO2VERIFYWITHVK` line
  (`pruvendo/zkhalo2verifywithvk-on-main`), not the older single-bundle operand.
- ~~Can the gosh halo2 fork be bumped to axiom halo2-lib v0.4.1 / zkevm-hashes 0.2.1?~~
  **Resolved 2026-05-29 — we did it ourselves** (branch `bump-halo2-lib-v0.4.1`, stable-preserving;
  PR to gosh pending push access). Remaining gosh-side ask: review + merge the bump to the fork's
  `main` so consumers can drop the `file://` patch for a public git rev.
- ~~Does the node accept building the `gosh` feature on a nightly toolchain?~~ **Resolved
  2026-05-29 — nightly accepted.** `tvm-sdk/rust-toolchain.toml` pins `channel = "nightly"`
  (the RLC stack `axiom-eth` → `snark-verifier-sdk` needs `trait_alias`). The stable
  BaseCircuitBuilder opcode path is unaffected. Recommend pinning a specific nightly date for
  reproducible CI once the team agrees one.

## 7. Artefacts
- Empirical test: `../vk-compat-check/axiom-reader/` (Blocker 2, real backend, kept as regression fixture).
- `../vk-compat-check/rlc-reader/` — node feasibility build (axiom-eth patched onto gosh backend); surfaces the 27 v0.4.0↔v0.4.1 drift errors documented in §4.5. Kept as the reproducer for the fork-bump prerequisite.
- `../vk-compat-check/vkblob-v2/` — isolation harness that compiles the real `halo2_tvm_bundle.rs` (via `#[path]`) so the VkBlob v1/v2 unit tests run without the private circuit siblings.
- `../vk-compat-check/pse-producer/` — incomplete Blocker 1 producer (dep-resolution blocked without deposit-prover `Cargo.lock`).
- Gosh halo2 fork cloned to `../halo2-lib-zkevm-sha256-and-bls12-381/` (sibling expected by the orchestrator); bump on branch `bump-halo2-lib-v0.4.1`.
- `../axiom-eth/` — local axiom-eth clone, branch `gosh-stable-rlcmanager-assignment` (one-line `RlcManager::Assignment = ()` patch so axiom-eth builds against the stable gosh fork). Referenced by the tvm-sdk `[patch]`.
- tvm-sdk opcode + bundle v2 + `[patch]` on branch `serhii/node-3406-vergrth16-with-vk` (o4).
