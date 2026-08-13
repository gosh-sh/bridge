#!/usr/bin/env bash
# Cargo git deps: use SSH instead of HTTPS for gosh-sh / tvmlabs (Linux + macOS).
# Writes .cargo/git-ssh.config and exports GIT_CONFIG_GLOBAL so cargo's git
# subprocesses (which run outside this repo) pick up the rewrite.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CFG="$ROOT/.cargo/git-ssh.config"

mkdir -p "$ROOT/.cargo"
cat >"$CFG" <<'EOF'
[url "git@github.com:gosh-sh/"]
    insteadOf = https://github.com/gosh-sh/
[url "git@github.com:tvmlabs/"]
    insteadOf = https://github.com/tvmlabs/
EOF

export GIT_CONFIG_GLOBAL="$CFG"

for cfg in "$ROOT/.cargo/config.toml" "$ROOT/crates/an-bridge-prover/.cargo/config.toml" "$ROOT/crates/bridge-prover-orchestrator/.cargo/config.toml"; do
  dir="$(dirname "$cfg")"
  mkdir -p "$dir"
  if [[ -f "$cfg" ]] && grep -q 'git-fetch-with-cli' "$cfg"; then
    continue
  fi
  if [[ -f "$cfg" ]]; then
    printf '\n[net]\ngit-fetch-with-cli = true\n' >>"$cfg"
  else
    printf '[net]\ngit-fetch-with-cli = true\n' >"$cfg"
  fi
done

echo "[bootstrap] GIT_CONFIG_GLOBAL=$CFG"
echo "[bootstrap] Cargo git SSH rewrite OK"
