#!/usr/bin/env bash
#
# Parallel-throughput sweep for `primary_real_prover_parallel` on n14
# (48 cores / 128 GB). Designed to run unattended under tmux.
#
# Usage on the remote host:
#   cd ~/bridge/crates/bridge-circuits/attestation-bls-checker-circuit
#   chmod +x bench_parallel.sh
#   tmux new -d -s sweep './bench_parallel.sh'
#   tmux set-option -t sweep remain-on-exit on
#   tmux attach -t sweep    # detach with Ctrl-b d, can reconnect any time
#
# Outputs:
#   logs/parallel_sweep_<TS>/sweep.log                            — combined stdout+stderr
#   params/primary_real_prover_parallel_ms{MS}_p{P}.log           — per-case structured log
#
# Plan (per ms ∈ {300, 1000, 2000}):
#   1. CALIBRATION  → --parallelism 1 --baseline --warmup
#      Gives T_solo (single-proof time), C_solo (cores a lone proof uses),
#      RSS_solo (RAM one proof needs). These anchor everything.
#   2. PARALLEL SWEEP → --parallelism N (no --baseline)
#      Wall time of N concurrent proofs. Compare to N × T_solo from
#      calibration for the real speedup ratio.
#
# Why no --baseline in the sweep phase: at high N, the sequential pass would
# burn 16 × T_solo before the parallel test even starts. Calibration at p=1
# already gives T_solo, so the sequential reference is implicit.

set -u
cd "$(dirname "$0")"

# Binary is built into the workspace target/ at the parent of this crate.
BIN=../target/release/primary_real_prover_parallel

if [ ! -x "$BIN" ]; then
    echo "Binary not found: $BIN" >&2
    echo "Rebuild first:" >&2
    echo "  cd ~/bridge/crates/bridge-circuits && \\" >&2
    echo "    cargo build --release -p attestation-bls-checker-circuit \\" >&2
    echo "      --features bench-bin --bin primary_real_prover_parallel" >&2
    exit 1
fi

RUN=parallel_sweep_$(date +%Y%m%d_%H%M%S)
LOG_DIR=logs/$RUN
mkdir -p "$LOG_DIR"
# Mirror everything (stdout + stderr) into sweep.log via process substitution.
# This catches crashes, OOM glibc messages, panic backtraces — none of which
# the in-binary log captures.
exec > >(tee -a "$LOG_DIR/sweep.log") 2>&1

echo "=== sweep start $(date -Is) ==="
echo "binary:  $(realpath "$BIN" 2>/dev/null || echo "$BIN")"
echo "logs in: $(realpath "$LOG_DIR" 2>/dev/null || echo "$LOG_DIR")"
echo "host:    $(uname -a)"
echo "cores:   $(nproc 2>/dev/null || echo '?')"
echo "RAM:     $(free -h 2>/dev/null | awk '/^Mem/{print $2}')"
echo

# Run one case, never abort the sweep on a single failure (we want all the
# data we can get; an OOM at p=16 should not block the smaller-N rows).
case_run() {
    local MS=$1; shift
    local P=$1; shift
    local extra="$*"
    echo "######### ms=$MS  p=$P  ${extra:-(plain)}  -- $(date -Is)"
    if "$BIN" --max-signers "$MS" --parallelism "$P" $extra; then
        echo "######### ok"
    else
        rc=$?
        echo "!!! run failed: ms=$MS p=$P (exit $rc) — continuing"
    fi
    echo
}

for MS in 300 1000 2000; do
    echo
    echo "================================================================"
    echo "==== max_signers = $MS"
    echo "================================================================"

    # ── Calibration: T_solo / C_solo / RSS_solo at this ms ──────────
    # Order: warmup (1 discarded proof, primes caches), sequential (1 proof
    # recorded), parallel (1 worker). Cheap baseline anchor.
    case_run "$MS" 1 --baseline --warmup

    # ── Parallelism sweep ───────────────────────────────────────────
    # Chosen around the cost-model prediction N* = H_cores / C_solo.
    # If calibration shows a very different C_solo, the user can re-run
    # this script with different SWEEP values per ms.
    case "$MS" in
        300)  SWEEP="2 4 8 16" ;;   # N* ≈ 48/6  = 8 (predicted); bracket it
        1000) SWEEP="2 3 4 6"  ;;   # N* ≈ 48/15 ≈ 3
        2000) SWEEP="2"        ;;   # N* ≈ 48/30 ≈ 1.6; verify parallel ≈ flat
    esac

    for P in $SWEEP; do
        case_run "$MS" "$P"
    done
done

echo "=== sweep end $(date -Is) ==="
echo
echo "Per-case logs:    params/primary_real_prover_parallel_ms{MS}_p{P}.log"
echo "Combined sweep:   $LOG_DIR/sweep.log"
