# M5 — production VkBlob emit (step circuit) + opcode-faithful round-trip

**Status: GREEN.** The fused light-client `step` circuit's verifying key is now
serialized as a real **`ZKHALO2VERIFYWITHVK` VkBlob**, keyed on the **Hermez**
KZG ceremony (the one the opcode actually embeds), and self-verified through the
byte-for-byte read + SHPLONK path the AN node runs. Artifacts are checked into
[`../fixtures/step_vkblob/`](../fixtures/step_vkblob/).

## Ceremony correction: the opcode is Hermez, not the chain ceremony

The M4 note (`m4_real_proof_and_srs.md`) framed the size question around the AN
**chain** Powers-of-Tau ceiling (`2^19`). That framing is now **superseded** —
verified against the opcode source:

`tvm-sdk/tvm_vm/src/executor/zk_halo2_utils.rs::KZG_S_G2_BYTES` (the constant the
`ZKHALO2VERIFYWITHVK` handler rebuilds its verifier `ParamsKZG` from) has, **since
2026-07-23**, been the **Hermez Perpetual Powers of Tau** `[s]·G2`:

```
s_g2 head = 92 8f af b3 …   tail = … 5c 69 00     (Hermez)
```

not the chain ceremony (`c6 02 8a cf …`, which now only backs the legacy Dark DEX
`ZKHALO2VERIFY` via `DARK_DEX_KZG_S_G2_BYTES`). Two consequences:

1. **The `k ≤ 19` ceiling disappears.** Hermez publishes powers for **any
   `K ≤ 28`**, so the step is not forced to the wide `k = 19 / 132-col` operating
   point by a ceremony limit. (We still emit at `k = 19` here for continuity with
   the M4 measurements; a future width/depth retune to e.g. `k = 20 / 66-col` is
   now purely a gas/perf tradeoff, not a ceremony blocker.)
2. **The VK + proof MUST be keyed on Hermez.** A KZG verifier checks openings
   against `[s]·G2`, and the VK's fixed/permutation commitments are `Σ cᵢ·[τ^i]G1`
   — both are `τ`-specific. Keying on the chain ceremony (as the M4 seam table
   said) would make the opcode **reject** the proof. This emit uses Hermez.

## SRS provenance for this emit

No public `hermez-raw-19` download is available anymore (the S3 bucket now
returns `AccessDenied`). Instead we **tau-preservingly downsize** an existing
Hermez SRS on n14:

```
source : deposit-prover/data/kzg_params_20.srs   (Hermez k=20, s_g2 928fafb3…)
loaded : k=20  →  downsize(19)  →  k=19  (same τ, same s_g2)
check  : s_g2 head == 92 8f af b3  (asserted in the example)
```

`ParamsKZG::downsize` keeps `g2`, `s_g2`, and the first `2^19` `g1` powers (a
prefix of the k=20 powers) — identical to how the deposit path derives its k=18
SRS from a larger one. The resulting k=19 SRS is the exact Hermez `τ` the opcode
verifies against.

## Emitted artifacts (`fixtures/step_vkblob/`)

| File | Size | SHA-256 | Role (opcode operand) |
|------|------|---------|-----------------------|
| `step_vk_blob.bin` | 17 573 B | `0d712e8c…58dafb42` | `vk_cell` — Base **v1** VkBlob |
| `step_public_inputs.bin` | 256 B | `3cb26707…f25bace8` (8 × 32 LE Fr) | `public_inputs_cell` |
| `step_proof_blake2b.bin` | 40 128 B | `c32e1964…f32a91db` | `proof_cell` (raw SHPLONK) |
| `step_base_circuit_params.json` | 182 B | — | carried `BaseCircuitParams` (reference) |

VkBlob wire (byte-identical to `deposit-prover/src/halo2_tvm_bundle.rs` Base
path): magic `VKBLOB\0\0`, `version=1`, `transcript=0` (Blake2b), `shape=0`
(Base), 5 reserved, then `len‖BaseCircuitParams-JSON` and `len‖VerifyingKey::write(RawBytes)`.

Config: `k=19, num_advice_per_phase=[132], num_fixed=1,
num_lookup_advice_per_phase=[4,0,0], lookup_bits=18, num_instance_columns=1`.

8 public inputs (`STEP_INSTANCE_LEN`), canonical order:

```
[0] attested_slot            [1] finalized_slot
[2] finalized_beacon_root_hi [3] finalized_beacon_root_lo
[4] participation            [5] committee_commitment
[6] execution_block_hash_hi  [7] execution_block_hash_lo
```

## Opcode-faithful self-check (what the PASS proves)

The example (`examples/export_step_vk_blob.rs`) reparses the emitted blob and runs
the **exact** two-stage path the `ZKHALO2VERIFYWITHVK` Base branch runs
(`zk_halo2_with_vk.rs::read_base_vk` + `execute_zkhalo2_verify_with_vk`):

1. `VerifyingKey::read::<_, BaseCircuitBuilder<Fr>>(RawBytes, BaseCircuitParams)` —
   reconstructs the VK from the carried config (and asserts the bytes are
   byte-stable across read→write, i.e. `RawBytes` curve-membership-checked).
2. `verify_proof::<KZGCommitmentScheme, VerifierSHPLONK, Blake2bRead,
   SingleStrategy>(srs.verifier_params(), vk, …)` — because the SRS is the Hermez
   ceremony, `srs.verifier_params()` carries the same `(g[0], g2, s_g2)` the
   opcode rebuilds via `build_shared_kzg_params`, so a PASS here is a faithful
   proxy for on-AN acceptance.

Result (n14, k=19):

```
keygen_vk 171.7 s   keygen_pk 119.1 s   prove 179.1 s
Self-check: VK reads back via BaseCircuitBuilder<Fr> + carried BaseCircuitParams  ✅
Self-check: verify_proof (VerifierSHPLONK + Blake2b + Hermez verifier_params)     ✅ ACCEPTED
RESULT: PASS — production step VkBlob is opcode-readable
```

## Reproduce (n14)

```bash
cd /mnt/data/gosh/sergey-bridge/eth-light-client-prover
# Hermez k=19 via tau-preserving downsize of the on-box Hermez k=20 SRS:
STEP_SRS_PATH=/mnt/data/gosh/sergey-bridge/acki-nacki-bridge/deposit-prover/data/kzg_params_20.srs \
  cargo run --release --example export_step_vk_blob      # ~8 min, ~32 GB RSS
# → out/step_vkblob/{step_vk_blob.bin, step_public_inputs.bin,
#                    step_proof_blake2b.bin, step_base_circuit_params.json}
```

## End-to-end opcode acceptance (landed 2026-08-21)

The in-process self-check above is now backed by a **real `tvm_vm` opcode test**:
the emitted triple is checked into `tvm-sdk` and driven through the actual
`ZKHALO2VERIFYWITHVK` handler (`execute_zkhalo2_verify_with_vk`) via the
three-cell stack ABI — not a proxy, the same code path the AN node runs.

```
tvm-sdk/tvm_vm/halo2_test_data/step_light_client/
  step_vk_blob.bin  step_public_inputs.bin  step_proof.bin  (+ README, config json)
tvm-sdk/tvm_vm/src/tests/test_halo2_with_vk.rs
  round_trip_step_light_client_real_proof_returns_true    ← real proof → Ok(true)
  step_light_client_flipped_proof_byte_rejected_as_false  ← negative
  step_light_client_tweaked_instance_rejected_as_false    ← negative
  step_light_client_cache_reused_across_two_invocations   ← per-VK cache smoke
```

Run (n14 / local tvm-sdk on branch `pruvendo/deposit-chainid-12pi-hermez-srs`):

```bash
cd ../tvm-sdk && cargo test -p tvm_vm --features gosh --lib step_light_client
# test result: ok. 4 passed; 0 failed; 0 ignored (finished in 0.47s)
```

Refresh the fixture triple after any re-emit with
[`scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh`](../../scripts/sync_step_opcode_fixtures_to_tvm_sdk.sh)
(byte-identity with `fixtures/step_vkblob/` is enforced by the SHA-256 sidecars).

## Remaining M5/M6 seams

| Step | Notes |
|------|-------|
| ~~**Sync into `tvm-sdk` fixtures**~~ | ✅ Done (2026-08-21) — see "End-to-end opcode acceptance" above. |
| ~~**AN contract `EthBeaconLightClient`**~~ | ✅ Source landed (2026-08-21) — `acki-nacki/contracts/exchange/EthBeaconLightClient.sol` embeds `VK_BLOB`, `submitUpdate` verifies via `gosh.zkhalo2VerifyWithVK`, gates on the committee commitment, advances head, and pushes the finalized `execution_block_hash` into `USDCBridge._acceptedBlockHash` (new `acceptBlockHashFromLightClient` writer) so `finalizeDeposit` consults it. Compile/deploy on the operator Mac. See [`m5_eth_beacon_light_client.md`](m5_eth_beacon_light_client.md). |
| **rotate ↔ step join** | ✅ Contract wired (2026-08-21) — `EthBeaconLightClient.submitRotate(proof, publicInputs)` (3 PIs: current/next committee commitment + period) is the only permissionless writer of `_currentCommittee`; `disableOwnerRotation()` (one-way) drops the owner off the committee-advance path → trustless from the checkpoint. ⛔ `ROTATE_VK_BLOB` emit is **hardware-blocked**: the SSZ committee root (~1023 SHA-256, k≈26) exceeds a 125 GB host (n14 thrashes). See [`m5_rotate_join.md`](m5_rotate_join.md). |
| **Real-committee proof** | This emit's proof is over a synthetic-but-valid 512-committee (VK is witness-independent, so the VkBlob is production). M6 produces proofs over the real mainnet/testnet committee. |
| **Width/depth retune (optional)** | Hermez K≤28 removes the ceiling; `k=20/66-col` roughly halves the VK/opcode-gas at 2× rows. Pure tradeoff, not required. |
