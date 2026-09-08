#!/usr/bin/env bash
# R15 production proving on n14: bound Halo2 proofs (1A/1B/2) + aggregator .bin export.
#
# Usage (from dev machine):
#   ./scripts/n14_r15_proving_run.sh sync-and-start
#   ./scripts/n14_r15_proving_run.sh continue-bc      # skip Phase A if bound proofs exist
#   ./scripts/n14_r15_proving_run.sh continue-bc-nosync
#   ./scripts/n14_r15_proving_run.sh continue-c       # Phase C only (snarks cached)
#   ./scripts/n14_r15_proving_run.sh continue-c-nosync-eth06  # Phase C, keep Circuit 4
#   ./scripts/n14_r15_proving_run.sh status
#   ./scripts/n14_r15_proving_run.sh pull-artifacts
#
# Requires: ssh access to gosh@94.156.178.14:22488 (see .cursor/rules/external-ssh.mdc)
# cargo is *not* --locked: crates/bridge-snark-utils has no Cargo.lock in git;
# rsync --delete would also wipe a remotely generated lock.
#
# Circuit 2: Phase A uses --num-layers 3 --num-chain-steps 2 (keygen REF_*).
# The June 2026 layer PK (k=17, 20 advice) was built against a smaller Circuit 2
# (TREE_DEPTH=2). origin/main is TREE_DEPTH=8 and NUM_MERKLE_SIBLINGS=4; that PK
# overflows advice even at 3/2. If Circuit 2 panics NOT ENOUGH ADVICE COLUMNS,
# move aside halo2-prover/params/layer_{pk,vk,config}* (bridge/params/layer_*
# are symlinks into that dir) so ensure_keys re-keygens. Do NOT sync-and-start
# from a local circuits checkout on circuit4-single-final-root / halo2 main.
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
SOLC_LOCAL="${SOLC_LOCAL:-/usr/bin/solc}"

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
    --exclude target --exclude params --exclude proofs --exclude .git --exclude 'contracts/ethereum/out' \
    -e "ssh -p 22488" \
    "${LOCAL_ROOT}/" \
    "${N14}:${REMOTE_ROOT}/"
  for lock in \
    crates/bridge-snark-utils/Cargo.lock \
    crates/bridge-evm-aggregator/Cargo.lock; do
    if [[ -f "${LOCAL_ROOT}/${lock}" ]]; then
      echo "    ${lock}"
      rsync -avz -e "ssh -p 22488" \
        "${LOCAL_ROOT}/${lock}" \
        "${N14}:${REMOTE_ROOT}/${lock}"
    fi
  done
  if [[ -f "${SOLC_LOCAL}" ]]; then
    echo "    solc -> n14 ~/bin/solc (static linux fallback on n14 if rsync binary incompatible)"
    ${SSH} "mkdir -p ~/bin"
    if ! ${SSH} "~/bin/solc --version >/dev/null 2>&1"; then
      ${SSH} "curl -fsSL -o ~/bin/solc https://github.com/ethereum/solidity/releases/download/v0.8.19/solc-static-linux && chmod +x ~/bin/solc"
    fi
    ${SSH} "~/bin/solc --version | head -1"
  else
    echo "    WARN: ${SOLC_LOCAL} not found — Phase B spike/.bin export needs solc on n14"
  fi
}

# Remote job body. $1 = skip_phase_a (0|1), $2 = phase_c_only (0|1),
# $3 = skip_circuit4 (0|1) — ETH-06 keeps the committed Circuit 4 pair.
do_start() {
  local skip_phase_a="${1:-0}"
  local phase_c_only="${2:-0}"
  local skip_circuit4="${3:-0}"
  echo "==> starting proving job on n14 (log: ${REMOTE_LOG}, skip_phase_a=${skip_phase_a}, phase_c_only=${phase_c_only}, skip_circuit4=${skip_circuit4})"
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
    WITNESS=${REMOTE_ROOT}/proofs/bound/bound_witness.bin
    SNARK_DIR=${REMOTE_ROOT}/proofs/bound/poseidon-snark

    if [[ \"${phase_c_only}\" == \"1\" ]]; then
      echo \"--- Phase A/A2: SKIP (continue-c) ---\"
    elif [[ -f \"\${WITNESS}\" ]] && [[ \"${skip_phase_a}\" == \"1\" ]]; then
      echo \"--- Phase A: SKIP (found \${WITNESS}) ---\"
    else
      cd crates/bridge-snark-utils
      echo \"--- Phase A: cargo +nightly build export-bound-block-proofs ---\"
      cargo +nightly build --release --bin export-bound-block-proofs
      echo \"--- Phase A: export-bound-block-proofs ---\"
      # Shape matches LayerHashesKeyManager REF_NUM_LAYERS=3 / REF_NUM_PREV_CHAIN_STEPS=2.
      # Remote comments must not contain apostrophes (this body is single-quoted over SSH).
      cargo +nightly run --release --bin export-bound-block-proofs -- \
        --params-dir ../../params \
        --out-dir ../../proofs/bound \
        --num-layers 3 \
        --num-chain-steps 2
    fi

    if [[ \"${phase_c_only}\" != \"1\" ]]; then
    cd ${REMOTE_ROOT}/crates/bridge-evm-aggregator
    echo \"--- build export-inner-aggregator + export-spike-artifacts ---\"
    cargo +nightly build --release \
      --bin export-inner-aggregator \
      --bin export-spike-artifacts

    cd ${REMOTE_ROOT}/crates/bridge-snark-utils
    echo \"--- Phase A2: export-bound-poseidon-snarks (in-process Snark export) ---\"
    cargo +nightly build --release --bin export-bound-poseidon-snarks
    cargo +nightly run --release --bin export-bound-poseidon-snarks -- \
      --params-dir ../../params \
      --bound-dir ../../proofs/bound \
      --snark-dir ../../proofs/bound/poseidon-snark

    cd ${REMOTE_ROOT}/crates/bridge-evm-aggregator
    echo \"--- Phase B: export-spike-artifacts (sanity; needs solc 0.8.19) ---\"
    if command -v solc >/dev/null; then
      cargo +nightly run --release --bin export-spike-artifacts || echo \"WARN: spike export failed (non-fatal)\"
    else
      echo \"SKIP spike: solc missing\"
    fi
    fi

    cd ${REMOTE_ROOT}/crates/bridge-evm-aggregator
    if [[ \"${phase_c_only}\" == \"1\" ]]; then
      echo \"--- build export-inner-aggregator (continue-c) ---\"
      cargo +nightly build --release --bin export-inner-aggregator
    fi

    # Circuit 4 (bridge event / withdrawal) inner Poseidon snark. Fills the
    # BridgeWithdrawalAggregatorVerifier gap: export-bound-poseidon-snarks only
    # emits 1A/1B/2, so without this the export_one circuit4 line always SKIPs.
    # Runs in every mode (incl. continue-c) whenever circuit4.snark is missing.
    if [[ ! -f \"\${SNARK_DIR}/circuit4.snark\" ]]; then
      cd ${REMOTE_ROOT}/crates/bridge-snark-utils
      echo \"--- Phase A2b: export-c4-poseidon-snark ---\"
      cargo +nightly build --release --bin export-c4-poseidon-snark
      cargo +nightly run --release --bin export-c4-poseidon-snark -- \
        --params-dir ../../params \
        --snark-dir \${SNARK_DIR}
      cd ${REMOTE_ROOT}/crates/bridge-evm-aggregator
    fi

    OUT=${REMOTE_ROOT}/contracts/ethereum/verifiers
    mkdir -p \"\${OUT}\"

    export_one() {
      local snark=\"\$1\" name=\"\$2\"
      if [[ -f \"\${snark}\" ]]; then
        echo \"--- Phase C: export-inner-aggregator \${name} ---\"
        cargo +nightly run --release --bin export-inner-aggregator -- \
          --inner-snark \"\${snark}\" \
          --out-dir \"\${OUT}\" \
          --name \"\${name}\"
      else
        echo \"SKIP \${name}: missing \${snark}\"
      fi
    }

    export_one \"\${SNARK_DIR}/primary.snark\" PrimaryAggregatorVerifier
    export_one \"\${SNARK_DIR}/fallback.snark\" FallbackAggregatorVerifier
    export_one \"\${SNARK_DIR}/layer_hashes.snark\" LayerHashesAggregatorVerifier
    if [[ \"${skip_circuit4}\" == \"1\" ]]; then
      echo \"SKIP BridgeWithdrawalAggregatorVerifier: committed Circuit 4 pair kept\"
    else
      export_one \"\${SNARK_DIR}/circuit4.snark\" BridgeWithdrawalAggregatorVerifier
    fi

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
  mkdir -p "${LOCAL_ROOT}/crates/bridge-snark-utils/proofs/bound"
  mkdir -p "${LOCAL_ROOT}/contracts/ethereum/verifiers"
  mkdir -p "${LOCAL_ROOT}/logs"
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/proofs/bound/" \
    "${LOCAL_ROOT}/crates/bridge-snark-utils/proofs/bound/" || true
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/contracts/ethereum/verifiers/" \
    "${LOCAL_ROOT}/contracts/ethereum/verifiers/" || true
  rsync -avz -e "ssh -p 22488" \
    "${N14}:${REMOTE_ROOT}/logs/r15_proving_"*.log \
    "${LOCAL_ROOT}/logs/" 2>/dev/null || true
}

case "${cmd}" in
  sync-and-start) do_sync && do_start 0 0 ;;
  continue-bc) do_sync && do_start 1 0 ;;
  continue-c) do_sync && do_start 1 1 ;;
  sync) do_sync ;;
  start) do_start 0 0 ;;
  status) do_status ;;
  pull-artifacts) do_pull ;;
  continue-bc-nosync) do_start 1 0 ;;
  continue-c-nosync) do_start 1 1 0 ;;
  continue-c-nosync-eth06) do_start 1 1 1 ;;
  *) echo "usage: $0 {sync-and-start|continue-bc|continue-c|continue-bc-nosync|continue-c-nosync|continue-c-nosync-eth06|sync|start|status|pull-artifacts}"; exit 1 ;;
esac
