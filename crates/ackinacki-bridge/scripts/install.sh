#!/usr/bin/env bash
# Provision everything a real `ackinacki-bridge withdraw` needs on this host.
#
#   scripts/install.sh --check     report what is missing, install nothing
#   scripts/install.sh             install what is missing, asking first
#   scripts/install.sh --yes       the same without the questions
#
# Four things are required, and a run that reaches stage 5 without one of
# them has already burned the USDC:
#
#   1. solc, exactly 0.8.19        the aggregator compiles the Yul verifier
#   2. kzg_bn254_21.srs            the Hermez ceremony, ~256 MB on disk
#   3. aggregate-proof             prebuilt; the CLI will not build it
#   4. ackinacki-bridge            the CLI itself
#
# Disk and RAM are checked too, because the keygen that needs them also runs
# at stage 5, after the burn.
#
# None of it is needed for `--dry-run`, which skips every prover artifact.

set -euo pipefail

readonly SOLC_VERSION="0.8.19"
readonly SOLC_URL="https://github.com/ethereum/solidity/releases/download/v${SOLC_VERSION}/solc-static-linux"
readonly PTAU_URL="https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_21.ptau"
readonly PARAMS_NEED_KB=$((5 * 1024 * 1024))   # ceremony + event_pk + headroom
readonly RAM_WARN_GB=48                        # Circuit 4 peaks around 40 GB

cd "$(dirname "$0")/.."     # crates/ackinacki-bridge/

MODE=install
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    --check)   MODE=check ;;
    --yes|-y)  ASSUME_YES=1 ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $arg (try --help)" >&2; exit 2 ;;
  esac
done

note() { printf '  %s\n' "$*"; }
ok()   { printf '  ok      %s\n' "$*"; }
warn() { printf '  warn    %s\n' "$*" >&2; }
gap()  { printf '  MISSING %s\n' "$*" >&2; }
step() { printf '\n== %s\n' "$*"; }

# Nothing installs without consent. --check never asks and never acts.
confirm() {
  if [ "$MODE" = check ]; then return 1; fi
  if [ "$ASSUME_YES" = 1 ]; then return 0; fi
  printf '  %s [y/N] ' "$1" >&2
  local reply
  read -r reply </dev/tty 2>/dev/null || return 1
  case "$reply" in [yY] | [yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

# Where params/ lives. Same precedence the CLI uses — shell environment
# first, profile second, because `dotenvy` does not overwrite what the shell
# already set.
resolve_params_dir() {
  local pd cfg
  if pd=$(printenv BRIDGE_PARAMS_DIR) && [ -n "$pd" ]; then
    printf '%s' "$pd"
    return
  fi
  cfg=$(printenv BRIDGE_CONFIG) || cfg=""
  [ -n "$cfg" ] || cfg=config/bridge_config
  if [ -r "$cfg" ]; then
    pd=$(set -a; . "$cfg"; printf '%s' "${BRIDGE_PARAMS_DIR-}")
    if [ -n "$pd" ]; then
      printf '%s' "$pd"
      return
    fi
  fi
  printf '%s' "../bridge-prover-libraries/params"
}

PARAMS_DIR=$(resolve_params_dir)
SRS="$PARAMS_DIR/kzg_bn254_21.srs"
PTAU="${HOME:-/nonexistent}/.cache/halo2-kzg-srs/powersOfTau28_hez_final_21.ptau"
AGGREGATOR_BIN=../bridge-evm-aggregator/target/release/aggregate-proof
CLI_BIN=../bridge-prover-libraries/target/release/ackinacki-bridge

# The four requirements, asked the same way at the start and at the end, so
# the summary reports the state of the host rather than this script's
# bookkeeping.
have_solc()       { command -v solc >/dev/null 2>&1 && [ "$(solc_version_of solc)" = "$SOLC_VERSION" ]; }
have_ceremony()   { [ -s "$SRS" ]; }
have_aggregator() { [ -x "$AGGREGATOR_BIN" ]; }
have_cli()        { [ -x "$CLI_BIN" ]; }

solc_version_of() { "$1" --version 2>/dev/null | sed -n 's/.*Version: \([0-9][0-9.]*\).*/\1/p'; }

printf 'ackinacki-bridge — install\n'
printf '  mode        %s\n' "$MODE"
printf '  params dir  %s\n' "$PARAMS_DIR"

# ---------------------------------------------------------------------------
step "Host resources"
# ---------------------------------------------------------------------------
mkdir -p "$PARAMS_DIR" 2>/dev/null || true
if [ -d "$PARAMS_DIR" ]; then
  avail=$(df -Pk "$PARAMS_DIR" 2>/dev/null | awk 'NR==2 {print $4}') || avail=""
  if [ -n "$avail" ] && [ "$avail" -ge "$PARAMS_NEED_KB" ]; then
    ok "$((avail / 1024 / 1024)) GB free on the params filesystem"
  elif [ -n "$avail" ]; then
    warn "$((avail / 1024 / 1024)) GB free on the params filesystem; ~5 GB wanted"
  fi
else
  warn "cannot create $PARAMS_DIR"
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
step "solc ${SOLC_VERSION}"
# ---------------------------------------------------------------------------
if have_solc; then
  ok "solc ${SOLC_VERSION} at $(command -v solc)"
elif command -v solc >/dev/null 2>&1; then
  gap "solc $(solc_version_of solc || echo unknown) at $(command -v solc); ${SOLC_VERSION} is required"
  note "another version emits different bytecode and fails the stage-5 self-check"
  note "put ${SOLC_VERSION} ahead of it on PATH, or replace this one"
else
  gap "no solc on PATH"
  if confirm "download solc ${SOLC_VERSION} (~15 MB) into ~/.local/bin?"; then
    mkdir -p "$HOME/.local/bin"
    curl -fL --progress-bar -o "$HOME/.local/bin/solc.part" "$SOLC_URL"
    chmod +x "$HOME/.local/bin/solc.part"
    got=$(solc_version_of "$HOME/.local/bin/solc.part") || got=""
    if [ "$got" != "$SOLC_VERSION" ]; then
      rm -f "$HOME/.local/bin/solc.part"
      echo "  the download reports '${got:-nothing}', not ${SOLC_VERSION} — discarded" >&2
      exit 1
    fi
    mv "$HOME/.local/bin/solc.part" "$HOME/.local/bin/solc"
    ok "installed solc $got into ~/.local/bin"
    case ":$PATH:" in
      *":$HOME/.local/bin:"*) ;;
      *) warn "~/.local/bin is not on PATH, and the aggregator resolves the bare name 'solc'"
         note 'add: export PATH="$HOME/.local/bin:$PATH"' ;;
    esac
  fi
fi

# ---------------------------------------------------------------------------
step "KZG ceremony"
# ---------------------------------------------------------------------------
if have_ceremony; then
  ok "$SRS ($(du -Lh "$SRS" | cut -f1))"
else
  gap "$SRS"
  note "one file is needed; degrees 17/19/20 are derived from it on first use"
  if confirm "provision it now? ~2.4 GB downloaded, ~30 min"; then
    if [ ! -s "$PTAU" ]; then
      mkdir -p "$(dirname "$PTAU")"
      # Downloaded to .part and renamed, so an interrupted fetch is not
      # mistaken for a cached one on the next run.
      curl -fL --progress-bar -o "$PTAU.part" "$PTAU_URL"
      mv "$PTAU.part" "$PTAU"
    fi
    params_abs=$(cd "$PARAMS_DIR" && pwd)
    (cd ../bridge-prover-libraries \
      && cargo build --release --bin bootstrap_hermez_srs \
      && ./target/release/bootstrap_hermez_srs --k 21 --params-dir "$params_abs")
    if have_ceremony; then ok "$SRS"; fi
    # The K=21 path has no SHA-256 anchor — the published one only reaches
    # K=20 — so what vouches for this file is the s_g2 head check the tool
    # makes, which identifies it as the Hermez ceremony.
  fi
fi

# ---------------------------------------------------------------------------
step "aggregate-proof"
# ---------------------------------------------------------------------------
if have_aggregator; then
  ok "$AGGREGATOR_BIN"
else
  gap "$AGGREGATOR_BIN"
  note "a real run refuses without it and will not fall back to cargo run"
  if confirm "build it now? ~10 min"; then
    (cd ../bridge-evm-aggregator && cargo build --release --bin aggregate-proof)
    if have_aggregator; then ok "built"; fi
  fi
fi

# ---------------------------------------------------------------------------
step "ackinacki-bridge"
# ---------------------------------------------------------------------------
if have_cli; then
  ok "$CLI_BIN"
else
  gap "$CLI_BIN"
  if confirm "build it now? ~5 min"; then
    cargo build --release -p ackinacki-bridge \
      --manifest-path ../bridge-prover-libraries/Cargo.toml
    if have_cli; then ok "built"; fi
  fi
fi

# ---------------------------------------------------------------------------
step "Result"
# ---------------------------------------------------------------------------
missing=0
have_solc       || { gap "solc ${SOLC_VERSION}"; missing=$((missing + 1)); }
have_ceremony   || { gap "$SRS"; missing=$((missing + 1)); }
have_aggregator || { gap "$AGGREGATOR_BIN"; missing=$((missing + 1)); }
have_cli        || { gap "$CLI_BIN"; missing=$((missing + 1)); }

if [ "$missing" -eq 0 ]; then
  cat <<'EOF'
  This host can run a real withdrawal. Next:

    export BRIDGE_CONFIG=$PWD/config/bridge_config

  Then read QUICKSTART.md.
EOF
  exit 0
fi

printf '  %d of 4 still missing.\n' "$missing" >&2
if [ "$MODE" = check ]; then
  printf '  Re-run without --check to install them.\n' >&2
fi
printf '  A --dry-run works without any of them. A real withdrawal does not.\n' >&2
exit 1
