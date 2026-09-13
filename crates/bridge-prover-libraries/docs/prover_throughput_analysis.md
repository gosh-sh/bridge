# Prover throughput vs shellnet chain speed — user wait analysis

Reduced summary of the daemon-vs-chain rate analysis. Answers: **how long
must a user wait for `withdrawByProof` to succeed after firing a burn, as
a function of daemon uptime T?**

## Constants (hard)

- `THINNING_FACTOR_P = 8` — capped by `historical-layer-hashes-movement-checker-circuit`; cannot grow
- `HISTORY_PROOF_WINDOW W = 128`
- Bundle stride `S = W · P = 1024` seq_nos per bundle
- Shellnet chain rate `r ≈ 3 seq/s` (measured, sustained)
- Chain produces one bundle every `S / r ≈ 5.69 min` — **hard lower bound on any daemon τ**

## Measured per-bundle proving time τ (Deploy #11, 2026-08-18)

| Daemon                  | τ (per bundle) | Composition                           | ρ = τ·r/S | Regime                    |
|-------------------------|:--------------:|---------------------------------------|:---------:|---------------------------|
| `bridge-relayer-daemon` | **≈ 13.5 min** | C1a + C2 (~6.6 min) + SHPLONK (~6.9)  | **2.42**  | subcritical → gap grows   |
| `bridge-prover-daemon`  | **≈ 6.1 min**  | C1a + C2 only (no SHPLONK wrap)       | **1.07**  | near break-even           |

Both are subcritical (ρ > 1) → **there is no steady state**; the daemon
falls behind chain head at rate `(r − S/τ)` seq_no per second forever.

## Wait-time model

After daemon uptime `T`, a burn at chain head requires the daemon to
close the accumulated gap plus prove the covering bundle:

```
W(T) = T · (ρ − 1) · τ  +  τ         (approx; covering-bundle overhead)
W_relayer(T) ≈ 85 · T + 11.9 min
W_prover(T)  ≈ 4.3 · T + 8   min
```

Where `T` is in hours, `W` in minutes.

## Wait-time table (obligatory)

| Daemon uptime T | W_relayer (SHPLONK, prod) | W_prover-daemon (raw halo2)  |
|:---------------:|:-------------------------:|:----------------------------:|
| 0 (cold start)  | **12 min**                | **8 min**                    |
| 30 min          | 55 min                    | 10 min                       |
| 1 h             | 1 h 37 min                | 12 min                       |
| **2 h**         | **3 h 02 min**            | **17 min**                   |
| **3 h**         | **4 h 27 min**            | **21 min**                   |
| 6 h             | 8 h 42 min                | 34 min                       |
| 12 h            | 17 h 12 min               | 60 min                       |
| 24 h            | 34 h 12 min               | 1 h 52 min                   |
| 48 h            | 68 h 12 min               | 3 h 38 min                   |

## Target: ≤ 3 h constant user wait

Solving `W(T) ≤ 3 h`:

- **`bridge-relayer-daemon` (prod):** max uptime **≈ 1.98 h** before user
  wait crosses 3 h. **The prod daemon must be restarted every ~2 hours**
  to keep withdrawal SLO under 3 h — or its τ must drop below `S/r = 5.69 min`.
- **`bridge-prover-daemon` (raw halo2, verifier-local only):** max uptime
  **≈ 39.8 h** — comfortably meets 3 h SLO for ~1.5 days between restarts.

## Empirical cross-check

Deploy #11 run today: daemon cold-started, burn fired ~10 min in, covering
bundle 4 landed at ~65 min, withdrawByProof mined ~73 min from cold start.
Model predicts `W(0.17 h) ≈ 26 min` for the gap-close portion + 12 min
τ = ~38 min for the *covering* bundle only, but bundles 1–3 must land
first because storedLastSeenBlockSeqNo advances strictly. Serialized
4-bundle chain ≈ 4·13.5 = 54 min + submission ≈ 65 min — matches observed.

## Ops implications

- **SHPLONK wrap is ~50 % of τ_relayer.** Halving it drops ρ from 2.42
  to ~1.7 — still subcritical but doubles max-uptime-before-SLO-breach.
- **The only path to a stable-uptime prod daemon** is τ_relayer < 5.69 min
  (ρ < 1). Requires either raw-halo2 on-chain acceptance (removes SHPLONK)
  or a ~55 % SHPLONK speedup.
- **Short-term mitigation:** cron-restart prod daemon every 90 min from
  a fresh `--at-head` genesis (each restart resets T → 0 → W → 12 min).
  Requires a new bridge deploy per restart (storedLastSeenBlockSeqNo is
  monotonic on-chain). Not sustainable for mainnet.

## Why P cannot grow

`P = 8` is fossilized by the layer-hashes movement circuit's max-depth
constant. Raising P to 16 doubles S → halves ρ → makes both daemons
subcritical, but requires the movement circuit to be re-parameterized
and re-verified. Out of scope for the current release.
