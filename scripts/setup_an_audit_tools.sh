#!/usr/bin/env bash
# Create .tools/ symlinks to sibling TVM-Solidity-Compiler and tvm-sdk release binaries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLS="$ROOT/.tools"
PARENT="$ROOT/.."

resolve_binary() {
  local env_var="$1"
  shift
  local candidate path
  if [[ -n "${!env_var:-}" ]]; then
    printf '%s' "${!env_var}"
    return 0
  fi
  for candidate in "$@"; do
    path="${candidate/\$PARENT/$PARENT}"
    if [[ -x "$path" ]]; then
      printf '%s' "$path"
      return 0
    fi
  done
  return 1
}

SOLD_SRC="$(resolve_binary TVM_SOLD_SRC \
  "$PARENT/TVM-Solidity-Compiler/target/release/sold" \
  "$PARENT/pruvendo-tvm-solidity-compiler/target/release/sold" \
  || true)"
DBG_SRC="$(resolve_binary TVM_DEBUGGER_SRC \
  "$PARENT/tvm-sdk/target/release/tvm-debugger" \
  "$PARENT/pruvendo-tvm-sdk/target/release/tvm-debugger" \
  || true)"
CLI_SRC="$(resolve_binary TVM_CLI_SRC \
  "$PARENT/tvm-sdk/target/release/tvm-cli" \
  "$PARENT/pruvendo-tvm-sdk/target/release/tvm-cli" \
  || true)"

mkdir -p "$TOOLS"

link() {
  local name="$1" src="$2"
  if [[ -z "$src" ]] || [[ ! -x "$src" ]]; then
    echo "WARN: $name not found — set TVM_${name^^//-/_}_SRC or build release in TVM-Solidity-Compiler / tvm-sdk" >&2
    if [[ -n "$src" ]]; then
      echo "       looked for: $src" >&2
    fi
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
