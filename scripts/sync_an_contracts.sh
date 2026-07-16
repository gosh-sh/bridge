#!/usr/bin/env bash
# Rsync AN bridge contracts from sibling acki-nacki into audit/spec/an-contracts/.
#
# Default: origin/dev @ git@github.com:gosh-sh/acki-nacki.git
# (history_cursor merged into dev). Override: ACKI_NACKI_BRANCH=other
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${ACKI_NACKI_ROOT:-$ROOT/../acki-nacki}"
DEST="$ROOT/audit/spec/an-contracts"
REMOTE="${ACKI_NACKI_REMOTE:-origin}"
BRANCH="${ACKI_NACKI_BRANCH:-dev}"

if [[ ! -d "$SRC/.git" ]]; then
  echo "Error: acki-nacki not found at $SRC" >&2
  exit 1
fi

checkout_branch() {
  local remote="$1" branch="$2"
  echo "Syncing $remote/$branch in $SRC ..."
  if ! git -C "$SRC" remote get-url "$remote" &>/dev/null; then
    echo "Error: remote '$remote' not configured in $SRC" >&2
    exit 1
  fi
  if git -C "$SRC" show-ref --verify --quiet "refs/heads/$branch"; then
    git -C "$SRC" fetch "$remote" "$branch" 2>/dev/null || true
    git -C "$SRC" checkout "$branch"
    git -C "$SRC" merge --ff-only "$remote/$branch" 2>/dev/null || true
    return 0
  fi
  if git -C "$SRC" show-ref --verify --quiet "refs/remotes/$remote/$branch"; then
    git -C "$SRC" fetch "$remote" "$branch" 2>/dev/null || true
    git -C "$SRC" checkout -B "$branch" "$remote/$branch"
    return 0
  fi
  echo "Fetching $branch from $remote ..."
  if git -C "$SRC" fetch "$remote" "$branch:$branch"; then
    git -C "$SRC" checkout "$branch"
    return 0
  fi
  echo "Error: branch '$branch' not found on $remote ($(git -C "$SRC" remote get-url "$remote"))" >&2
  exit 1
}

checkout_branch "$REMOTE" "$BRANCH"

# Bridge exchange stack (USDCBridge, DepositVoucher, deps)
synced=0
for sub in exchange token eccconfig; do
  if [[ -d "$SRC/contracts/$sub" ]]; then
    echo "Sync contracts/$sub ..."
    rsync -a --delete "$SRC/contracts/$sub/" "$DEST/$sub/"
    synced=$((synced + 1))
  fi
done

if [[ $synced -eq 0 ]]; then
  echo "Warn: no contracts/* subdirs synced — is $BRANCH the right branch?" >&2
  ls -la "$SRC/contracts/" 2>/dev/null | head -15 || true
  exit 1
fi

# Root makefile include if present
if [[ -f "$SRC/contracts/Makefile.inc" ]]; then
  cp "$SRC/contracts/Makefile.inc" "$DEST/"
fi

MANIFEST="$DEST/contracts_manifest.json"
cat > "$MANIFEST" <<'EOF'
{
  "contracts": [
    {"name": "USDCBridge", "source": "exchange"},
    {"name": "DepositVoucher", "source": "exchange"}
  ]
}
EOF

if [[ -d "$SRC/tests/exchange/fixtures/deposit_10proofs" ]]; then
  echo "Sync deposit_10proofs fixtures ..."
  mkdir -p "$ROOT/audit/spec/an/fixtures/deposit_10proofs"
  rsync -a "$SRC/tests/exchange/fixtures/deposit_10proofs/" \
    "$ROOT/audit/spec/an/fixtures/deposit_10proofs/"
fi

echo "Source: $REMOTE/$BRANCH ($(git -C "$SRC" rev-parse --short HEAD))"
echo "Updated $MANIFEST"
echo "Run: cd audit/spec/an-contracts && ./build.sh"
