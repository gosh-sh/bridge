#!/usr/bin/env bash
# Shallow-clone sibling repos into audit/vendors/ for local audit (gitignored).
# Prefer existing ../acki-nacki via symlink. ZK circuit trees are optional — clone on demand.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDORS="$ROOT/audit/vendors"
mkdir -p "$VENDORS"

clone_shallow() {
  local name="$1" url="$2" branch="${3:-}"
  local dest="$VENDORS/$name"
  if [[ -e "$dest" ]]; then
    echo "  skip $name (already present)"
    return 0
  fi
  echo "  clone $name ($url${branch:+ @ $branch}) ..."
  if [[ -n "$branch" ]]; then
    git clone --depth 1 --branch "$branch" "$url" "$dest"
  else
    git clone --depth 1 "$url" "$dest"
  fi
}

link_sibling() {
  local name="$1" rel="$2"
  local dest="$VENDORS/$name"
  local src="$ROOT/$rel"
  if [[ -e "$dest" ]]; then
    echo "  skip $name (already present)"
    return 0
  fi
  if [[ -d "$src/.git" ]]; then
    ln -sf "$src" "$dest"
    echo "  link $dest -> $src"
    return 0
  fi
  return 1
}

echo "Audit vendors → $VENDORS"

# Canonical AN contracts (required for sync_an_contracts if no ../acki-nacki)
if ! link_sibling acki-nacki ../acki-nacki; then
  clone_shallow acki-nacki git@github.com:gosh-sh/acki-nacki.git dev
fi

# Partner provers — optional; needed only for BC-AN-01 dual-proof PoC / withdraw regen
if [[ "${SETUP_AUDIT_VENDORS_PROVERS:-0}" == "1" ]]; then
  clone_shallow acki-nacki-to-eth-bridge-halo2-prover \
    git@github.com:gosh-sh/acki-nacki-to-eth-bridge-halo2-prover.git
  clone_shallow acki-nacki-to-eth-bridge-halo2-circuits \
    git@github.com:gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits.git
else
  echo "  (prover/circuit repos skipped — SETUP_AUDIT_VENDORS_PROVERS=1 to clone)"
fi

# Toolchain sources — optional; .tools/ symlinks prefer ../ siblings
if [[ "${SETUP_AUDIT_VENDORS_TOOLCHAIN:-0}" == "1" ]]; then
  if ! link_sibling tvm-sdk ../tvm-sdk; then
    clone_shallow tvm-sdk git@github.com:tvmlabs/tvm-sdk.git \
      full_dex_and_bridge_test_with_final_halo2_circuit
  fi
  if ! link_sibling TVM-Solidity-Compiler ../TVM-Solidity-Compiler; then
    clone_shallow TVM-Solidity-Compiler \
      git@github.com:ackinacki-org/TVM-Solidity-Compiler.git
  fi
fi

echo "{\"generated_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "$VENDORS/manifest.json"

echo "Done. See audit/vendors/README.md"
