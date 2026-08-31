#!/usr/bin/env bash
set -Eeuo pipefail
set +x

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

env_file=${1:-}
[[ -n "$env_file" && -f "$env_file" ]] || die "usage: $0 /path/to/instance.env"

# The committed examples contain no shell expressions. Production files are
# administrator-owned and use the same simple KEY=value format.
# shellcheck disable=SC1090
source "$env_file"
export -n RELAYER_PRIVATE_KEY 2>/dev/null || true

runtime_layout=${RELAYER_RUNTIME_LAYOUT:-host}
case "$runtime_layout" in
  host) ;;
  compose)
    script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
    # shellcheck disable=SC1091
    source "$script_dir/container-paths.sh"
    remap_compose_runtime_paths
    ;;
  *) die "unknown RELAYER_RUNTIME_LAYOUT: $runtime_layout" ;;
esac

for name in BRIDGE_PARAMS_DIR PARAMS_SHA256SUMS PARAMS_MANIFEST_SHA256; do
  value=${!name-}
  [[ -n "$value" && "$value" != *CHANGEME* ]] || die "$name is missing or a placeholder"
done

[[ -d "$BRIDGE_PARAMS_DIR" ]] || die "params directory not found: $BRIDGE_PARAMS_DIR"
[[ -f "$PARAMS_SHA256SUMS" ]] || die "params manifest not found: $PARAMS_SHA256SUMS"
[[ "$PARAMS_MANIFEST_SHA256" =~ ^[0-9a-fA-F]{64}$ ]] || die "invalid PARAMS_MANIFEST_SHA256"

actual_manifest_hash=$(sha256sum "$PARAMS_SHA256SUMS" | awk '{print $1}')
[[ "${actual_manifest_hash,,}" == "${PARAMS_MANIFEST_SHA256,,}" ]] ||
  die "params manifest hash mismatch"

if ! awk '
  NF != 2 || length($1) != 64 || $1 !~ /^[0-9a-fA-F]+$/ ||
    $2 !~ /^[A-Za-z0-9._\/-]+$/ { bad=1 }
  $2 ~ /^\// || $2 ~ /(^|\/)\.\.($|\/)/ { bad=1 }
  END { exit bad }
' "$PARAMS_SHA256SUMS"; then
  die "manifest contains malformed or unsafe paths"
fi

required=(
  kzg_bn254_17.srs
  kzg_bn254_19.srs
  kzg_bn254_20.srs
  kzg_bn254_21.srs
  kzg_bn254_22.srs
  primary_pk.bin primary_vk.bin primary_config_params.json
  fallback_pk.bin fallback_vk.bin fallback_config_params.json
  layer_pk.bin layer_vk.bin layer_config_params.json
)
for file_name in "${required[@]}"; do
  grep -Eq "^[0-9a-fA-F]{64}  ${file_name//./\\.}$" "$PARAMS_SHA256SUMS" ||
    die "params manifest does not pin $file_name"
done

printf 'Hashing the full proving-artifact set; this can take several minutes.\n'
(
  cd "$BRIDGE_PARAMS_DIR"
  sha256sum --check --strict "$PARAMS_SHA256SUMS"
)
printf 'All proving artifacts match the pinned manifest.\n'
