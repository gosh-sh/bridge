#!/usr/bin/env bash
set -Eeuo pipefail
set +x
umask 077

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

params_dir=${1:-}
service_user=${2:-gosh-relayer}
[[ -n "$params_dir" && -d "$params_dir" ]] ||
  die "usage: $0 PARAMS_DIR [SERVICE_USER]"
id "$service_user" >/dev/null 2>&1 || die "service user does not exist: $service_user"

declare -A srs_sizes=(
  [17]=16777476
  [19]=67109124
  [20]=134217988
  [21]=268435716
  [22]=536871172
)
for k in 17 19 20 21 22; do
  path=$params_dir/kzg_bn254_${k}.srs
  [[ -f "$path" ]] || die "missing K=$k SRS"
  size=$(stat -c '%s' "$path")
  [[ "$size" == "${srs_sizes[$k]}" ]] || die "bad K=$k SRS size: $size"
  head=$(od -An -tx1 -j $((size - 128)) -N 6 "$path" | tr -d ' \n')
  [[ "$head" == 928fafb3d0cc ]] || die "K=$k is not a Hermez SRS"
done

for stem in primary fallback layer; do
  for suffix in _pk.bin _vk.bin _config_params.json; do
    [[ -s "$params_dir/$stem$suffix" ]] || die "missing/empty $stem$suffix"
  done
done

manifest_tmp=$(mktemp --tmpdir="$params_dir" .SHA256SUMS.XXXXXX)
cleanup() {
  rm -f -- "$manifest_tmp"
}
trap cleanup EXIT
(
  cd "$params_dir"
  find . -maxdepth 1 -type f \
    ! -name 'SHA256SUMS' ! -name '.SHA256SUMS.*' -printf '%P\0' |
    sort -z | xargs -0 sha256sum
) >"$manifest_tmp"
[[ -s "$manifest_tmp" ]] || die "empty params manifest"
mv -f -- "$manifest_tmp" "$params_dir/SHA256SUMS"
trap - EXIT

# Static SRS/inner keys are immutable to the runtime account.  The outer
# aggregator cache is intentionally isolated in its own mutable directory.
find "$params_dir" -maxdepth 1 -type f -exec chown root:root {} +
find "$params_dir" -maxdepth 1 -type f -exec chmod 0444 {} +
chown root:root "$params_dir"
chmod 0755 "$params_dir"
install -d -o "$service_user" -g "$service_user" -m 0700 "$params_dir/pk_cache"

manifest_sha=$(sha256sum "$params_dir/SHA256SUMS" | awk '{print $1}')
printf 'PARAMS_MANIFEST_SHA256=%s\n' "$manifest_sha"
printf 'Static params finalized read-only; mutable outer cache: %s/pk_cache\n' "$params_dir"
