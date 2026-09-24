#!/usr/bin/env bash
# Capture real Ethereum beacon light-client fixtures for the M0 spec / M1-M3 tests.
#
# Usage:
#   scripts/fetch_lc_fixtures.sh [mainnet|hoodi] [BEACON_URL]
#
# Defaults to a public CORS-enabled Lodestar endpoint. For reproducible CI, also
# pull the ethereum/consensus-spec-tests light_client vectors (see bottom).
#
# Endpoints used (Beacon REST, Altair light-client namespace):
#   GET /eth/v1/beacon/light_client/finality_update
#   GET /eth/v1/beacon/light_client/updates?start_period=P&count=1   (carries next_sync_committee)
#   GET /eth/v1/beacon/light_client/bootstrap/{block_root}           (WS anchor: committee + branch)
set -euo pipefail

NET="${1:-mainnet}"
case "$NET" in
  mainnet) DEFAULT_URL="https://lodestar-mainnet.chainsafe.io" ;;
  hoodi)   DEFAULT_URL="https://lodestar-hoodi.chainsafe.io" ;;
  holesky) DEFAULT_URL="https://lodestar-holesky.chainsafe.io" ;;
  *) echo "unknown network: $NET (use mainnet|hoodi|holesky)" >&2; exit 1 ;;
esac
BEACON="${2:-$DEFAULT_URL}"

OUT_DIR="$(cd "$(dirname "$0")/.." && pwd)/fixtures/$NET"
mkdir -p "$OUT_DIR"
echo "network=$NET beacon=$BEACON out=$OUT_DIR"

# 1. finality_update — core fixture (attested/finalized headers, finality_branch,
#    execution_branch, sync_aggregate). Small (~4 KB).
FU="$OUT_DIR/finality_update.json"
curl -fsS -H 'accept: application/json' \
  "$BEACON/eth/v1/beacon/light_client/finality_update" -o "$FU"
SLOT="$(jq -r '.data.attested_header.beacon.slot' "$FU")"
PERIOD=$(( SLOT / 8192 ))
echo "finality_update: attested slot=$SLOT period=$PERIOD -> $FU"

# 2. full update for the previous (completed) period — carries next_sync_committee
#    + next_sync_committee_branch (rotation hop). Large (~56 KB: 512 pubkeys).
PREV=$(( PERIOD - 1 ))
UPD="$OUT_DIR/update_period_${PREV}.json"
curl -fsS -H 'accept: application/json' \
  "$BEACON/eth/v1/beacon/light_client/updates?start_period=${PREV}&count=1" -o "$UPD"
echo "update(period=$PREV): -> $UPD"

# 3. bootstrap at the finalized beacon root — WS anchor (current_sync_committee + branch).
ROOT="$(jq -r '.data.finalized_header.beacon.parent_root' "$FU")"
BS="$OUT_DIR/bootstrap.json"
if curl -fsS -H 'accept: application/json' \
     "$BEACON/eth/v1/beacon/light_client/bootstrap/${ROOT}" -o "$BS"; then
  echo "bootstrap(root=$ROOT): -> $BS"
else
  echo "bootstrap fetch failed for $ROOT (node may prune); pick a checkpoint root manually" >&2
fi

cat <<EOF

Done. Committed fixtures: finality_update.json (small).
Large dumps (update_period_*.json, bootstrap.json) are gitignored — regenerate on demand.

For deterministic, fork-tagged CI vectors, also pull consensus-spec-tests:
  V=v1.5.0   # match the target fork release
  curl -fsSL -o /tmp/lc.tar.gz \\
    https://github.com/ethereum/consensus-spec-tests/releases/download/\$V/mainnet.tar.gz
  tar -xzf /tmp/lc.tar.gz --wildcards 'tests/mainnet/*/light_client/*' -C "$OUT_DIR/.."
  # gives single_merkle_proof + sync + update_ranking SSZ vectors per fork.
EOF
