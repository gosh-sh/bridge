#!/usr/bin/env bash
# Sets the Sepolia light client up once, as docs/eth-light-client.md does: the Gloas fork slot,
# the committee lc-prove proved (set-committee), then the owner flip (flip-owner:
# disableOwnerAnchorsOn(11155111) on the bridge and disableOwnerRotation on the light
# client). After it the owner can neither accept a Sepolia block nor change the committee.
# Input: the bridge owner's tvm keys JSON; deployLightClient gave the light client its pubkey.
# Args: LC, the light client's address.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
cp /run/input "$WORK/lc-owner.keys.json"
an=(--an-graphql-url "$GQL" --an-keys-path "$WORK/lc-owner.keys.json"
    --an-light-client "$LC" --an-sender "$LC")
eth-lc-relayer set-gloas-fork-slot --slot "$SEPOLIA_GLOAS_FORK_SLOT" "${an[@]}"
eth-lc-relayer set-committee --bundle-dir "$OUT/lc-bootstrap" --state-dir "$OUT/lc-state" "${an[@]}"
eth-lc-relayer flip-owner "${an[@]}" --an-usdc-bridge "$AN_BRIDGE"
log "light client set up; owner rotation and owner anchors on 11155111 are off"
