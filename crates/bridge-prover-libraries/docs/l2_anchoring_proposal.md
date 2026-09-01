# L2 anchoring mode — proposal

Short proposal to add an **L2 anchoring** mode to `bridge-prover-lib`,
`bridge-prover-daemon`, and `bridge-relayer-daemon`. Solves the
subcritical throughput regime documented in
[`prover_throughput_analysis.md`](prover_throughput_analysis.md).

## Idea

Today the daemon proves one bundle per L1 boundary — every `W·P = 1024`
seq_no. Chain produces a bundle every 5.7 min; prover needs ~13.5 min per
bundle (SHPLONK-wrapped). Gap grows linearly forever.

**Proposal:** prove one bundle per L2 key block — every `W² = 16 384`
seq_no. Circuit 2 walks a single horizontal hop at level 2 (`num_layers=2,
chain_steps=1`) covering the full L2 stride in one shot.

## Wait-time table

| Mode | Bundle stride S | Chain-time S/r | τ (prove) | ρ = τ·r/S | Regime | Avg user wait | Ceiling |
|:--|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| **L1 (now)** | W·P = 1 024 seq | 5.7 min | 13.5 min | **2.42** | subcritical | grows w/T | ∞ |
| **L2** | **W² = 16 384 seq** | **91 min** | ~13.5 min | **0.15** | supercritical | **~59 min** | **~105 min** |

`r = 3 seq/s` shellnet chain rate. L2 chain time = 16 384 / 3 = 91 min per
bundle. Avg user wait = ½·S/r + τ; ceiling = S/r + τ.

## Why it works

- Circuit 2 already exposes `num_layers ∈ [1,10]`, `layer_hash_frs[0..9]`,
  and `chain_steps ∈ [1,11]` as public instances (movement-checker
  README §Public Instances). Prove time is flat across all
  `(num_layers, chain_steps)` combinations per its real-prover sweep.
- Circuit 1a (attestation-bls-checker) is level-agnostic — it only
  attests `(block_id, block_seq_no)`. No layer_hashes involvement.
- Circuit 4 (event proof) already supports anchoring at L2+.
- `AckiNackiBridge.verifyBlock` is stride-blind — accepts a 16 384 jump
  in `storedLastSeenBlockSeqNo` the same as a 1 024 jump.

**No circuit change, no VK change, no on-chain verifier redeploy.**

## Wins

- **Constant user wait ~1 h avg, ~2 h ceiling** — independent of daemon uptime.
- **16× cheaper Sepolia gas** — `verifyBlock` frequency drops from
  10.5/h to 0.66/h.
- **No cron-restart discipline** for prod daemon.
- **6.7× headroom** on ρ (τ could grow to ~90 min before subcritical).

## Cost

Additive feature, L1 stays as default. Work items:

1. `bridge-prover-lib`: `mode: L1 | L2` selector; witness builders in
   `chain_proof_builder` / `real_chain_builder` / `layer_prover` set
   `num_layers=2, chain_steps=1` and pick L2-anchor block IDs.
2. `bridge-prover-daemon` / `bridge-relayer-daemon`: `--anchor-level`
   CLI flag; bundle scheduling on L2 boundaries; prover-state schema
   carries the level.
3. `compute_bridge_anchors`: `--level 2` emits L2-aligned genesis seed.
4. Genesis env: `BRIDGE_BOOTSTRAP_SEQNO` and
   `GENESIS_LAST_SEEN_BLOCK_SEQNO` at an L2 boundary.
5. Smoke-test the event-anchoring path with an L2-covered burn.

## Cold-start trade-off

First burn after cold-start waits up to `S₂/r + τ ≈ 105 min` (vs ~19 min
at L1). This is the price of predictability — L1 is fast cold then drifts
unbounded; L2 is slower cold then holds forever. For a "fire burn and
withdraw within 2 h reliably" SLO, L2 wins.
