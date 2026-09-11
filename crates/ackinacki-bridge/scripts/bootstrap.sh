#!/usr/bin/env bash
# Take a machine with nothing on it to a working `ackinacki-bridge`.
#
# For people who have neither Rust nor a checkout. If you already have the
# repository, run scripts/install.sh inside it instead — this script is a
# wrapper that gets you to the point where you can.
#
#   curl -fLO https://raw.githubusercontent.com/gosh-sh/bridge/main/crates/ackinacki-bridge/scripts/bootstrap.sh
#   less bootstrap.sh            # it installs software; read it first
#   bash bootstrap.sh
#
# Options:
#   --dir PATH     where to clone (default ~/ackinacki-bridge)
#   --check        report what is missing, install nothing
#   --yes          do not ask before each step
#
# What it installs, in order, skipping whatever is already there:
#
#   1. system packages   a C toolchain, git, curl
#   2. rustup            the toolchain version is pinned by the repository
#   3. the repository    a shallow clone of the default branch
#   4. the withdrawal prerequisites, by handing over to scripts/install.sh
#
# It never runs a privileged command without showing it to you first.

set -euo pipefail

readonly REPO_URL="https://github.com/gosh-sh/bridge.git"
readonly RUSTUP_URL="https://sh.rustup.rs"
readonly DISK_NEED_KB=$((20 * 1024 * 1024))   # build artifacts + the ceremony

DIR="$HOME/ackinacki-bridge"
MODE=install
ASSUME_YES=0
while [ $# -gt 0 ]; do
  case "$1" in
    --dir)     DIR=${2:?--dir needs a path}; shift 2 ;;
    --check)   MODE=check; shift ;;
    --yes|-y)  ASSUME_YES=1; shift ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
  esac
done

note() { printf '  %s\n' "$*"; }
ok()   { printf '  ok      %s\n' "$*"; }
warn() { printf '  warn    %s\n' "$*" >&2; }
gap()  { printf '  MISSING %s\n' "$*" >&2; }
step() { printf '\n== %s\n' "$*"; }

confirm() {
  if [ "$MODE" = check ]; then return 1; fi
  if [ "$ASSUME_YES" = 1 ]; then return 0; fi
  printf '  %s [y/N] ' "$1" >&2
  local reply
  read -r reply </dev/tty 2>/dev/null || return 1
  case "$reply" in [yY] | [yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

printf 'ackinacki-bridge — bootstrap\n'
printf '  mode        %s\n' "$MODE"
printf '  install to  %s\n' "$DIR"

# ---------------------------------------------------------------------------
step "This machine"
# ---------------------------------------------------------------------------
os=$(uname -s)
arch=$(uname -m)
if [ "$os" = Linux ] && [ "$arch" = x86_64 ]; then
  ok "$os $arch"
else
  warn "$os $arch — this path is exercised on Linux x86_64 only"
  note "the pinned solc build is solc-static-linux; on another platform you"
  note "have to provide solc 0.8.19 yourself, and the rest is untested here"
fi

parent=$(dirname "$DIR")
mkdir -p "$parent" 2>/dev/null || true
avail=$(df -Pk "$parent" 2>/dev/null | awk 'NR==2 {print $4}') || avail=""
if [ -n "$avail" ] && [ "$avail" -ge "$DISK_NEED_KB" ]; then
  ok "$((avail / 1024 / 1024)) GB free on $parent"
elif [ -n "$avail" ]; then
  warn "$((avail / 1024 / 1024)) GB free on $parent; ~20 GB wanted for build artifacts and the ceremony"
fi

# ---------------------------------------------------------------------------
step "System packages"
# ---------------------------------------------------------------------------
# Three things, and no more. Rust needs a C toolchain to link with, git
# fetches the repository and curl the downloads. No OpenSSL: the crate this
# builds uses rustls and the aggregator has no TLS at all — read off their
# lockfiles rather than assumed from the repository's other container
# definitions, which build a different binary.
need=()
command -v git  >/dev/null 2>&1 || need+=(git)
command -v curl >/dev/null 2>&1 || need+=(curl)
command -v cc   >/dev/null 2>&1 || need+=("a C toolchain")

if [ ${#need[@]} -eq 0 ]; then
  ok "git, curl and a C toolchain are present"
else
  gap "${need[*]}"
  if   command -v apt-get >/dev/null 2>&1; then
    cmd="sudo apt-get update && sudo apt-get install -y build-essential git curl"
  elif command -v dnf >/dev/null 2>&1; then
    cmd="sudo dnf install -y gcc gcc-c++ make git curl"
  elif command -v pacman >/dev/null 2>&1; then
    cmd="sudo pacman -S --needed base-devel git curl"
  else
    cmd=""
    note "unrecognised package manager — install by hand: a C toolchain, git, curl"
  fi
  if [ -n "$cmd" ]; then
    note "$cmd"
    if confirm "run that now? it needs your sudo password"; then
      eval "$cmd"
      ok "installed"
    fi
  fi
fi

# ---------------------------------------------------------------------------
step "Rust"
# ---------------------------------------------------------------------------
# The repository pins its toolchain in rust-toolchain.toml, so rustup is
# installed with no default toolchain and the pin decides what gets
# downloaded on the first cargo invocation inside the clone.
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi

if command -v cargo >/dev/null 2>&1; then
  ok "cargo $(cargo --version 2>/dev/null | awk '{print $2}')"
  command -v rustup >/dev/null 2>&1 \
    || warn "rustup is not on PATH; the repository's pinned toolchain will not be honoured"
else
  gap "no cargo on PATH"
  note "installs rustup from $RUSTUP_URL, with no default toolchain —"
  note "the repository pins its own and rustup will fetch it on first build"
  if confirm "install rustup now?"; then
    curl -fsSL --proto '=https' --tlsv1.2 "$RUSTUP_URL" -o rustup-init.sh
    sh rustup-init.sh -y --no-modify-path --default-toolchain none
    rm -f rustup-init.sh
    . "$HOME/.cargo/env"
    ok "installed rustup"
    case ":$PATH:" in
      *":$HOME/.cargo/bin:"*) ;;
      *) warn "~/.cargo/bin is not on PATH in your login shell"
         note 'add: . "$HOME/.cargo/env"' ;;
    esac
  fi
fi

# ---------------------------------------------------------------------------
step "Repository"
# ---------------------------------------------------------------------------
if git -C "$DIR" rev-parse --git-dir >/dev/null 2>&1; then
  ok "$DIR"
elif [ -e "$DIR" ]; then
  gap "$DIR exists and is not a git checkout"
  note "pass --dir to clone somewhere else"
else
  gap "$DIR"
  if confirm "clone $REPO_URL into it? shallow, ~200 MB"; then
    git clone --depth 1 "$REPO_URL" "$DIR"
    ok "cloned"
    note "shallow; run 'git fetch --unshallow' inside it if you want history"
  fi
fi

# ---------------------------------------------------------------------------
step "Withdrawal prerequisites"
# ---------------------------------------------------------------------------
inner="$DIR/crates/ackinacki-bridge/scripts/install.sh"
if [ ! -x "$inner" ]; then
  gap "$inner"
  note "nothing more can be done until the clone is in place"
  printf '\n  Bootstrap incomplete.\n' >&2
  exit 1
fi

inner_args=()
if [ "$MODE" = check ]; then inner_args+=(--check); fi
if [ "$ASSUME_YES" = 1 ]; then inner_args+=(--yes); fi
note "handing over to scripts/install.sh"
set +e
"$inner" ${inner_args[@]+"${inner_args[@]}"}
inner_rc=$?
set -e

# ---------------------------------------------------------------------------
step "Bootstrap result"
# ---------------------------------------------------------------------------
if [ "$inner_rc" -eq 0 ]; then
  cat <<EOF
  Ready. From here:

    cd $DIR/crates/ackinacki-bridge
    export BRIDGE_CONFIG=\$PWD/config/bridge_config

  Then read QUICKSTART.md — it is one withdrawal in seven steps.
EOF
  exit 0
fi

printf '  scripts/install.sh reported work still to do (exit %d).\n' "$inner_rc" >&2
printf '  Re-run it inside the clone:\n' >&2
printf '    cd %s/crates/ackinacki-bridge && scripts/install.sh\n' "$DIR" >&2
exit "$inner_rc"
