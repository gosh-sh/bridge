#!/usr/bin/env bash
# Sync deposit_10proofs from in-tree deposit-prover into audit overlay.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/deposit-prover/fixtures/deposit_10proofs"
DEST="$ROOT/audit/spec/an/fixtures/deposit_10proofs"

if [[ ! -d "$SRC/proof_00" ]]; then
  echo "Error: $SRC not found — run deposit-prover fixture regen first" >&2
  exit 1
fi

mkdir -p "$DEST"
rsync -a --delete "$SRC/" "$DEST/"
echo "[OK] synced deposit_10proofs → $DEST"
