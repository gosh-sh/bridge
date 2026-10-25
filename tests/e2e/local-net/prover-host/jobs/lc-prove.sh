#!/usr/bin/env bash
# The Sepolia light client's one trusted input, proven: eth-lc-relayer prove-one proves the
# latest finality update from the beacon chain. Its public inputs carry the commitment of the
# sync committee that signed it; lc-bootstrap sets that commitment once. Writes the bundle to
# /out/lc-bootstrap/ and LC_COMMITMENT, LC_PERIOD and LC_ATTESTED_SLOT to /out/lc.env.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
mkdir -p "$WORK/lc-prover"
log "prove-one from $BEACON_URL"
eth-lc-relayer prove-one --beacon-url "$BEACON_URL" --step-prover-bin /bin-ci/export_step_vk_blob \
  --prover-dir "$WORK/lc-prover" --srs-path /params/kzg_bn254_19.srs --out-dir "$OUT/lc-bootstrap"
pi=$OUT/lc-bootstrap/step_public_inputs.bin
# Little-endian words of 32 bytes: word 0 opens with the attested slot, word 5 is the
# committee commitment, which the contract takes as a big-endian uint256.
attested=$(od -An -t u8 -j 0 -N 8 --endian=little "$pi" | tr -d ' ')
commitment=0x$(od -An -t x1 -v -j 160 -N 32 "$pi" | tr -s ' \n' '\n' | grep . | tac | tr -d '\n')
emit lc.env "LC_ATTESTED_SLOT=$attested" "LC_PERIOD=$((attested / 8192))" "LC_COMMITMENT=$commitment"
log "committee of period $((attested / 8192)): $commitment"
