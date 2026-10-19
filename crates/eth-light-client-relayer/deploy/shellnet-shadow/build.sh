#!/usr/bin/env bash
# Build the prover example (what `prove-one`/daemon spawn) and the relayer with
# live AN submit, then install the relayer binary into $ROOT/bin.
#
# Toolchain: the repo pins nightly-2026-08-03 in rust-toolchain.toml; rustup
# picks it up automatically (`rustup toolchain install nightly-2026-08-03`).
# The prover needs the pin too: this script builds `export_step_vk_blob` and
# the daemon runs that binary (LIGHT_CLIENT_STEP_PROVER_BIN in the env file).

# shellcheck disable=SC1091
source "$(dirname "$0")/lib.sh"
need cargo

log "prover: cargo build --release --example export_step_vk_blob"
( cd "$SRC/eth-light-client-prover" && cargo build --release --example export_step_vk_blob )

log "relayer: cargo build --release --features live-submit"
( cd "$SRC/crates/eth-light-client-relayer" && cargo build --release --features live-submit )

mkdir -p "$BIN"
install -m 755 "$SRC/crates/eth-light-client-relayer/target/release/eth-lc-relayer" "$RELAYER"
log "installed $RELAYER"
"$RELAYER" --help | head -3
sha256sum "$RELAYER"
log "step prover: $SRC/eth-light-client-prover/target/release/examples/export_step_vk_blob (LIGHT_CLIENT_STEP_PROVER_BIN)"
