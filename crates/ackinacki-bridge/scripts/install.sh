#!/usr/bin/env bash
# Install ackinacki-bridge from published releases. Downloads only — nothing
# is compiled here, and neither Rust nor a checkout of this repository is
# needed.
#
#   curl -fLO https://raw.githubusercontent.com/gosh-sh/bridge/main/crates/ackinacki-bridge/scripts/install.sh
#   less install.sh          # it downloads and installs; read it first
#   bash install.sh
#
# Options:
#   --prefix PATH   where to install (default ~/.local/share/ackinacki-bridge)
#   --check         report what is missing, download nothing
#   --yes           do not ask before each download
#
# Overrides, for a mirror or an internal build:
#   BRIDGE_RELEASE_BASE   where the assets live (default: this repository's
#                         latest GitHub release)
#   BRIDGE_SOLC_URL       where solc 0.8.19 comes from
#
# What it puts on disk, skipping whatever is already there:
#
#   1. ackinacki-bridge   the CLI, ~50 MB
#   2. aggregate-proof    the prover subprocess the CLI shells out to, ~7 MB
#   3. solc 0.8.19        the aggregator compiles a Yul verifier with it
#   4. verifier bytecode  what the proof is self-checked against, ~400 KB
#   5. kzg_bn254_21.srs   the Hermez ceremony, ~257 MB
#   6. a profile          with absolute paths into the prefix
#
# Every release asset is checked against the release's SHA256SUMS, and solc
# against the version it reports.

set -euo pipefail

readonly RELEASE_BASE="${BRIDGE_RELEASE_BASE:-https://github.com/gosh-sh/bridge/releases/latest/download}"
readonly SOLC_VERSION="0.8.19"
readonly SOLC_URL="${BRIDGE_SOLC_URL:-https://github.com/ethereum/solidity/releases/download/v${SOLC_VERSION}/solc-static-linux}"
readonly BUNDLE="ackinacki-bridge-linux-x86_64.tar.gz"
readonly CEREMONY="kzg_bn254_21.srs"
readonly SUMS="SHA256SUMS"
readonly DISK_NEED_KB=$((6 * 1024 * 1024))   # ceremony + binaries + keygen headroom
readonly RAM_WARN_GB=48                      # Circuit 4 peaks around 40 GB

SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
readonly SELF

PREFIX="$HOME/.local/share/ackinacki-bridge"
MODE=install
ASSUME_YES=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)  PREFIX=${2:?--prefix needs a path}; shift 2 ;;
    --check)   MODE=check; shift ;;
    --yes|-y)  ASSUME_YES=1; shift ;;
    -h|--help) awk 'NR==1{next} /^#/{sub(/^# ?/, ""); print; next} {exit}' "$SELF"; exit 0 ;;
    *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
  esac
done

# One stream for the whole report. Splitting it across stdout and stderr
# reorders the lines under a pipe, and a MISSING line under the wrong heading
# is worse than no heading. The exit code is the machine signal.
note() { printf '  %s\n' "$*"; }
ok()   { printf '  ok      %s\n' "$*"; }
warn() { printf '  warn    %s\n' "$*"; }
gap()  { printf '  MISSING %s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

confirm() {
  if [ "$MODE" = check ]; then return 1; fi
  if [ "$ASSUME_YES" = 1 ]; then return 0; fi
  printf '  %s [y/N] ' "$1"
  local reply
  read -r reply </dev/tty 2>/dev/null || return 1
  case "$reply" in [yY] | [yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

# `--aggregator-dir` names a cargo tree, not a bin directory: the CLI joins
# `target/release/aggregate-proof` onto it (preflight.rs). The installed
# layout reproduces that shape rather than inventing a tidier one the binary
# would refuse.
BIN_DIR="$PREFIX/bin"
AGG_DIR="$PREFIX/aggregator"
AGG_BIN="$AGG_DIR/target/release/aggregate-proof"
CLI_BIN="$BIN_DIR/ackinacki-bridge"
SOLC_BIN="$BIN_DIR/solc"
VERIFIERS="$PREFIX/verifiers"
PARAMS="$PREFIX/params"
SRS="$PARAMS/$CEREMONY"
PROFILE="$PREFIX/bridge_config"
WITHDRAW_VERIFIER="$VERIFIERS/BridgeWithdrawalAggregatorVerifier.bin"

solc_version_of() { "$1" --version 2>/dev/null | sed -n 's/.*Version: \([0-9][0-9.]*\).*/\1/p'; }

# `-x` is a permission bit, not a promise that the file runs. These are
# binaries built somewhere else: the usual way they fail is the dynamic
# loader refusing a glibc older than the build host's, and that has to be
# caught here rather than at stage 5, after the burn. LAST_RUN_ERROR carries
# the loader's own words so the refusal can quote them.
LAST_RUN_ERROR=""
can_run() {   # can_run <binary> <arg...>
  local out rc
  LAST_RUN_ERROR=""
  if [ ! -x "$1" ]; then return 1; fi
  out=$("$@" 2>&1); rc=$?
  case "$out" in
    *"GLIBC_"*|*"error while loading shared libraries"*|*"cannot execute"*)
      LAST_RUN_ERROR=$(printf '%s' "$out" | head -1)
      return 1 ;;
  esac
  if [ "$rc" -ge 126 ]; then
    LAST_RUN_ERROR=$(printf '%s' "$out" | head -1)
    return 1
  fi
  return 0
}

have_cli()       { can_run "$CLI_BIN" --version; }
have_agg()       { can_run "$AGG_BIN" --help; }
have_verifiers() { [ -s "$WITHDRAW_VERIFIER" ]; }
have_ceremony()  { [ -s "$SRS" ]; }
have_profile()   { [ -s "$PROFILE" ]; }
have_solc()      {
  if [ -x "$SOLC_BIN" ] && [ "$(solc_version_of "$SOLC_BIN")" = "$SOLC_VERSION" ]; then return 0; fi
  command -v solc >/dev/null 2>&1 && [ "$(solc_version_of solc)" = "$SOLC_VERSION" ]
}

printf 'ackinacki-bridge — install\n'
printf '  mode        %s\n' "$MODE"
printf '  prefix      %s\n' "$PREFIX"
printf '  release     %s\n' "$RELEASE_BASE"

# ---------------------------------------------------------------------------
step "This machine"
# ---------------------------------------------------------------------------
os=$(uname -s); arch=$(uname -m)
if [ "$os" = Linux ] && [ "$arch" = x86_64 ]; then
  ok "$os $arch"
else
  warn "$os $arch — the published binaries are Linux x86_64"
  note "nothing below will run here; build from source instead"
fi

for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || warn "no $tool on PATH — this script needs it"
done

mkdir -p "$PREFIX" 2>/dev/null || true
if [ -d "$PREFIX" ]; then
  avail=$(df -Pk "$PREFIX" 2>/dev/null | awk 'NR==2 {print $4}') || avail=""
  if [ -n "$avail" ] && [ "$avail" -ge "$DISK_NEED_KB" ]; then
    ok "$((avail / 1024 / 1024)) GB free on $PREFIX"
  elif [ -n "$avail" ]; then
    warn "$((avail / 1024 / 1024)) GB free on $PREFIX; ~6 GB wanted"
  fi
fi

if [ -r /proc/meminfo ]; then
  total_gb=$(awk '/^MemTotal:/ {m=$2} /^SwapTotal:/ {s=$2} END {print int((m+s)/1024/1024)}' /proc/meminfo)
  if [ "${total_gb:-0}" -ge "$RAM_WARN_GB" ]; then
    ok "${total_gb} GB RAM + swap"
  else
    warn "${total_gb} GB RAM + swap; Circuit 4 peaks near 40 GB, and it runs AFTER the burn"
  fi
fi

# ---------------------------------------------------------------------------
# Fetch an asset and check it against the release's SHA256SUMS. Without that
# list nothing is written: an unverified 50 MB binary that signs transactions
# is not something to install merely because a download succeeded.
# ---------------------------------------------------------------------------
WORK=""
cleanup() { if [ -n "$WORK" ]; then rm -rf "$WORK"; fi; }
trap cleanup EXIT

sums_ready=0
fetch_sums() {
  if [ "$sums_ready" = 1 ]; then return 0; fi
  if [ -z "$WORK" ]; then WORK=$(mktemp -d); fi
  if ! curl -fsSL "$RELEASE_BASE/$SUMS" -o "$WORK/$SUMS"; then
    warn "cannot fetch $RELEASE_BASE/$SUMS"
    return 1
  fi
  sums_ready=1
}

fetch_verified() {   # fetch_verified <asset> <destination>
  local asset=$1 dest=$2
  fetch_sums || return 1
  if ! grep -q "  ${asset}\$" "$WORK/$SUMS"; then
    warn "$asset is not listed in $SUMS — refusing to install it unverified"
    return 1
  fi
  curl -fL --progress-bar "$RELEASE_BASE/$asset" -o "$WORK/$asset" || return 1
  if ! (cd "$WORK" && grep "  ${asset}\$" "$SUMS" | sha256sum -c --status -); then
    warn "$asset failed its checksum — discarded"
    rm -f "$WORK/$asset"
    return 1
  fi
  mkdir -p "$(dirname "$dest")"
  mv "$WORK/$asset" "$dest"
}

# ---------------------------------------------------------------------------
step "Binaries and verifier bytecode"
# ---------------------------------------------------------------------------
if have_cli && have_agg && have_verifiers; then
  ok "$CLI_BIN"
  ok "$AGG_BIN"
  ok "$VERIFIERS"
else
  gap "$BUNDLE"
  note "the CLI, the aggregator subprocess, the verifier .bin files and a profile"
  if confirm "download $BUNDLE (~60 MB) from $RELEASE_BASE?"; then
    if [ -z "$WORK" ]; then WORK=$(mktemp -d); fi
    if fetch_verified "$BUNDLE" "$WORK/bundle.tar.gz"; then
      unpack="$WORK/unpacked"
      mkdir -p "$unpack"
      tar -xzf "$WORK/bundle.tar.gz" -C "$unpack" --strip-components=1
      mkdir -p "$BIN_DIR" "$(dirname "$AGG_BIN")" "$VERIFIERS"
      install -m 0755 "$unpack/ackinacki-bridge" "$CLI_BIN"
      install -m 0755 "$unpack/aggregate-proof" "$AGG_BIN"
      cp "$unpack"/verifiers/*.bin "$VERIFIERS/"
      cp "$unpack/bridge_config" "$PROFILE.release"
      ok "unpacked into $PREFIX"
      if have_cli && have_agg; then
        ok "both binaries run on this system"
      else
        warn "unpacked, but a binary does not run here: ${LAST_RUN_ERROR:-unknown}"
      fi
    fi
  fi
fi

# ---------------------------------------------------------------------------
step "solc ${SOLC_VERSION}"
# ---------------------------------------------------------------------------
if have_solc; then
  if [ -x "$SOLC_BIN" ]; then
    ok "$SOLC_BIN"
  else
    ok "solc ${SOLC_VERSION} at $(command -v solc)"
  fi
else
  if command -v solc >/dev/null 2>&1; then
    gap "solc $(solc_version_of solc || echo unknown) on PATH; ${SOLC_VERSION} is required"
    note "another version emits different bytecode and fails the stage-5 self-check"
    note "the pinned one goes into the prefix, ahead of it on PATH"
  else
    gap "solc ${SOLC_VERSION}"
  fi
  if confirm "download solc ${SOLC_VERSION} (~15 MB)?"; then
    mkdir -p "$BIN_DIR"
    curl -fL --progress-bar "$SOLC_URL" -o "$SOLC_BIN.part"
    chmod +x "$SOLC_BIN.part"
    got=$(solc_version_of "$SOLC_BIN.part") || got=""
    if [ "$got" != "$SOLC_VERSION" ]; then
      rm -f "$SOLC_BIN.part"
      warn "the download reports '${got:-nothing}', not ${SOLC_VERSION} — discarded"
    else
      mv "$SOLC_BIN.part" "$SOLC_BIN"
      ok "installed solc $got"
    fi
  fi
fi

# ---------------------------------------------------------------------------
step "KZG ceremony"
# ---------------------------------------------------------------------------
if have_ceremony; then
  ok "$SRS ($(du -Lh "$SRS" | cut -f1))"
else
  gap "$SRS"
  note "the Hermez Perpetual Powers of Tau at degree 21; lower degrees derive from it"
  if confirm "download $CEREMONY (~257 MB)?"; then
    if fetch_verified "$CEREMONY" "$SRS"; then ok "$SRS"; fi
  fi
fi

# ---------------------------------------------------------------------------
step "Profile"
# ---------------------------------------------------------------------------
# The release ships a profile whose paths are relative to a source checkout.
# Rewrite exactly the path keys to point into the prefix, and leave the
# endpoints, the bridge address and the pinned identity as published.
if have_profile; then
  ok "$PROFILE"
elif [ -s "$PROFILE.release" ]; then
  mkdir -p "$PREFIX/work_dir" "$PREFIX/withdraw-state" "$PARAMS/pk_cache"
  sed -E \
    -e "s#^BRIDGE_PARAMS_DIR=.*#BRIDGE_PARAMS_DIR=$PARAMS#" \
    -e "s#^BRIDGE_PK_CACHE_DIR=.*#BRIDGE_PK_CACHE_DIR=$PARAMS/pk_cache#" \
    -e "s#^BRIDGE_WORK_DIR=.*#BRIDGE_WORK_DIR=$PREFIX/work_dir#" \
    -e "s#^BRIDGE_SNARK_DIR=.*#BRIDGE_SNARK_DIR=$PREFIX/work_dir/shplonk-snark#" \
    -e "s#^BRIDGE_WITHDRAW_STATE_DIR=.*#BRIDGE_WITHDRAW_STATE_DIR=$PREFIX/withdraw-state#" \
    -e "s#^BRIDGE_AGGREGATOR_DIR=.*#BRIDGE_AGGREGATOR_DIR=$AGG_DIR#" \
    -e "s#^BRIDGE_VERIFIERS_DIR=.*#BRIDGE_VERIFIERS_DIR=$VERIFIERS#" \
    "$PROFILE.release" > "$PROFILE"
  ok "$PROFILE"
  note "endpoints and the bridge address are the release's; only paths were rewritten"
else
  gap "$PROFILE"
fi

# ---------------------------------------------------------------------------
step "Result"
# ---------------------------------------------------------------------------
missing=0
if ! have_cli; then
  gap "$CLI_BIN"
  if [ -n "$LAST_RUN_ERROR" ]; then
    note "$LAST_RUN_ERROR"
    note "the published binary is newer than this system's C library; use a"
    note "distribution release at least as new as the build host, or build"
    note "from source"
  fi
  missing=$((missing + 1))
fi
if ! have_agg; then
  gap "$AGG_BIN"
  if [ -n "$LAST_RUN_ERROR" ]; then note "$LAST_RUN_ERROR"; fi
  missing=$((missing + 1))
fi
have_solc      || { gap "solc ${SOLC_VERSION}"; missing=$((missing + 1)); }
have_verifiers || { gap "$WITHDRAW_VERIFIER"; missing=$((missing + 1)); }
have_ceremony  || { gap "$SRS"; missing=$((missing + 1)); }
have_profile   || { gap "$PROFILE"; missing=$((missing + 1)); }

if [ "$missing" -eq 0 ]; then
  cat <<EOF
  This host can run a real withdrawal.

    export PATH="$BIN_DIR:\$PATH"
    export BRIDGE_CONFIG="$PROFILE"
    ackinacki-bridge withdraw --help

  The PATH line is not a convenience: the aggregator resolves the bare name
  \`solc\`, so the pinned one has to be reachable that way.

  Then read QUICKSTART.md.
EOF
  exit 0
fi

printf '  %d of 6 still missing.\n' "$missing"
if [ "$MODE" = check ]; then
  printf '  Re-run without --check to download them.\n'
fi
exit 1
