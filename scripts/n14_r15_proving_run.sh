#!/usr/bin/env bash
# R15 production proving on n14: bound Halo2 proofs (1A/1B/2) + aggregator .bin export.
#
# Usage (from dev machine):
#   ./scripts/n14_r15_proving_run.sh sync-and-start
#   ./scripts/n14_r15_proving_run.sh continue-bc      # skip Phase A if bound proofs exist
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

DENSE_TREE_LOCAL="${DENSE_TREE_LOCAL:-${HOME}/.cargo/git/checkouts/dense-balanced-tree-62b64667ab66462f/0b600eb}"
SOLC_LOCAL="${SOLC_LOCAL:-${HOME}/.local/bin/solc}"

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
  if [[ -f "${SOLC_LOCAL}" ]]; then
    echo "    solc -> n14 ~/bin/solc"
    ${SSH} "mkdir -p ~/bin"
    rsync -avz -e "ssh -p 22488" "${SOLC_LOCAL}" "${N14}:~/bin/solc"
    ${SSH} "chmod +x ~/bin/solc"
  else
    echo "    WARN: ${SOLC_LOCAL} not found — Phase B spike/.bin export needs solc on n14"
  fi
}

# Remote job body. $1 = skip_phase_a (0|1)
do_start() {
  local skip_phase_a="${1:-0}"
  echo "==> starting proving job on n14 (log: ${REMOTE_LOG}, skip_phase_a=${skip_phase_a})"
  ${SSH} "mkdir -p ${REMOTE_ROOT}/logs ${REMOTE_ROOT}/params ${REMOTE_ROOT}/proofs ~/bin"
  ${SSH} "nohup bash -lc '
    set -euo pipefail
    export PATH=\"\$HOME/bin:\$PATH\"
    cd ${REMOTE_ROOT}
    exec > ${REMOTE_LOG} 2>&1
    echo \"=== R15 n14 proving started \$(date -Is) ===\"
    echo \"Host: \$(hostname)\"
    command -v solc && solc --version | head -1 || echo \"WARN: solc not in PATH\"

    BOUND=${REMOTE_ROOT}/proofs/bound/bound_scenario.json
    SNARK_DIR=${REMOTE_ROOT}/proofs/bound/poseidon-snark
    SNARK_EXPORTER=${REMOTE_ROOT}/crates/bridge-evm-aggregator/target/release/export-halo2-poseidon-snark

    if [[ \"${skip_phase_a}\" == \"0\" ]] || [[ ! -f \"\${BOUND}\" ]]; then
      cd crates/bridge-prover-orchestrator
      echo \"--- Phase A: cargo +nightly build export-bound-block-proofs ---\"
      cargo +nightly build --release --locked --bin export-bound-block-proofs
      echo \"--- Phase A: export-bound-block-proofs ---\"
      cargo +nightly run --release --locked --bin export-bound-block-proofs -- \
        --params-dir ../../params \
        --out-dir ../../proofs/bound
    else
      echo \"--- Phase A: SKIP (found \${BOUND}) ---\"
    fi

    cd ${REMOTE_ROOT}/crates/bridge-evm-aggregator
    echo \"--- build export-halo2-poseidon-snark + export-inner-aggregator ---\"
    cargo +nightly build --release --locked \
      --bin export-halo2-poseidon-snark \
      --bin export-inner-aggregator \
      --bin export-spike-artifacts

    cd ${REMOTE_ROOT}/crates/bridge-prover-orchestrator
    echo \"--- Phase A2: export-bound-poseidon-snarks ---\"
    cargo +nightly build --release --locked --bin export-bound-poseidon-snarks
    cargo +nightly run --release --locked --bin export-bound-poseidon-snarks -- \
      --params-dir ../../params \
      --bound-dir ../../proofs/bound \
      --snark-dir ../../proofs/bound/poseidon-snark \
      --snark-exporter \"\${SNARK_EXPORTER}\"

    cd ${REMOTE_ROOT}/crates/bridge-evm-aggregator
    echo \"--- Phase B: export-spike-artifacts (sanity; needs solc) ---\"
    if command -v solc >/dev/null; then
      cargo +nightly run --release --locked --bin export-spike-artifacts
    else
      echo \"SKIP spike: solc missing\"
    fi

    OUT=${REMOTE_ROOT}/contracts/ethereum/verifiers
    mkdir -p \"\${OUT}\"

    export_one() {
      local snark=\"\$1\" name=\"\$2\" n=\"\$3\"
      if [[ -f \"\${snark}\" ]]; then
        echo \"--- Phase C: export-inner-aggregator \${name} ---\"
        cargo +nightly run --release --locked --bin export-inner-aggregator -- \
          --inner-snark \"\${snark}\" \
          --out-dir \"\${OUT}\" \
          --name \"\${name}\" \
          --inner-instances \"\${n}\"
      else
        echo \"SKIP \${name}: missing \${snark}\"
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
  mkdir -p "${LOCAL_ROOT}/crates/bridge-prover-orchestrator/proofs/bound"
  mkdir -p "${LOCAL_ROOT}/logs"
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/proofs/bound/" \
    "${LOCAL_ROOT}/crates/bridge-prover-orchestrator/proofs/bound/" || true
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/contracts/ethereum/verifiers/" \
    "${LOCAL_ROOT}/contracts/ethereum/verifiers/" || true
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/logs/r15_proving_"*.log \
    "${LOCAL_ROOT}/logs/" 2>/dev/null || true
}

case "${cmd}" in
  sync-and-start) do_sync && do_start 0 ;;
  continue-bc) do_sync && do_start 1 ;;
  sync) do_sync ;;
  start) do_start 0 ;;
  status) do_status ;;
  pull-artifacts) do_pull ;;
  *) echo "usage: $0 {sync-and-start|continue-bc|sync|start|status|pull-artifacts}"; exit 1 ;;
esac
