#!/usr/bin/env bash
# R15 production proving on n14: bound Halo2 proofs (1A/1B/2) + aggregator .bin export.
#
# Usage (from dev machine):
#   ./scripts/n14_r15_proving_run.sh sync-and-start
#   ./scripts/n14_r15_proving_run.sh status
#   ./scripts/n14_r15_proving_run.sh pull-artifacts
#
# Requires: ssh access to gosh@94.156.178.14:22488 (see .cursor/rules/external-ssh.mdc)
set -euo pipefail

N14="gosh@94.156.178.14"
SSH="ssh -o BatchMode=yes -p 22488 ${N14}"
REMOTE_ROOT="/mnt/data/gosh/sergey-bridge/acki-nacki-bridge"
REMOTE_LOG="${REMOTE_ROOT}/logs/r15_proving_$(date +%Y%m%d_%H%M%S).log"
LOCAL_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

cmd="${1:-sync-and-start}"

LOCAL_GOSH="$(cd "${LOCAL_ROOT}/.." && pwd)"
REMOTE_GOSH="$(dirname "${REMOTE_ROOT}")"

SIBLING_REPOS=(
  halo2-lib-zkevm-sha256-and-bls12-381
  gosh-halo2-crypto-lib
  acki-nacki-to-eth-bridge-halo2-circuits
  acki-nacki-to-eth-bridge-halo2-prover
  bk-set-stub
)

# Local path for private dense-balanced-tree (cargo git cache on dev machine).
DENSE_TREE_LOCAL="${DENSE_TREE_LOCAL:-${HOME}/.cargo/git/checkouts/dense-balanced-tree-62b64667ab66462f/0b600eb}"

do_sync() {
  echo "==> rsync sibling repos to n14"
  if [[ -d "${DENSE_TREE_LOCAL}" ]]; then
    echo "    dense-balanced-tree (from cargo cache)"
    rsync -avz --exclude target --exclude .git -e "ssh -p 22488" \
      "${DENSE_TREE_LOCAL}/" "${N14}:${REMOTE_GOSH}/dense-balanced-tree/"
  fi
  for repo in "${SIBLING_REPOS[@]}"; do
    if [[ -d "${LOCAL_GOSH}/${repo}" ]]; then
      echo "    ${repo}"
      rsync -avz \
        --exclude target --exclude params --exclude .git \
        -e "ssh -p 22488" \
        "${LOCAL_GOSH}/${repo}/" \
        "${N14}:${REMOTE_GOSH}/${repo}/"
    else
      echo "    SKIP ${repo} (not found locally)"
    fi
  done
  echo "==> rsync bridge repo to n14"
  rsync -avz --delete \
    --exclude target --exclude params --exclude .git --exclude 'contracts/ethereum/out' \
    -e "ssh -p 22488" \
    "${LOCAL_ROOT}/" \
    "${N14}:${REMOTE_ROOT}/"
  # Cargo.lock is gitignored but required on n14 (--locked) so patches win over stale git pins.
  for lock in \
    crates/bridge-prover-orchestrator/Cargo.lock \
    crates/bridge-evm-aggregator/Cargo.lock; do
    if [[ -f "${LOCAL_ROOT}/${lock}" ]]; then
      echo "    ${lock}"
      rsync -avz -e "ssh -p 22488" \
        "${LOCAL_ROOT}/${lock}" \
        "${N14}:${REMOTE_ROOT}/${lock}"
    fi
  done
}

do_start() {
  echo "==> starting proving job on n14 (log: ${REMOTE_LOG})"
  ${SSH} "mkdir -p ${REMOTE_ROOT}/logs ${REMOTE_ROOT}/params ${REMOTE_ROOT}/proofs"
  ${SSH} "nohup bash -lc '
    set -euo pipefail
    cd ${REMOTE_ROOT}
    exec > ${REMOTE_LOG} 2>&1
    echo \"=== R15 n14 proving started \$(date -Is) ===\"
    echo \"Host: \$(hostname)\"

    # --- Phase A: bound block proofs (1A + 1B + 2, Blake2b; ~3-30 min depending on keygen cache) ---
    cd crates/bridge-prover-orchestrator
    echo \"--- cargo build --release export-bound-block-proofs ---\"
    cargo build --release --locked --bin export-bound-block-proofs
    echo \"--- export-bound-block-proofs ---\"
    cargo run --release --locked --bin export-bound-block-proofs -- \
      --params-dir ../../params \
      --out-dir ../../proofs/bound

    # --- Phase B: aggregator crate (M2 spike sanity + export-inner-aggregator binary) ---
    cd ../bridge-evm-aggregator
    echo \"--- cargo build --release export-inner-aggregator export-spike-artifacts ---\"
    cargo build --release --locked --bin export-inner-aggregator --bin export-spike-artifacts
    echo \"--- export-spike-artifacts (sanity) ---\"
    cargo run --release --locked --bin export-spike-artifacts

    # --- Phase C: per-circuit .bin (requires inner Snark bincode — see logs) ---
    OUT=${REMOTE_ROOT}/contracts/ethereum/verifiers
    mkdir -p \"\${OUT}\"
    SNARK_DIR=${REMOTE_ROOT}/proofs/bound/poseidon-snark
    mkdir -p \"\${SNARK_DIR}\"

    export_one() {
      local snark=\"\$1\" name=\"\$2\" n=\"\$3\"
      if [[ -f \"\${snark}\" ]]; then
        echo \"--- export-inner-aggregator \${name} ---\"
        cargo run --release --locked --bin export-inner-aggregator -- \
          --inner-snark \"\${snark}\" \
          --out-dir \"\${OUT}\" \
          --name \"\${name}\" \
          --inner-instances \"\${n}\"
      else
        echo \"SKIP \${name}: missing \${snark} (run export-poseidon-inner-snarks first)\"
      fi
    }

    export_one \"\${SNARK_DIR}/primary.snark\" PrimaryAggregatorVerifier 4
    export_one \"\${SNARK_DIR}/fallback.snark\" FallbackAggregatorVerifier 4
    export_one \"\${SNARK_DIR}/layer_hashes.snark\" LayerHashesAggregatorVerifier 14
    export_one \"\${SNARK_DIR}/circuit4.snark\" BridgeWithdrawalAggregatorVerifier 10

    echo \"--- EIP-170 check ---\"
    cd ${REMOTE_ROOT}
    chmod +x scripts/check_eip170_verifier_bins.sh
    ./scripts/check_eip170_verifier_bins.sh contracts/ethereum/verifiers || true

    echo \"=== R15 n14 proving finished \$(date -Is) ===\"
  ' </dev/null >/dev/null 2>&1 & echo \"PID \$! log ${REMOTE_LOG}\""
}

do_status() {
  ${SSH} "ls -lt ${REMOTE_ROOT}/logs/r15_proving_*.log 2>/dev/null | head -3; echo '--- tail ---'; tail -30 \$(ls -t ${REMOTE_ROOT}/logs/r15_proving_*.log 2>/dev/null | head -1) 2>/dev/null || echo 'no log yet'"
}

do_pull() {
  echo "==> pull proofs + verifiers + logs"
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/proofs/bound/" \
    "${LOCAL_ROOT}/crates/bridge-prover-orchestrator/proofs/bound/" || true
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/contracts/ethereum/verifiers/" \
    "${LOCAL_ROOT}/contracts/ethereum/verifiers/" || true
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/logs/r15_proving_"*.log \
    "${LOCAL_ROOT}/logs/" 2>/dev/null || mkdir -p "${LOCAL_ROOT}/logs"
}

case "${cmd}" in
  sync-and-start) do_sync && do_start ;;
  sync) do_sync ;;
  start) do_start ;;
  status) do_status ;;
  pull-artifacts) do_pull ;;
  *) echo "usage: $0 {sync-and-start|sync|start|status|pull-artifacts}"; exit 1 ;;
esac
