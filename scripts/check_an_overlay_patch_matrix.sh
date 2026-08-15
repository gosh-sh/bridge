#!/usr/bin/env bash
# TD-04 — grep AN bridge Solidity for deposit security markers.
# Supports audit overlay USDCBridge (legacy patch) and upstream eccUSDCBridge
# (acki-nacki @ contracts/bridge: _trustedL1Bridge SET allowlist).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXCHANGE="${REPO_ROOT}/audit/spec/an-contracts/exchange"
if [[ -f "${EXCHANGE}/USDCBridge.sol" ]]; then
  OVERLAY="${EXCHANGE}/USDCBridge.sol"
elif [[ -f "${EXCHANGE}/eccUSDCBridge.sol" ]]; then
  OVERLAY="${EXCHANGE}/eccUSDCBridge.sol"
else
  echo "TD-04 overlay missing: no USDCBridge.sol / eccUSDCBridge.sol in ${EXCHANGE}" >&2
  exit 1
fi
VOUCHER="${EXCHANGE}/DepositVoucher.sol"

fail=0
check() {
  local label="$1"
  local pattern="$2"
  local file="${3:-$OVERLAY}"
  if grep -q "$pattern" "$file"; then
    echo "OK  $label"
  else
    echo "FAIL $label (pattern: $pattern)" >&2
    fail=1
  fi
}

check_either() {
  local label="$1"
  local p1="$2"
  local p2="$3"
  local file="${4:-$OVERLAY}"
  if grep -q "$p1" "$file" || grep -q "$p2" "$file"; then
    echo "OK  $label"
  else
    echo "FAIL $label (need $p1 or $p2)" >&2
    fail=1
  fi
}

echo "check_an_overlay_patch_matrix: $OVERLAY"
echo "  bridge branch: expects _trustedL1Bridge + setTrustedL1Bridge on eccUSDCBridge.sol"
echo "  legacy overlay: expects _expectedBridgeFr / anchors / mint-cap when USDCBridge.sol present"
check "12-PI parser (9 Fr read)" 'for (uint k = 0; k < 9; k++'
check "chainId at fr[4]" 'f.chainId      = fr\[4\]'
check "12-PI publicInputs doc" '12 × 32-byte LE Fr'

if grep -q '_trustedL1Bridge' "$OVERLAY"; then
  check "trusted L1 bridge SET (_trustedL1Bridge)" '_trustedL1Bridge'
  check "setTrustedL1Bridge" 'function setTrustedL1Bridge'
  check "chainId in voucher hash" 'abi.encode(depositId, contractAddr, dappId, chainId)' "$VOUCHER"
  echo "OK  contracts/bridge model — anchor/mint-cap/attest checks deferred (ops on bridge branch)"
else
  check_either "L1 bridge allowlist" '_expectedBridgeFr' 'setExpectedBridge'
  check "setExpectedBridge" 'function setExpectedBridge'
  check "_expectedAnDappId / ERR_WRONG_DAPP" '_expectedAnDappId'
  check "ERR_WRONG_DAPP 223" 'ERR_WRONG_DAPP'
  check "_acceptedBlockHash anchor" '_acceptedBlockHash'
  check "ERR_UNKNOWN_BLOCK 224" 'ERR_UNKNOWN_BLOCK'
  check "ERR_UNKNOWN_SOURCE 222" 'ERR_UNKNOWN_SOURCE'
  check "DEP-N-5 _depositIdentity" '_depositIdentity'
  if grep -q 'abi.encode(srcChainId, depositId, contractAddr, dappId)' "$VOUCHER"; then
    echo "OK  srcChainId in voucher hash"
  else
  check "srcChainId in voucher hash" 'abi.encode(srcChainId, depositId, contractAddr, dappId)' "$VOUCHER"
  fi
  check "_mintCapByChain" '_mintCapByChain'
  check "attestBlockHash M-of-N" 'function attestBlockHash'
fi

if [[ "$fail" -ne 0 ]]; then
  echo "EXIT: overlay patch matrix incomplete" >&2
  exit 1
fi

echo "OK — overlay patch matrix (TD-04 checklist)"
