#!/usr/bin/env bash
# The light client's step proving key, generated once at the start of the run, before anything
# is deployed: export_step_vk_blob with STEP_KEYGEN_ONLY=1 writes it to $LC_STEP_KEY_DIR, where
# lc-prove and lc-relayer read it and only prove. Writes LC_VK_BLOB_SHA256 (the key's VK blob,
# which must be the light client contract's VK_BLOB) and LC_PK_BYTES to /out/lc-keygen.env.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
mkdir -p "$WORK/lc-keygen" && cd "$WORK/lc-keygen"
STEP_KEYGEN_ONLY=1 STEP_KEY_DIR=$LC_STEP_KEY_DIR STEP_SRS_PATH=/params/kzg_bn254_19.srs \
  STEP_OUT_DIR=$WORK/lc-keygen export_step_vk_blob
emit lc-keygen.env "LC_VK_BLOB_SHA256=$(sha256sum "$LC_STEP_KEY_DIR/step_vk_blob.bin" | cut -c1-64)" \
  "LC_PK_BYTES=$(stat -c %s "$LC_STEP_KEY_DIR/step_pk.bin")"
log "step proving key in $LC_STEP_KEY_DIR"
