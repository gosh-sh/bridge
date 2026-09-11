#!/usr/bin/env bash
# Fails if the fixture path cannot work on this machine. Run before
# scripts/deploy_msig_and_mint.sh; also runnable in CI.
set -euo pipefail

# This must pick the binary the fixture will pick. Disagreeing with
# `helper/common.py`'s `_resolve_tvm_cli` is worse than not checking at
# all: the check advertises whether the fixture can run, and then the
# fixture goes and resolves its own.
#
# `command -v tvm-cli` answers with the FIRST match on PATH and nothing
# else, which is what this used to do. The resolver deliberately tries
# every match, because `deploy_msig_and_mint.py` prepends `python/bin`
# and the binary committed there may be built for another OS/arch — an
# early entry that cannot run, with a working system install behind it.
# So this check failed on precisely the arrangement the fixture supports.
# Hardcoded, NOT `${COMPILER_DIR:-…}`, because the resolver hardcodes it
# too (`helper/common.py`: `COMPILER_DIR = "./contracts/compiler"`, no
# env read). Honouring an override here would let this check probe a
# candidate the fixture never tries — the same disagreement the rest of
# this file exists to remove, reintroduced by a convenience.
#
# The directory does not exist in this repository, so this candidate
# never runs. It is listed because the resolver lists it: leaving it out
# would make the two disagree about what was tried, which is what the
# FAIL message below reports.
COMPILER_DIR="./contracts/compiler"

# `timeout` is coreutils and not present everywhere (macOS). Use it when
# it is there; the stdin redirect below is the half that matters and
# costs nothing.
TIMEOUT="$(command -v timeout || true)"

# Ask an unknown executable for its version, bounded and without handing
# it our stdin — a candidate that reads stdin, or sits on a stale network
# mount, would otherwise block here forever. Mirrors the resolver's
# `timeout=5, stdin=DEVNULL`.
probe() {
  if [ -n "$TIMEOUT" ]; then
    "$TIMEOUT" 5 "$1" version >/dev/null 2>&1 </dev/null
  else
    "$1" version >/dev/null 2>&1 </dev/null
  fi
}

# The resolver's candidate list, in its order. `CLI_NAME` wins outright
# and brings no fallback with it — the resolver returns it without
# looking at anything else. It is still probed here, and that is not a
# divergence: the resolver will USE it whether or not it runs, so an
# explicit name that cannot run is exactly what this check exists to
# report.
list_candidates() {
  if [ -n "${CLI_NAME:-}" ]; then
    printf '%s\n' "$CLI_NAME"
    return
  fi
  printf '%s' "${PATH:-}" | tr ':' '\n' | while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    if [ -f "$entry/tvm-cli" ] && [ -x "$entry/tvm-cli" ]; then
      printf '%s\n' "$entry/tvm-cli"
    fi
  done
  printf '%s\n' "$COMPILER_DIR/tvm-cli"
}

cli=""
tried=""
while IFS= read -r cand; do
  [ -n "$cand" ] || continue
  case "$tried" in
    *"[$cand]"*) continue ;;  # the resolver dedupes too
  esac
  tried="$tried[$cand]"
  if probe "$cand"; then
    cli="$cand"
    break
  fi
  # Only noise when there is somewhere else to go; an explicit
  # CLI_NAME has no next candidate and gets the FAIL below instead.
  [ -n "${CLI_NAME:-}" ] || echo "  skipping $cand: does not answer \`version\` here" >&2
done <<EOF
$(list_candidates)
EOF

if [ -z "$cli" ]; then
  if [ -n "${CLI_NAME:-}" ]; then
    echo "FAIL: CLI_NAME=$CLI_NAME does not answer \`version\` on this platform:" >&2
    file "$CLI_NAME" >&2 || true
  else
    echo "FAIL: no tvm-cli on PATH answers \`version\`, and neither does" >&2
    echo "      $COMPILER_DIR/tvm-cli." >&2
    echo "      Every candidate tried is listed above." >&2
  fi
  echo "      Install a native tvm-cli, or export CLI_NAME=/path/to/tvm-cli." >&2
  exit 1
fi

echo "OK: tvm-cli = $cli ($("$cli" version 2>&1 </dev/null | head -1))"
