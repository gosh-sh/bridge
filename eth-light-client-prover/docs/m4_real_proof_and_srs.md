# M4 — real proof of the fused step + SRS / verification-path analysis

**Status: GREEN.** The fused light-client `step` circuit produces a **real SHPLONK
proof** (not just MockProver), and — the pivotal finding — it can be configured at
**k = 19**, exactly the Acki Nacki chain Powers-of-Tau ceiling. So the AN-side
`ZKHALO2VERIFYWITHVK` opcode can consume the step proof **directly, with no
Groth16 wrapper or recursive aggregation**.

## Why k mattered (and why the panic was premature)

The AN on-chain Halo2 verifier (`ZKHALO2VERIFYWITHVK`) embeds a single group
element, `[s]·G2`, from AN's own decentralized ceremony (BN254; see AGENTS.md
"KZG trusted setup provenance"). A KZG/SHPLONK **verifier** only needs `[s]·G2`
(plus `G1`, `G2` generators and the roots of unity, which are computable) — it is
**independent of circuit size**. The **prover**, however, needs `2^k` powers of
that *same* `τ`. The ceremony published `2^19` powers (`params/kzg_bn254_19.srs`).

Therefore the real question is not "is the circuit big?" but:

> **Can the fused step be configured at `k ≤ 19`?** If yes, the prover has enough
> chain-`τ` powers and the opcode verifies it as-is. If no, we'd need a
> constant-size wrapper (Groth16, à la Telepathy) or a bigger ceremony.

## Shape probe (`step_circuit_shape`, n14, witness-gen only, lookup_bits = 18)

```
total_advice        = 68_967_295 cells   (single phase)
total_fixed         = 619
total_lookup_advice =  2_062_869 cells
```

halo2-base auto-columns for each candidate `k` (range table `2^18` fits `2^19`):

| k  | advice cols | lookup-advice cols | prover SRS = 2^k | fits chain (≤2^19)? |
|----|-------------|--------------------|------------------|---------------------|
| 19 | **132**     | 4                  | 2^19             | ✅ **yes**          |
| 20 | 66          | 2                  | 2^20             | no                  |
| 21 | 33          | 1                  | 2^21             | no                  |
| 22 | 17          | 1                  | 2^22             | no                  |
| 23 | 9           | 1                  | 2^23             | no                  |

The cell count is (roughly) `k`-independent; smaller `k` is bought with **more
columns**. At `k = 19`, 132 advice columns absorb the ~69 M cells within `2^19`
rows — wide, but a perfectly valid single-phase circuit that fits the ceremony.

## Real prover round-trip (`step_real_proof_k19`, n14)

Full SHPLONK keygen → prove → verify on the **real fused witness** (real mainnet
headers + real finality/execution branches + valid 512-committee), 8 public
inputs wired to the instance column. Fresh test SRS via `gen_srs(19)` (provability
and sizes are `τ`-independent; chain-`τ` only matters for actual opcode
consumption in M5/M6).

```
config: BaseCircuitParams { k: 19, num_advice_per_phase: [132], num_fixed: 1,
        num_lookup_advice_per_phase: [4,0,0], lookup_bits: Some(18),
        num_instance_columns: 1 }

keygen_vk   176 s
keygen_pk   127 s
prove       158 s
verify       20 ms
--------------------------------
VK          17_418 B
proof       40_128 B
instances   8
peak RSS   ~41.5 GB     wall (whole test) 8:24
```

Verification is **20 ms** — as expected, constant-ish and size-cheap, exactly what
the on-chain/on-AN verifier does.

## What this means for the AN-side consumption

- **No wrapper required.** Unlike the earlier assumption, the light-client step
  does **not** need a Groth16 or snark-verifier aggregation layer to reach AN. A
  `k = 19` VkBlob (Base shape, `circuit_shape = 0` — `BaseCircuitBuilder`) keyed
  on the chain ceremony is directly verifiable by `ZKHALO2VERIFYWITHVK`.
- **8-PI public statement** (`STEP_INSTANCE_LEN`): `[attested_slot,
  finalized_slot, finalized_beacon_root_hi/lo, participation,
  committee_commitment, execution_block_hash_hi/lo]`.
- **Size vs deposit.** The deposit VkBlob is ~3.6 KB (k=18, 11 PI, RLC, ~narrow).
  The step VK is ~17 KB because of the 132-column width; the proof is ~40 KB.
  Both are well within cell/opcode limits (there is no EIP-170 analogue on AN),
  but the wider VK means more per-proof commitments → higher opcode gas. If gas
  becomes a concern, trading width for depth (e.g. `k = 20`, 66 cols) is possible
  **only if the ceremony is extended to `2^20`**; at the current `2^19` ceiling,
  `k = 19`/132-col is the operating point.

## Seam → M5

> **Correction (M5):** the "chain ceremony" framing below is **superseded**. The
> `ZKHALO2VERIFYWITHVK` opcode's `KZG_S_G2_BYTES` has been **Hermez** since
> 2026-07-23 (valid K≤28, so there is no `2^19` ceiling), and the production
> VkBlob is keyed on Hermez — see [`m5_vkblob.md`](m5_vkblob.md). This is DONE.

| Step | Notes |
|------|-------|
| **Emit the production VkBlob** | ✅ DONE (M5) — keyed on **Hermez** k=19 (not chain), Base v1 `circuit_shape=0`, self-verified through the opcode read+SHPLONK path. See `m5_vkblob.md` + `fixtures/step_vkblob/`. |
| **rotate ↔ step join** | The relayer feeds the current period's rotate-anchored committee commitment as the step's expected `instances[5]`; equality is what ties the aggregated committee to the chain `state_root`. |
| **tvm-sdk fixture + AN contract** | Sync the VkBlob/proof/PI into `tvm-sdk` for an end-to-end opcode test, and embed the VkBlob into the `EthBeaconLightClient` AN contract. |

## Reproduce (n14)

```bash
cd /mnt/data/gosh/sergey-bridge/eth-light-client-prover
cargo test --test step_mock_prover step_circuit_shape   -- --ignored --nocapture  # shape, ~66 s, 9 GB
cargo test --test step_mock_prover step_real_proof_k19   -- --ignored --nocapture  # real proof, ~8.5 min, 41 GB
```
