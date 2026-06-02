# AN-side Halo2 Deposit-Proof Verification — Design Alignment with `tvm-sdk` `ZKHALO2VERIFY`

**Status**: design memo for the AN partner team (Serhii et al.) + record for our integration team.
**Authors**: bridge integration team.
**Date**: 2026-05-17.
**Companion docs**: `docs/an_partner_integration_plan.md` (Decision Log 2026-05-17), `docs/verifying_eth_proof_on_an.md`, `docs/integration_analysis.md` §1.3 / §5.4.

---

## TL;DR

The Phase 4.3 pivot (Decision Log 2026-05-17) retires the legacy ETH-side Groth16 deposit-verifier chain and routes the ETH→AN deposit-event proof through **native AN-side Halo2 SHPLONK verification**. The AN side already has a Halo2 SHPLONK opcode in flight — `ZKHALO2VERIFY` (0xC7 0x49) on branch `serhii/node-3406-vergrth16-with-vk` of `tvm-sdk`. The current implementation, however, **hard-codes the verifying key** to the DarkDex W=8 circuit (`DARK_DEX_W8_VK_BYTES`, 842 bytes), which is the wrong shape for a general bridge use-case.

The bridge needs a variant of the opcode that **accepts the verifying key as a caller-supplied operand** so the deposit-prover circuit's VK can be set once at deployment time and used by `TokenBridge.finalizeDeposit(...)`. We call this opcode `ZKHALO2VERIFYWITHVK`; it landed in `tvm-sdk` on 2026-05-22 at dispatch byte `0xC7 0x4A` (PR #240, real implementation on the `serhii/verhalo2shplonk-real-impl` branch).

The single biggest open question (Q-WIRE-1 below) is *which Halo2 transcript flavour* the AN-side verifier expects. The current implementation uses Blake2b via `gosh-zk-snark-halo2-utils::proof::Proof::verify_with_vk`; the bridge's `deposit-prover/` crate has historically used the Keccak transcript for EVM compatibility. Aligning these is a one-line producer-side switch *or* a transcript parameter on the opcode — and the answer dictates whether `deposit-prover/` needs a `--blake2b-transcript` mode.

---

## 1. Context: Phase 4.3 demolition

See `docs/an_partner_integration_plan.md` Decision Log 2026-05-17 for the full rationale. In short:

- The Halo2 SHPLONK proof produced by `deposit-prover/` already encodes everything the AN side needs to credit a user (deposit event in a real Ethereum block; 10 public inputs: `[depositId, sender, amount, contractAddress, anWorkchain, anAccountHigh, anAccountLow, blockHashHigh, blockHashLow, promiseCommit]` — the AN recipient `anWorkchain`/`anAccount` was bound in-circuit 2026-06-02 so the credited account is proven, not trusted).
- On Ethereum we previously needed a gnark Groth16 wrapper because of EIP-170's 24 KB contract code limit. **The AN side has no such limit**, so we can verify Halo2 SHPLONK natively and skip the wrapper entirely.
- Skipping the wrapper eliminates two attack surfaces (R15 no-op `Define` stub; any EIP-170-driven wrapper simplifications) — see `docs/audit_trail_v2.md` R-8.

This pushes the entire deposit-side verification work onto the AN side, which is what this memo is about.

## 2. What's already in `tvm-sdk` (branch `serhii/node-3406-vergrth16-with-vk`)

| Component | File | Notes |
| --------- | ---- | ----- |
| Opcode `ZKHALO2VERIFY` at `0xC7 0x49` | `tvm_vm/src/executor/zk_halo2.rs` | Handler `execute_halo2_proof_verification` |
| Hard-coded VK | `tvm_vm/src/executor/zk_halo2_utils.rs` | `DARK_DEX_W8_VK_BYTES: [u8; 842]` for DarkDex K=19 W=8 circuit |
| Cached `(VerifyingKey, ParamsKZG<Bn256>)` via `OnceLock` | same | Warmup spawned via `warmup_halo2()` thread on node startup |
| Public-input parsing | `execute_halo2_proof_verification` | 32-byte stride; dual path (u64 packed in last 8 bytes vs full 32-byte LE `Fr::to_repr()`) |
| Cargo deps | `tvm_vm/Cargo.toml` | `halo2-base` (gosh fork w/ `halo2-axiom`), `gosh-zk-snark-halo2-utils` (Blake2b SHPLONK transcript), `pse-poseidon` — all behind the `gosh` feature |
| Mnemonic + dispatcher | `tvm_assembler/src/simple.rs`, `tvm_vm/src/executor/engine/handlers.rs` | Registered under `#[cfg(feature = "gosh")]` |
| Self-acknowledged limitation | `zk_halo2_utils.rs` | `// TODO: we need VK per each desired historical window, now for tests we need 4 and in future 128 will be required!` |

### 2.1 Current stack ABI

```
ZKHALO2VERIFY:
  Stack (top→bottom):
    pub_inputs_cell : Cell containing N × 32 bytes (LE Fr representation, with u64-shortcut)
    proof_cell      : Cell containing the Halo2 SHPLONK proof bytes (Blake2b transcript)
  Pushes:
    Boolean         : true if verification succeeded, false otherwise

  VK / ParamsKZG    : implicit, hard-coded to DarkDex W=8 K=19
  Gas               : not currently metered
```

### 2.2 What's good and reusable

- **The 32-byte LE `Fr` wire format** for public inputs is exactly what the bridge needs and matches the producer-side `Fr::to_repr()` from the `halo2-axiom` ecosystem `deposit-prover/` uses.
- **`gosh-zk-snark-halo2-utils::proof::Proof::verify_with_vk`** is the right abstraction — it accepts an arbitrary `VerifyingKey<G1Affine>` + `ParamsKZG<Bn256>`, so the underlying verification machinery is **already circuit-agnostic**. The only gap is the API surface that gates it.
- **`OnceLock` caching + background warmup** for the heavy `EvaluationDomain<Fr, K=19>` precomputation is essential and reusable for any per-circuit VK we add. The `WithVK` variant just needs a small per-VK cache (a `Mutex<HashMap<Hash32, (VK, Params)>>` keyed by `keccak256(vk_bytes)`).

### 2.3 What blocks bridge use

- **Hard-coded VK and circuit params**. The bridge's deposit-prover Halo2 circuit has a different `BaseCircuitParams` (different K, different `num_advice_per_phase`, different `lookup_bits`) and a different VK from DarkDex W=8. The opcode as written cannot verify our circuit at all.
- **No per-VK cache**. Even if `DARK_DEX_W8_VK_BYTES` were replaced by an `enum CircuitFlavour { DarkDexW8, DarkDexW128, BridgeDeposit, … }`, every additional flavour requires a code change to the VM.
- **No gas accounting**. Mainnet readiness requires a benchmark-driven gas number for `ZKHALO2VERIFY` + `ZKHALO2VERIFYWITHVK`; the current placeholder `ZKHALO2_VERIFY_WITH_VK_GAS_PRICE = 5_000` (see `tvm_vm/src/executor/zk_halo2_with_vk.rs`) is a structural guess scaled from `VERGRTH16_GAS_PRICE` for the bigger VK (kilobytes vs 192 B).
- **Transcript flavour not parameterised** (see §4 Q-WIRE-1).

---

## 3. Proposed extension: `ZKHALO2VERIFYWITHVK`

We added a **second opcode** rather than re-purposing `ZKHALO2VERIFY`, for the reasons captured in `docs/an_partner_integration_plan.md` Decision Log 2026-05-17:

- `ZKHALO2VERIFY` keeps its compact 2-operand calling convention for circuits whose VK is naturally global to the chain (zkLogin-style; DarkDex). Cheaper gas, no per-call deserialization of the VK.
- `ZKHALO2VERIFYWITHVK` takes 1 operand (a `Halo2TvmBundle` cell carrying the VK + config + instances + proof together) and lets a contract carry its own VK in storage. Slightly more expensive gas; one opcode covers every circuit anybody ever deploys to AN.

Final naming is a partner decision; below we use `ZKHALO2VERIFYWITHVK` for concreteness.

### 3.1 Proposed stack ABI

```
ZKHALO2VERIFYWITHVK:
  Stack (top→bottom):
    vk_cell         : Cell containing the canonical-compressed binary form of
                      `halo2_proofs::plonk::VerifyingKey<G1Affine>` for Bn256,
                      written by `VerifyingKey::write(...)` on the producer side.
                      Carries the embedded `BaseCircuitParams` (K, advice
                      columns, lookup bits, instance columns) so the consumer
                      does not need a separate "params" operand.
    pub_inputs_cell : Cell containing N × 32 bytes (LE Fr representation, with
                      u64-shortcut, identical layout to ZKHALO2VERIFY).
    proof_cell      : Cell containing the Halo2 SHPLONK proof bytes.
  Pushes:
    Boolean         : true on accept, false on cryptographic reject

  Throws FatalError on structural errors only:
    - VK bytes don't deserialize as a Halo2 VerifyingKey<G1Affine>
    - Public-inputs payload length is not a multiple of 32 bytes
    - Proof bytes don't deserialize as a valid SHPLONK proof container

  Cryptographic failure (well-formed but invalid proof) → false, no exception.
  This matches the VERGRTH16 / ZKHALO2VERIFY convention.
```

### 3.2 Proposed gas model

Modelled on `VERGRTH16_GAS_PRICE`, scaled up for the bigger VK (kilobytes vs 192 B):

```rust
/// Gas price for the `ZKHALO2VERIFYWITHVK` opcode.
///
/// Marginal cost over `ZKHALO2VERIFY`'s implicit baseline: the additional cost
/// covers:
/// - `VerifyingKey::<G1Affine>::read(...)` — VK deserialization +
///   `EvaluationDomain` reconstruction. For K=19 this is ~3 s wall-clock on
///   first call; amortise via a per-VK cache (see §3.3).
/// - One additional MSM proportional to `vk.cs.num_instance_columns`.
/// - Optional `ParamsKZG<Bn256>` reconstruction for chains where the SRS is
///   per-circuit rather than shared.
///
/// Concrete number TBD by benchmark; placeholder `5000` to start the
/// conversation.
pub const ZKHALO2VERIFY_WITH_VK_GAS_PRICE: i64 = 5_000;
```

### 3.3 Per-VK cache

Because building `EvaluationDomain<Fr, K>` is a multi-second precomputation, calling `ZKHALO2VERIFYWITHVK` with a fresh VK on every transaction would be infeasible. We propose:

```rust
static VK_CACHE: Lazy<Mutex<LruCache<[u8; 32], CachedHalo2Vk>>> =
    Lazy::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(8).unwrap())));

struct CachedHalo2Vk {
    vk: VerifyingKey<G1Affine>,
    params: ParamsKZG<Bn256>,    // or borrow a shared ParamsKZG if the SRS is global
}

// Cache key: keccak256(vk_bytes). Cheap, collision-resistant for VKs.
```

Cache size `8` is plenty for chains running a handful of bridge / zkLogin / app circuits concurrently. The eviction policy is LRU; a cold-cache call pays the ~3 s warmup gas; a warm call pays the ~few ms verification gas.

The cache should not include the proof bytes themselves — we cache *only the deserialised VK and KZG params*. Proof verification is per-call.

### 3.4 Bridge-side usage sketch (TVM-Solidity)

```solidity
// Compiled by TVM-Solidity-Compiler to ZKHALO2VERIFYWITHVK
// (mirroring gosh.vergrth16WithVK that we landed in the compiler PR for the Groth16 case).
function finalizeDeposit(
    TvmCell halo2Proof,
    uint256[10] memory publicInputs,   // [depositId, sender, amount, contractAddress,
                                       //  anWorkchain, anAccountHigh, anAccountLow,
                                       //  blockHashHigh, blockHashLow, promiseCommit]
    TvmCell vk
) public {
    require(
        gosh.zkHalo2VerifyWithVK(halo2Proof, publicInputs, vk),
        "invalid deposit proof"
    );
    require(publicInputs[3] == ETH_BRIDGE_ADDRESS_FR, "wrong bridge contract");
    require(!nullifier[publicInputs[0]], "already credited");
    nullifier[publicInputs[0]] = true;

    // The recipient is a PROVEN public input — credit the AN account bound in
    // the proof, not an EVM address (which is not a valid AN recipient).
    int8 anWorkchain = int8(int256(publicInputs[4]));
    uint256 anAccount = (publicInputs[5] << 128) | publicInputs[6];
    uint256 amount = publicInputs[2];
    _mintTo(anWorkchain, anAccount, amount);
}
```

The `vk` argument is stored once at deployment in immutable storage. Same trust-anchoring story as the gnark VK on Ethereum.

---

## 4. Open questions for the AN partner team

> **Status update 2026-05-22**: Serhii's explicit response — _"Принимай решения на
> свой вкус, Сергей готов принять любое наше решение"_ — closes the design phase
> from the partner side. The bridge team has therefore **frozen all five Q-WIRE
> questions** with the decisions below. The text of the original open questions
> is preserved for the audit trail. Phase A is now considered closed; the
> remaining work is purely implementation (Phase B onward).
>
> Each decision is dated **2026-05-22** and authored by the bridge integration
> team. Any future change to these decisions must be paired with a follow-up
> Decision Log entry in `docs/an_partner_integration_plan.md` and a bumped
> `Halo2TvmBundle::FORMAT_VERSION` byte.
>
> | ID | Decision | Owner of next action |
> |----|----------|----------------------|
> | Q-WIRE-1 | Blake2b SHPLONK transcript (sole supported flavour for v1; the bundle still carries a `transcript_kind` discriminator byte to keep the door open for Keccak in a future version) | bridge / closed |
> | Q-WIRE-2 | Verifier-only `ParamsKZG<Bn256>` built at runtime from 3 globally-embedded G1/G2 points, parameterised by `k = vk.cs.degree`. NOT carried in `Halo2TvmBundle` and NOT loaded from disk — SHPLONK verification only needs `g[0]`, `g2`, `s_g2`. | tvm-sdk (reuses the same `KZG_*_BYTES` constants `ZKHALO2VERIFY` uses for DarkDex W=8) |
> | Q-WIRE-3 | Strict 32-byte little-endian `Fr::to_repr()`; no u64 shortcut. `Fr::from_repr` rejects ≥ modulus inputs structurally | bridge / closed |
> | Q-WIRE-4 | Self-describing `Halo2TvmBundle` (see `crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs`). Magic `b"HALO2TVM"` + version `0x01` + transcript_kind byte + length-prefixed `(config_json, vk_bytes, instances, proof)` | bridge / closed |
> | Q-WIRE-5 | Pin `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` to a commit SHA in `tvm_vm/Cargo.toml`. Any SHA bump requires a CI gate that proves & verifies a checked-in `Halo2TvmBundle` fixture; deserialisation drift = red CI = blocked merge | tvm-sdk maintainer + bridge CI |
> | Q-NAME-1 | `ZKHALO2VERIFYWITHVK`, dispatch byte `0xC7 0x4A` (adjacent to `ZKHALO2VERIFY = 0xC7 0x49`). If `node-3406` reshuffles bytes pre-merge, our follow-up PR rebases onto whatever byte ends up adjacent | bridge / will rebase at follow-up PR |
>
> The five sub-sections below are kept as the rationale capture for each closed
> question. They explain _why_ we picked each side, not _whether_.

### Q-WIRE-1 — Halo2 transcript flavour

The current `ZKHALO2VERIFY` consumes proofs produced with `gosh-zk-snark-halo2-utils::Proof::create_for_circuit`, which uses the **Blake2b** SHPLONK transcript (`Blake2bWrite` from `halo2_proofs`).

The bridge's `deposit-prover/` currently produces proofs with the **Keccak256** transcript (for historical EVM-compatibility reasons that no longer apply post-Phase-4.3, since the AN side has no Keccak precompile gas pressure).

**Question**: should the bridge's `deposit-prover/` switch to the Blake2b transcript so its proofs are verifiable by the existing `gosh-zk-snark-halo2-utils` machinery? Or should `ZKHALO2VERIFYWITHVK` carry a transcript flavour discriminator on the stack (e.g. an additional `uint8` operand)?

Our preference: switch the producer side to Blake2b. It's a single-line change in `deposit-prover/src/prover.rs` and avoids growing the opcode surface.

**Decision 2026-05-22 — DECIDED Blake2b.** The fallback path in
`crates/bridge-prover-orchestrator/src/prover.rs::generate_fallback_proof`
already uses `Blake2bWrite` / `Blake2bRead`, and the
`Halo2TvmBundle` round-trip test in
`crates/bridge-prover-orchestrator/tests/halo2_tvm_bundle_round_trip.rs`
exercises this end-to-end against a real Circuit 1B fixture (k=20, 10
signers, ~21.2 KB bundle). Forcing Keccak on the AN side would require a
duplicate transcript gadget stack on a node that has no EVM-style
Keccak-precompile pressure to begin with. The `transcript_kind` byte in
`Halo2TvmBundle` is reserved (`0x00 = Blake2b`); we keep the door open
for `0x02 = Keccak` in a future format version without breaking
existing VKs.

### Q-WIRE-2 — SRS sharing vs per-circuit

Originally proposed as "load `$AN_DATA_DIR/halo2_srs/kzg_bn254_<k>.srs`
per supported `k` at VM startup". On real-impl review (2026-05-22) we
discovered the verifier-only path is much cheaper: SHPLONK only ever
touches `g[0]`, `g2`, and `s_g2` from the SRS — three group elements,
~320 bytes total. Serhii's existing
`tvm_vm/src/executor/zk_halo2_utils::build_kzg_verifier_params()`
hard-codes these for DarkDex W=8 at `k=19`. The real-impl branch
parameterises that pattern by `k`:

```
fn build_shared_kzg_params(k: u32) -> ParamsKZG<Bn256> {
    // ... reads embedded KZG_{G0,G2,S_G2}_BYTES into a K=0 dummy blob,
    //     then `dummy.from_parts(k, vec![g0], Some(vec![]), g2, s_g2)`.
}
```

This means **no on-disk SRS file is required**. The `KZG_*_BYTES`
constants live in `zk_halo2_utils.rs` (promoted to `pub(crate)` on the
real-impl branch) and are the same trusted-setup points DarkDex W=8
uses. The producer side (`bridge-prover-orchestrator`) still uses a
full `kzg_bn254_20.srs` blob for proving (proving needs g[0..2^k]); the
saving is purely on the verifier (TVM) side.


### Q-WIRE-3 — Public-input layout: full 32-byte LE Fr vs the u64 shortcut

The current opcode does dual-path parsing per element:

- If `bytes[0..24] == 0`, treat `bytes[24..32]` as big-endian u64 and feed `Fr::from(u64)`.
- Otherwise treat `bytes[..]` as little-endian `Fr::to_repr()`.

This is convenient for short integers (deposit IDs, block heights) but is **ambiguous** for genuine `Fr` elements whose first 24 bytes happen to be zero (e.g. a low-bit pattern in `Fr::from_bytes_le`).

**Question**: is this ambiguity intentional? For the bridge we'd prefer the WithVK variant to be *strictly LE Fr* — no shortcut — since the 7 deposit public inputs include addresses (160 bits, never confusable) and hashes (always full 32 bytes). Strictness saves us from a class of subtle bugs where a small Ethereum address coincidentally has 24 zero bytes prefix and gets reinterpreted.

**Decision 2026-05-22 — DECIDED strict 32-byte LE.** `ZKHALO2VERIFYWITHVK`
uses `Fr::from_repr(<[u8; 32]>)` directly, with no preprocessing.
`Fr::from_repr` returns `CtOption::None` for ≥ modulus inputs and the
opcode throws `FatalError` in that case (structural error, not
cryptographic reject). The legacy `ZKHALO2VERIFY` keeps its
dual-path / u64-shortcut behavior for backward compatibility with the
DarkDex W=8 contract that's already deployed. The two opcodes are
intentionally **not** interchangeable on the public-inputs encoding —
this is documented in the opcode mnemonic table comment.

### Q-WIRE-4 — VK serialization format

`gosh-zk-snark-halo2-utils::io::read_vk` reads VKs via `VerifyingKey::<G1Affine>::read(&mut reader, format, &BaseCircuitParams)` from `halo2_proofs`. That signature requires *both* the bytes *and* the `BaseCircuitParams` — meaning the VK bytes alone are not self-describing.

The current `ZKHALO2VERIFY` works around this by hard-coding `dark_dex_w8_config_params()` alongside `DARK_DEX_W8_VK_BYTES`. For the WithVK variant we have two options:

- **Option A**: the `vk_cell` carries a small TLV envelope: `[k:u32][num_advice:u32][lookup_bits:u32][num_instance_columns:u32][vk_bytes:...]`. The opcode parses the header, reconstructs `BaseCircuitParams`, then calls `VerifyingKey::read`.
- **Option B**: extend `gosh-zk-snark-halo2-utils` to emit a self-describing VK blob (`write_vk_with_params`) and a matching `read_vk_with_params`. Cleaner long-term but requires changes in the partner crate.

We prefer **Option B** and are happy to send a PR to `gosh-zk-snark-halo2-utils` for it.

**Decision 2026-05-22 — DECIDED Option B (self-describing bundle).** The
bridge has already implemented the format under the name
`Halo2TvmBundle` in
`crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs`. The wire
layout is:

```
offset  size   field
------  ----   -----
 0      8      magic = b"HALO2TVM"            (ASCII)
 8      1      format_version = 0x01
 9      1      transcript_kind                (0x00 = Blake2b; 0x01 reserved for Keccak)
10      4      config_json_len  (LE u32)
14      n0     config_json bytes              (serde_json of BaseCircuitParams)
14+n0   4      vk_len (LE u32)
…       n1     vk_bytes                       (canonical-compressed VerifyingKey<G1Affine>)
…       4      instances_len (LE u32)         (== num_instance_columns * count_per_col * 32)
…       n2     instances bytes                (concatenated 32-byte LE Fr per column, in column-major order)
…       4      proof_len (LE u32)
…       n3     proof bytes                    (raw SHPLONK proof body)
```

The opcode deserialises `config_json` → `BaseCircuitParams`, calls
`VerifyingKey::<G1Affine>::read(&vk_bytes, SerdeFormat::RawBytes, &cp)`,
then runs the standard `gosh-zk-snark-halo2-utils::Proof::verify_with_vk`
path. The round-trip test
(`crates/bridge-prover-orchestrator/tests/halo2_tvm_bundle_round_trip.rs`)
proves and verifies a real Circuit 1B (k=20, 10 signers) fixture
through this format; format version byte will bump on any breaking
change.

### Q-WIRE-5 — Halo2 axiom fork stability

`gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` is a single-commit fork of axiom's halo2-lib. It's pinned by `branch = "main"` in `tvm_vm/Cargo.toml`. Bridge proofs will be verified by this same crate, so we share a lockstep upgrade risk: any rebase of that fork that changes `VerifyingKey::read`'s on-wire format breaks every previously-issued bridge VK.

**Question**: do we pin to a commit SHA in `tvm-sdk` going forward? Or is there a CI test on the partner fork that asserts wire-format stability?

**Decision 2026-05-22 — DECIDED pin to commit SHA + format-stability CI
gate.** `tvm_vm/Cargo.toml` pins
`gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381` to a `rev = "<sha>"` (not
a branch). The bridge's CI checks in a `Halo2TvmBundle` fixture
(`crates/bridge-prover-orchestrator/tests/fixtures/halo2_tvm_bundle_v1_circuit1b_k20.bin`)
along with its expected `verify_with_vk` outcome. Any future SHA bump
must run this fixture through the latest verifier; a deserialisation
failure or a flipped verdict is a red CI and blocks the bump until the
producer side emits a new bundle under a bumped `format_version`. This
gives us a single, mechanical, hard-to-bypass guard against silent
on-wire drift in the partner fork.

### Q-NAME-1 — Opcode name

Working name `ZKHALO2VERIFYWITHVK`. Alternatives the AN team floated informally included `HALO2VERIFYVK`, `VERHALO2WITHVK`.

**Decision 2026-05-22 — `ZKHALO2VERIFYWITHVK` @ `0xC7 0x4A` (LANDED).**
Dispatch byte `0x4A` chosen as the next free slot after `0xC7 0x49 = ZKHALO2VERIFY`.
The compiler-side builtin is `gosh.zkHalo2VerifyWithVK(bundle)`. The
single-operand `Halo2TvmBundle` ABI replaced the original 3-operand
sketch during the real-impl pass; see `tvm-sdk` PR #240 and
`docs/an_partner_integration_plan.md` Decision Log 2026-05-22.

---

## 5. Implementation roadmap

### Phase A — confirm transcript + VK envelope (Q-WIRE-1, Q-WIRE-4)

Owner: AN team (Serhii) + bridge team (this side).
Outcome: a one-paragraph agreement on transcript flavour and VK on-wire format.

**Status (2026-05-22): CLOSED.** Serhii's "Принимай решения на свой вкус"
explicitly delegates the call to the bridge team, so all five Q-WIRE
questions are now decided (see §4 above). The bundle format is frozen at
`format_version = 0x01`; any change requires a version bump. The
implementation is unblocked on the design front; the only remaining
gate is `serhii/node-3406-vergrth16-with-vk` landing the
`gosh-zk-snark-halo2-utils` dep tree on `tvm-sdk`'s `main`.

**Status (2026-05-18): bridge-side proposal landed, awaiting partner ack.** The
bridge has committed to a concrete byte layout and verified it
end-to-end against a real Circuit 1B (fallback attestation) proof. See
`crates/bridge-prover-orchestrator/src/halo2_tvm_bundle.rs` (the
`Halo2TvmBundle` wire format, 8-byte magic + version + transcript_kind
byte + length-prefixed `(config_json, vk_bytes, instances, proof)`
chunks) and the round-trip integration test
`crates/bridge-prover-orchestrator/tests/halo2_tvm_bundle_round_trip.rs`.

What's now known:

- **Q-WIRE-1**: bridge-side proof generator (`generate_fallback_proof`)
  uses the Blake2b SHPLONK transcript today; the bundle commits to it.
  Switching to Keccak would be a one-line change on both sides if the AN
  team prefers — the format reserves a `transcript_kind` discriminator
  byte. Bridge preference: keep Blake2b (matches `gosh-zk-snark-halo2-utils`).
- **Q-WIRE-3**: bundle uses strict 32-byte little-endian `Fr::to_repr()`,
  no u64 shortcut. `Fr::from_repr` rejects ≥ modulus inputs structurally.
- **Q-WIRE-4 / Option B**: bundle is self-describing — the VK envelope
  carries `BaseCircuitParams` JSON inline, so the consumer doesn't need
  any out-of-band schema. Verified for the `(k=20, advice=44, lookup=19,
  instance=1)` Circuit 1B shape; format is generic across K and
  `BaseCircuitParams`.
- **Q-WIRE-2** closed (2026-05-22): the KZG SRS is intentionally NOT in the
  bundle. The consumer (TVM opcode) is expected to load it once at VM
  startup keyed by `k`. The round-trip test sources it from the local
  shared `kzg_bn254_K.srs` cache.

Empirical sizes (real fixture, Circuit 1B, 10 signers, K=20):
bundle ≈ 21.2 KB (VK 6.1 KB + proof 14.8 KB + 4 × 32 B instances +
headers).

Partner action needed: explicit ack of the format, or a counter-proposal
on Q-WIRE-1 / Q-WIRE-4. Once acked, the TVM-side opcode wiring (Phase B
below) can take this format as the contract.

### Phase B — `ZKHALO2VERIFYWITHVK` opcode skeleton in tvm-sdk

Owner: bridge team (proposed PR on top of `serhii/node-3406-vergrth16-with-vk`).
Scope:

- `tvm_vm/src/executor/zk_halo2.rs::execute_halo2_proof_verification_with_vk` (alongside the existing handler).
- Per-VK LRU cache (`§3.3`).
- Gas constant `ZKHALO2VERIFY_WITH_VK_GAS_PRICE` (`§3.2`).
- Mnemonic registration: `ZKHALO2VERIFYWITHVK => 0xC7 0x4A` (next free byte after `0xC7 0x49 = ZKHALO2VERIFY`).
- Unit tests against a bridge-supplied test VK + a known-good proof generated by `deposit-prover/`.

A *partial* skeleton (handler + mnemonic + gas + docstring; no live cache; test marked `#[ignore]` pending real VK fixture) lives on branch `serhii/verhalo2shplonk-skeleton` of `tvm-sdk` for discussion. It does **not** depend on `serhii/node-3406-vergrth16-with-vk`'s halo2 deps — that wiring lands when Phase A finalises the on-wire format.

**Status (2026-05-22)**: Phase A is closed (§ above). Skeleton on
`serhii/verhalo2shplonk-skeleton` will be flipped into a Draft PR with
the locked decisions inlined into the cover letter. Real
implementation (replace `FatalError` stub with a live
`Proof::verify_with_vk` call against an LRU-cached `(VerifyingKey,
ParamsKZG)` pair) rebases onto `serhii/node-3406-vergrth16-with-vk`
after that branch merges — we don't want to maintain conflict
resolution against a 272-commit-wide moving target while it's still
under review. Tracked locally as task `tvm-sdk:zkhalo2vk-real-impl`.

### Phase C — `TVM-Solidity-Compiler` support

Owner: bridge team.
Scope: add `gosh.zkHalo2VerifyWithVK(...)` in the spirit of `gosh.vergrth16WithVK(...)` that we landed in the compiler PR for the Groth16 case (`AGENTS.md` Decision Log 2026-05-17).

### Phase D — AN-side `TokenBridge.finalizeDeposit(...)`

Owner: bridge team.
Scope: AN-side Solidity contract that calls the opcode and implements the nullifier + wrong-bridge checks (§3.4).

### Phase E — Bridge-side `deposit-prover/` adjustments

Owner: bridge team.
Scope:

- (Conditional on Q-WIRE-1) Switch the producer-side transcript from Keccak to Blake2b.
- (Conditional on Q-WIRE-4 Option B) Emit VK blobs in the agreed self-describing envelope.
- Add a small `tools/halo2_proof_to_tvm/` helper that converts the producer-side `(vk, instances, proof)` tuple into the TVM-cell wire form the opcode expects (mirroring our existing `gnark_to_ark/` for the Groth16 side, except much simpler).

### Phase F — end-to-end test

Owner: bridge team.
Scope: a CI test that drives `deposit-prover/` to produce a proof for a synthetic deposit, feeds it through the conversion tool, calls `TokenBridge.finalizeDeposit(...)` via `tvm-sdk` against an in-memory TVM, and asserts the user balance changes by the expected `amount`. Mirrors the AN→ETH Foundry-level E2E tests we have for `verifyBlock`.

---

## 6. Risk register

| ID | Risk | Mitigation |
| --- | --- | ---------- |
| H1 | Per-VK cache memory pressure with many concurrent bridge VKs | LRU eviction; cache size 8; advise contracts to pin a single VK per deployment. |
| H2 | Cold-call latency (~3 s for K=19 EvaluationDomain) on first transaction after a node restart | `warmup_halo2()` style background loader, extended to enumerate VKs declared in well-known system contracts. |
| H3 | `gosh-zk-snark-halo2-utils` API drift breaks existing VKs | Pin to commit SHA; Phase A agrees on a stable wire format; producer side carries a version byte in the VK envelope. |
| H4 | Subtle ambiguity in current `ZKHALO2VERIFY` u64-shortcut public-input parsing (see Q-WIRE-3) | New opcode uses strict 32-byte LE Fr. |
| H5 | Transcript mismatch silently rejects valid proofs (Blake2b vs Keccak) | Phase A locks transcript; producer-side test in `deposit-prover/` generates a fixture proof that round-trips through `gosh-zk-snark-halo2-utils::Proof::verify_with_vk` before any TVM-side work. |
| H6 | VK fixture lifecycle: which version of `deposit-prover`'s circuit ships with which deployment of `TokenBridge` | Document in `docs/verifying_eth_proof_on_an.md` §4 V4: VK is immutable per `TokenBridge` deployment; circuit upgrades require a contract upgrade. |

---

## 7. References

- `tvm-sdk` branch [`serhii/node-3406-vergrth16-with-vk`](https://github.com/tvmlabs/tvm-sdk/tree/serhii/node-3406-vergrth16-with-vk) — current home of `ZKHALO2VERIFY` + `ZKHALO2VERIFYWITHVK` (after PR #240). `VERGRTH16WITHVK` was retired on 2026-05-22 via PR #242.
- `tvm-sdk` PR [#240 — ZKHALO2VERIFYWITHVK (real implementation)](https://github.com/tvmlabs/tvm-sdk/pull/240) — handler, `Halo2TvmBundle` decoder, per-VK cache, tests.
- `tvm-sdk` PR [#242 — Remove VERGRTH16WITHVK + nightly fmt](https://github.com/tvmlabs/tvm-sdk/pull/242) — retirement of the per-VK Groth16 path now superseded by native Halo2.
- `docs/an_partner_integration_plan.md` Decision Log 2026-05-17 — Phase 4.3 rationale.
- `docs/audit_trail_v2.md` §1 R-8/R-9 — trust delta from Phase 4.3.
- `docs/verifying_eth_proof_on_an.md` — operational verification flow.
- `deposit-prover/README.md` — Halo2 deposit circuit overview.
- `gosh-zk-snark-halo2-utils` repository — `Proof::verify_with_vk` and `io::read_vk` reference impl.
