#!/usr/bin/env bash
# Check out acki-nacki at one commit ON the e2e host (the playbook's own clone is
# skipped: acki_repo is passed empty) and apply the bridge's patch to
# tests/dexdo/up.sh. Piped over ssh by .woodpecker/local-net-e2e.yaml (`bash -s`).
#
# Required env: ACKI_REPO (https clone URL), ACKI_SHA, ACKI_DIR, GITHUB_TOKEN (read),
# PATCH (path of up.sh.patch on the host, copied there by the pipeline). The token
# lives in ~/.netrc only for the fetch and is removed on every exit path.
set -euo pipefail
: "${ACKI_REPO:?}" "${ACKI_SHA:?}" "${ACKI_DIR:?}" "${GITHUB_TOKEN:?}" "${PATCH:?}"
cleanup() { rm -f ~/.netrc; }
trap cleanup EXIT
sudo mkdir -p "$ACKI_DIR" && sudo chown "$(id -u):$(id -g)" "$ACKI_DIR"
if [ ! -d "$ACKI_DIR/.git" ]; then git init -q "$ACKI_DIR"; git -C "$ACKI_DIR" remote add origin "$ACKI_REPO"; fi
git -C "$ACKI_DIR" remote set-url origin "$ACKI_REPO"
printf 'machine github.com\nlogin x-access-token\npassword %s\n' "$GITHUB_TOKEN" > ~/.netrc && chmod 600 ~/.netrc
git -C "$ACKI_DIR" fetch -q --depth 1 origin "$ACKI_SHA"
rm -f ~/.netrc
git -C "$ACKI_DIR" checkout -q -f FETCH_HEAD
git -C "$ACKI_DIR" clean -ffdxq -e config -e logs -e docker/.env
[ "$(git -C "$ACKI_DIR" rev-parse HEAD)" = "$ACKI_SHA" ] || { echo "FATAL: checkout HEAD != ACKI_SHA"; exit 66; }
# The patch is a stop-gap until acki-nacki carries the change; a checkout it no
# longer applies to is a reason to look, not to proceed.
if ! git -C "$ACKI_DIR" apply --check "$PATCH"; then echo "FATAL: $PATCH does not apply to acki-nacki $ACKI_SHA tests/dexdo/up.sh: upstream changed, re-base the patch or drop it"; exit 67; fi
git -C "$ACKI_DIR" apply "$PATCH"
echo "acki-nacki $ACKI_SHA checked out at $ACKI_DIR, up.sh patched"
