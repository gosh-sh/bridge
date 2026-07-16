#!/usr/bin/env bash
# Create .tools/ symlinks to sibling TVM-Solidity-Compiler and tvm-sdk release binaries.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOOLS="$ROOT/.tools"
SOLD_SRC="$ROOT/../TVM-Solidity-Compiler/target/release/sold"
DBG_SRC="$ROOT/../tvm-sdk/target/release/tvm-debugger"
CLI_SRC="$ROOT/../tvm-sdk/target/release/tvm-cli"

mkdir -p "$TOOLS"

link() {
  local name="$1" src="$2"
  if [[ ! -x "$src" ]]; then
    echo "WARN: $src not found — build with 'cargo build --release' in $(dirname "$src")/.." >&2
    return 1
  fi
  ln -sf "$src" "$TOOLS/$name"
  echo "  $TOOLS/$name -> $src"
}

echo "Linking AN audit toolchain into $TOOLS/"
link sold "$SOLD_SRC"
link tvm-debugger "$DBG_SRC"
link tvm-cli "$CLI_SRC"

# an-contracts workspace tools symlink
AC="$ROOT/audit/spec/an-contracts/tools"
mkdir -p "$(dirname "$AC")"
if [[ ! -e "$AC" ]]; then
  ln -sf ../../../.tools "$AC"
  echo "  $AC -> ../../../.tools"
fi

echo "Done."
