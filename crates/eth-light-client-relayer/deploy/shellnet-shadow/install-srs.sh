#!/usr/bin/env bash
# Put the Hermez k=19 KZG SRS in place and verify it.
#
# The ZKHALO2VERIFYWITHVK opcode is keyed on the Hermez ceremony, so the prover
# must use an SRS with the same tau. Two ways to obtain it:
#
#   1. Copy an existing slice (any host that runs the shellnet L2 relayer has
#      it in its params directory as kzg_bn254_19.srs):
#        install-srs.sh /path/to/kzg_bn254_19.srs
#
#   2. Downsize a bigger Hermez slice (k=20/21) tau-preservingly:
#        install-srs.sh --from-k /path/to/kzg_bn254_20.srs
#      (`eth-light-client-prover/examples/downsize_srs.rs`).
#
# Regenerating from the ceremony .ptau (scripts/bootstrap_hermez_srs.sh) is
# documented upstream but the public ptau mirrors returned 403 on 2026-09-04.

# shellcheck disable=SC1091
source "$(dirname "$0")/lib.sh"

EXPECTED_SHA="9ebbbbfc3d4899435ef254c915c62f5aa94c539bde1cec52ca7d45679d2adf4a"
DST="$ROOT/srs/kzg_bn254_19.srs"
mkdir -p "$ROOT/srs"

case "${1:-}" in
    --from-k)
        need cargo
        [[ -f "${2:-}" ]] || die "usage: $0 --from-k <kzg_bn254_2x.srs>"
        log "downsizing $2 -> k=19"
        ( cd "$SRC/eth-light-client-prover" && \
          cargo run --release --example downsize_srs -- --input "$2" --output "$DST" --k 19 )
        ;;
    "")
        [[ -f "$DST" ]] || die "no SRS at $DST; pass a source file"
        ;;
    *)
        [[ -f "$1" ]] || die "no such file: $1"
        cp -f "$1" "$DST"
        ;;
esac

chmod 444 "$DST"
actual="$(sha256sum "$DST" | awk '{print $1}')"
if [[ "$actual" == "$EXPECTED_SHA" ]]; then
    log "OK $DST sha256=$actual"
else
    log "WARNING: sha256 $actual differs from the pinned L2-relayer slice $EXPECTED_SHA."
    log "A different Hermez downsize can still be valid; the prover verifies s_g2 against the opcode constant."
fi
