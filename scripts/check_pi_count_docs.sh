#!/usr/bin/env bash
# TD-68 / DEP-PI-COUNT — fail canonical docs that still claim 11 deposit PI.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'EOF'
TD-68 — deposit public-input count docs gate (canonical = 12 PI).

Checks audit/PROJECT_FACTS.md, evm_an_deposit_e2e_runbook.md,
zk_halo2_an_side_design.md, audit/spec/an/AGENT_CONTEXT.md for stale
"11 PI" deposit claims without legacy/stale context.

Allowlist (not scanned): _archive/, USDCBridge_12pi_chainid_allowlist.patch,
audit overlay USDCBridge.sol (legacy parser documented in td_04).
EOF
  exit 0
fi

CANONICAL=(
  "audit/PROJECT_FACTS.md"
  "docs/operations/evm_an_deposit_e2e_runbook.md"
  "docs/zk/an-side/zk_halo2_an_side_design.md"
  "audit/spec/an/AGENT_CONTEXT.md"
)

# Extended regex (ERE) patterns — deposit-flow stale 11-PI claims.
PATTERNS=(
  'NUM_PUBLIC_INPUTS[[:space:]]*=[[:space:]]*11'
  '11[[:space:]]+public[[:space:]]+inputs?'
  'num_instance\(\)[[:space:]]*==[[:space:]]*vec!\[11\]'
  '11[[:space:]]*×[[:space:]]*32'
  '11[[:space:]]*x[[:space:]]*32'
  '11-PI'
  'Public inputs \(11\)'
)

# Same-line allowlist: historical / legacy context (case-insensitive).
ALLOW_RE='legacy|stale|superseded|historical|pre-12|obsolete|deprecated|reference of|kept as|TRACK-2|overlay|8-Fr|8 Fr|parser'

FACTS="${ROOT}/audit/PROJECT_FACTS.md"
if [[ ! -f "$FACTS" ]]; then
  echo "TD-68 FAIL: missing $FACTS" >&2
  exit 1
fi

deposit_row=$(grep -E '^\|[[:space:]]*Deposit[[:space:]]*\|' "$FACTS" | head -1 || true)
if [[ -z "$deposit_row" ]]; then
  echo "TD-68 FAIL: no Deposit row in PROJECT_FACTS.md" >&2
  exit 1
fi
if ! grep -qE '\*\*12\*\*' <<<"$deposit_row"; then
  echo "TD-68 FAIL: PROJECT_FACTS Deposit PI count is not **12**:" >&2
  echo "  $deposit_row" >&2
  exit 1
fi

fail=0
for rel in "${CANONICAL[@]}"; do
  path="${ROOT}/${rel}"
  if [[ ! -f "$path" ]]; then
    echo "TD-68 FAIL: missing canonical file $rel" >&2
    fail=1
    continue
  fi
  for pat in "${PATTERNS[@]}"; do
    while IFS= read -r hit; do
      [[ -z "$hit" ]] && continue
      line_no="${hit%%:*}"
      line_text="${hit#*:}"
      if grep -qiE "$ALLOW_RE" <<<"$line_text"; then
        continue
      fi
      echo "TD-68 FAIL: stale deposit PI=11 in ${rel}:${line_no}: ${line_text}" >&2
      fail=1
    done < <(grep -nE "$pat" "$path" || true)
  done
done

if [[ "$fail" -ne 0 ]]; then
  echo "TD-68: canonical deposit PI count must be 12 (see DEPOSIT_PUBLIC_INPUT_LAYOUT)" >&2
  exit 1
fi

echo "TD-68 OK: canonical docs agree on 12 deposit public inputs"
