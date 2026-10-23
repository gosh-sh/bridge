#!/usr/bin/env bash
# Resolve "the latest build of an acki-nacki branch" ON the e2e host, where the registry
# login lives: pull the branch-tagged images, read the commit they were built from (the
# OCI label org.opencontainers.image.revision), take their digests, and copy the zerostate
# helpers out of the all-bin image. Piped over ssh by .woodpecker/local-net-e2e.yaml
# (`bash -s`); stdout carries ONLY KEY=VALUE lines for the runner to source, everything
# else goes to stderr.
#
# Required env: REGISTRY (docker.gosh.sh), TAG (e.g. br_dev), WORK_ROOT (e.g. ~/bridge-e2e).
# node and all-bin must come from the same commit (the helpers bake the zerostate the
# nodes will run); gql-server and block-manager are reported and run by digest as found.
set -euo pipefail
REGISTRY=${REGISTRY:?REGISTRY is required}; TAG=${TAG:?TAG is required}; WORK_ROOT=${WORK_ROOT:?WORK_ROOT is required}
label() { docker inspect "$1" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}'; }
digest() { docker inspect "$1" --format '{{index .RepoDigests 0}}'; }
declare -A REV DIG
for img in ackinacki-node ackinacki-gql-server ackinacki-block-manager ackinacki-all-bin; do
  ref="$REGISTRY/$img:$TAG"
  docker pull -q "$ref" >&2
  REV[$img]=$(label "$ref"); DIG[$img]=$(digest "$ref")
  [ -n "${REV[$img]}" ] && [ -n "${DIG[$img]}" ] || { echo "FATAL: $ref has no revision label or digest" >&2; exit 65; }
  echo "$img:$TAG -> ${REV[$img]:0:12} ${DIG[$img]##*@}" >&2
done
[ "${REV[ackinacki-node]}" = "${REV[ackinacki-all-bin]}" ] || { echo "FATAL: node ($TAG) is built from ${REV[ackinacki-node]:0:12} but all-bin ($TAG) from ${REV[ackinacki-all-bin]:0:12}: a build is in flight, retry later" >&2; exit 66; }
SHA=${REV[ackinacki-node]}
H="$WORK_ROOT/helpers/$SHA"
if [ ! -x "$H/node-helper" ] || [ ! -x "$H/zerostate-helper" ]; then
  mkdir -p "$H"
  c=$(docker create "$REGISTRY/ackinacki-all-bin@${DIG[ackinacki-all-bin]##*@}" /bin/true)
  docker cp "$c:/usr/local/bin/node-helper" "$H/node-helper" >&2
  docker cp "$c:/usr/local/bin/zerostate-helper" "$H/zerostate-helper" >&2
  docker rm -f "$c" >/dev/null
  chmod +x "$H/node-helper" "$H/zerostate-helper"
fi
echo "ACKI_SHA=$SHA"
echo "NODE_IMG=${DIG[ackinacki-node]}"
echo "GQL_IMG=${DIG[ackinacki-gql-server]}"
echo "BM_IMG=${DIG[ackinacki-block-manager]}"
echo "ALLBIN_IMG=${DIG[ackinacki-all-bin]}"
echo "HELPERS_DIR=$H"
