# syntax=docker/dockerfile:1.7
#
# Every bridge service image, built from the sources of one commit
# (bridge-audit#135). Build context: the repository root.
#
#   docker build -f docker/Dockerfile --target bridge-relayer         -t bridge-relayer .
#   docker build -f docker/Dockerfile --target bridge-deposit-relayer -t bridge-deposit-relayer .
#   docker build -f docker/Dockerfile --target bridge-eth-lc-relayer  -t bridge-eth-lc-relayer .
#   docker build -f docker/Dockerfile --target bridge-cli             -t bridge-cli .
#   docker build -f docker/Dockerfile --target tools --output type=local,dest=out .
#
# `tools` is every binary the images carry, as plain files: the local-network
# e2e builds it once and runs the one-shot jobs from it. The builder stages
# share one cargo home (registry, git checkouts and cargo's own lock file, so
# the stages that build at the same time do not race on a checkout) and one
# target directory per workspace through BuildKit cache mounts, so a rebuild
# after a small change is short.
#
# The runtime base is debian:bookworm-slim, the same glibc as the rust:*-bookworm
# builder: a binary built against a newer glibc than the runtime does not start.
# Every image runs as the unprivileged `bridge` user; what the services write
# goes to /data (state, work, logs) and /data/params, which the compose file
# bind-mounts (bridge-audit#132).

ARG RUST_IMAGE=rust:1-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

# ---------------------------------------------------------------------------
# Sources, once, with the two pinned toolchains installed up front: every
# builder stage inherits them instead of downloading them on its first cargo
# call (one layer, cached while the rust-toolchain.toml files do not change).
# ---------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS source
# The cargo home is a cache mount in every builder stage: the lock file must
# live in it, or two stages fetching the same git dependency corrupt each
# other's checkout.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true \
    CARGO_HOME=/cargo
WORKDIR /src
COPY rust-toolchain.toml ./
COPY deposit-prover/rust-toolchain.toml deposit-prover/
# `rustup` itself lives in the image's cargo home, hence the explicit
# CARGO_HOME for this one command; cargo's proxies find the toolchains in
# RUSTUP_HOME whatever CARGO_HOME is.
RUN channel() { sed -n 's/^channel = "\(.*\)"/\1/p' "$1"; } \
    && CARGO_HOME=/usr/local/cargo rustup toolchain install "$(channel rust-toolchain.toml)" \
         --profile minimal --component rustfmt --component clippy \
    && CARGO_HOME=/usr/local/cargo rustup toolchain install \
         "$(channel deposit-prover/rust-toolchain.toml)" --profile minimal
COPY . .

# relayer daemon-live, compute_bridge_anchors, the CLI and the BK-set seeder:
# the bridge-prover-libraries workspace, root rust-toolchain.toml.
# CLI_FEATURES: cargo features for ackinacki-bridge; the local-network e2e
# passes `ackinacki-bridge/dev-unfixed-bridge`, a release passes none.
FROM source AS build-prover-libs
ARG CLI_FEATURES=
RUN --mount=type=cache,id=cargo-home,target=/cargo \
    --mount=type=cache,id=target-prover-libs,target=/src/crates/bridge-prover-libraries/target \
    cd crates/bridge-prover-libraries \
    && cargo build --release --locked \
         -p bridge-relayer-daemon --bin relayer \
         -p bridge-prover-lib --bin compute_bridge_anchors \
         -p ackinacki-bridge --bin ackinacki-bridge ${CLI_FEATURES:+--features "$CLI_FEATURES"} \
    && cargo build --release --locked -p bridge-prover-daemon --example seed_prover_bk_set \
    && install -D -m 0755 -t /out \
         target/release/relayer target/release/compute_bridge_anchors \
         target/release/ackinacki-bridge target/release/examples/seed_prover_bk_set

# aggregate-proof: the SHPLONK aggregator the withdraw relayer and the CLI run.
FROM source AS build-aggregator
RUN --mount=type=cache,id=cargo-home,target=/cargo \
    --mount=type=cache,id=target-aggregator,target=/src/crates/bridge-evm-aggregator/target \
    cd crates/bridge-evm-aggregator \
    && cargo build --release --locked --bin aggregate-proof \
    && install -D -m 0755 -t /out target/release/aggregate-proof

# The three deposit-prover tools the deposit relayer and the CLI start, plus
# downsize_srs for the params setup. Its own toolchain (nightly-2026-02-03).
FROM source AS build-deposit-prover
RUN --mount=type=cache,id=cargo-home,target=/cargo \
    --mount=type=cache,id=target-deposit-prover,target=/src/deposit-prover/target \
    cd deposit-prover \
    && cargo build --release --locked \
         --example fetch_deposit_data --example export_vk_blob \
         --example export_blake2b_proof --example downsize_srs \
    && install -D -m 0755 -t /out \
         target/release/examples/fetch_deposit_data target/release/examples/export_vk_blob \
         target/release/examples/export_blake2b_proof target/release/examples/downsize_srs \
    && install -D -m 0644 -t /out configs/circuit_params.json

FROM source AS build-deposit-relayer
RUN --mount=type=cache,id=cargo-home,target=/cargo \
    --mount=type=cache,id=target-deposit-relayer,target=/src/crates/deposit-relayer-daemon/target \
    cd crates/deposit-relayer-daemon \
    && cargo build --release --locked \
    && install -D -m 0755 -t /out target/release/deposit-relayer

# eth-lc-relayer with live-submit (it sends to Acki Nacki), and the two
# light-client provers it starts: the step prover and the rotate prover.
FROM source AS build-lc-relayer
RUN --mount=type=cache,id=cargo-home,target=/cargo \
    --mount=type=cache,id=target-lc-relayer,target=/src/crates/eth-light-client-relayer/target \
    cd crates/eth-light-client-relayer \
    && cargo build --release --locked --features live-submit --bin eth-lc-relayer \
    && install -D -m 0755 -t /out target/release/eth-lc-relayer

FROM source AS build-lc-prover
RUN --mount=type=cache,id=cargo-home,target=/cargo \
    --mount=type=cache,id=target-lc-prover,target=/src/eth-light-client-prover/target \
    cd eth-light-client-prover \
    && cargo build --release --locked --example export_step_vk_blob --example downsize_srs \
    && cargo build --release --locked --features aggregation \
         --example export_shard_snark --example export_step_snark --example rotate_tree_n8 \
    && install -D -m 0755 -t /out \
         target/release/examples/export_step_vk_blob target/release/examples/export_shard_snark \
         target/release/examples/export_step_snark target/release/examples/rotate_tree_n8 \
    && install -D -m 0755 target/release/examples/downsize_srs /out/lc/downsize_srs

# cast and forge: the relayer's preflight reads the chain with cast, the CLI
# image carries both for the operator. Pinned release, sha256-checked.
FROM ${RUNTIME_IMAGE} AS foundry
ARG FOUNDRY_VERSION=v1.7.1
ARG FOUNDRY_SHA256=cf7e688ed0c4c48adffca788b496076e31060b67ac5afe1e43dbb5499c20c88b
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL -o /tmp/foundry.tgz \
         "https://github.com/foundry-rs/foundry/releases/download/${FOUNDRY_VERSION}/foundry_${FOUNDRY_VERSION}_linux_amd64.tar.gz" \
    && echo "${FOUNDRY_SHA256}  /tmp/foundry.tgz" | sha256sum -c - \
    && mkdir -p /out && tar -xzf /tmp/foundry.tgz -C /out cast forge && rm /tmp/foundry.tgz

# ---------------------------------------------------------------------------
# Every binary as plain files (docker build --target tools --output ...).
# ---------------------------------------------------------------------------
FROM scratch AS tools
COPY --from=build-prover-libs /out/ /
COPY --from=build-aggregator /out/ /
COPY --from=build-deposit-prover /out/ /
COPY --from=build-deposit-relayer /out/ /
COPY --from=build-lc-relayer /out/ /
COPY --from=build-lc-prover /out/ /
COPY --from=foundry /out/ /

# ---------------------------------------------------------------------------
# Runtime base: the unprivileged user, the few tools the scripts need, the
# directory skeleton the compose file mounts over.
# ---------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS runtime-base
ARG BRIDGE_UID=998
ARG BRIDGE_GID=998
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl jq xxd libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid "${BRIDGE_GID}" bridge \
    && useradd --uid "${BRIDGE_UID}" --gid "${BRIDGE_GID}" \
         --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin bridge \
    && install -d -m 0755 /opt/bridge/bin /data \
    && install -d -m 0755 -o "${BRIDGE_UID}" -g "${BRIDGE_GID}" \
         /data/state /data/work /data/logs /data/params
ENV PATH="/opt/bridge/bin:${PATH}"
# The commit after the package layer, so a new commit does not rebuild it.
ARG SOURCE_COMMIT=local
LABEL org.opencontainers.image.source="https://github.com/gosh-sh/bridge" \
      org.opencontainers.image.revision="${SOURCE_COMMIT}"
RUN printf '%s\n' "${SOURCE_COMMIT}" >/opt/bridge/SOURCE_COMMIT && chmod 0444 /opt/bridge/SOURCE_COMMIT

# relayer daemon-live: the withdraw relayer, with its preflight. The BK set
# file is a mounted input (/run/config/bk_set.json), not part of the image.
FROM runtime-base AS bridge-relayer
LABEL org.opencontainers.image.title="GOSH bridge: Acki Nacki -> EVM relayer (daemon-live)"
COPY --from=build-prover-libs /out/relayer /out/compute_bridge_anchors /out/seed_prover_bk_set /opt/bridge/bin/
COPY --from=build-aggregator /out/aggregate-proof /opt/bridge/bin/
COPY --from=foundry /out/cast /opt/bridge/bin/
COPY contracts/ethereum/verifiers/PrimaryAggregatorVerifier.bin \
     contracts/ethereum/verifiers/FallbackAggregatorVerifier.bin \
     contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.bin \
     contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin \
     contracts/ethereum/verifiers/PrimaryAggregatorVerifier.sol \
     contracts/ethereum/verifiers/FallbackAggregatorVerifier.sol \
     contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.sol \
     contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.sol \
     /opt/bridge/verifiers/
COPY docker/relayer/container-entrypoint.sh docker/relayer/container-paths.sh \
     docker/relayer/preflight.sh docker/relayer/verify-params.sh /opt/bridge/bin/
RUN chmod 0555 /opt/bridge/bin/* && chmod 0444 /opt/bridge/verifiers/* \
    && cd /opt/bridge \
    && sha256sum bin/* verifiers/* >IMAGE-SHA256SUMS \
    && chmod 0444 IMAGE-SHA256SUMS \
    && sha256sum --check --strict IMAGE-SHA256SUMS
USER bridge
WORKDIR /data/work
ENTRYPOINT ["/opt/bridge/bin/container-entrypoint.sh"]
CMD ["run"]

# deposit-relayer daemon with the deposit-prover tools it starts. The
# proving key and the SRS live in /data/params (DEPOSIT_PROVER_DIR).
FROM runtime-base AS bridge-deposit-relayer
LABEL org.opencontainers.image.title="GOSH bridge: EVM -> Acki Nacki deposit relayer"
COPY --from=build-deposit-relayer /out/deposit-relayer /opt/bridge/bin/
COPY --from=build-deposit-prover /out/fetch_deposit_data /out/export_vk_blob \
     /out/export_blake2b_proof /out/downsize_srs /opt/bridge/bin/
COPY --from=build-deposit-prover /out/circuit_params.json /opt/bridge/deposit-prover/configs/
# The tools read configs/circuit_params.json and data/ relative to their
# working directory (DEPOSIT_PROVER_DIR): the config is in the image, data/
# is the mounted params (the SRS, and the proving key the first proof writes).
RUN chmod 0555 /opt/bridge/bin/* \
    && ln -s /data/params /opt/bridge/deposit-prover/data \
    && cd /opt/bridge && sha256sum bin/* >IMAGE-SHA256SUMS && chmod 0444 IMAGE-SHA256SUMS
ENV DEPOSIT_FETCH_DEPOSIT_DATA_BIN=/opt/bridge/bin/fetch_deposit_data \
    DEPOSIT_EXPORT_VK_BLOB_BIN=/opt/bridge/bin/export_vk_blob \
    DEPOSIT_EXPORT_BLAKE2B_PROOF_BIN=/opt/bridge/bin/export_blake2b_proof \
    DEPOSIT_PROVER_DIR=/opt/bridge/deposit-prover \
    DEPOSIT_RELAYER_STATE_DIR=/data/state \
    DEPOSIT_RELAYER_WORK_DIR=/data/work \
    DEPOSIT_RELAYER_LOG_DIR=/data/logs
USER bridge
WORKDIR /data/work
ENTRYPOINT ["/opt/bridge/bin/deposit-relayer"]
CMD ["daemon"]

# eth-lc-relayer: the beacon light-client daemon and the Arbitrum block
# relayer (arbitrum-anchors) are two commands of this binary; the step
# prover and the three rotate stages it starts are beside it. SRS, the step
# key and the rotate keys (rotate-keys/) live in /data/params.
FROM runtime-base AS bridge-eth-lc-relayer
LABEL org.opencontainers.image.title="GOSH bridge: Ethereum light-client and Arbitrum block relayer"
COPY --from=build-lc-relayer /out/eth-lc-relayer /opt/bridge/bin/
COPY --from=build-lc-prover /out/export_step_vk_blob /out/export_shard_snark \
     /out/export_step_snark /out/rotate_tree_n8 /out/lc/downsize_srs /opt/bridge/bin/
RUN chmod 0555 /opt/bridge/bin/* \
    && cd /opt/bridge && sha256sum bin/* >IMAGE-SHA256SUMS && chmod 0444 IMAGE-SHA256SUMS
ENV LIGHT_CLIENT_STEP_PROVER_BIN=/opt/bridge/bin/export_step_vk_blob \
    LIGHT_CLIENT_SHARD_PROVER_BIN=/opt/bridge/bin/export_shard_snark \
    LIGHT_CLIENT_STEP_SNARK_PROVER_BIN=/opt/bridge/bin/export_step_snark \
    LIGHT_CLIENT_ROTATE_PROVER_BIN=/opt/bridge/bin/rotate_tree_n8 \
    LIGHT_CLIENT_PROVER_DIR=/data/params \
    STEP_SRS_PATH=/data/params/kzg_bn254_19.srs \
    STEP_KEY_DIR=/data/params/step-key \
    ETH_LC_RELAYER_STATE_DIR=/data/state \
    ETH_LC_RELAYER_WORK_DIR=/data/work \
    ETH_LC_RELAYER_LOG_DIR=/data/logs \
    ARBITRUM_ANCHORS_STATE_DIR=/data/state \
    ARBITRUM_ANCHORS_WORK_DIR=/data/work \
    ARBITRUM_ANCHORS_LOG_DIR=/data/logs
USER bridge
WORKDIR /data/work
ENTRYPOINT ["/opt/bridge/bin/eth-lc-relayer"]
CMD ["daemon"]

# The operator's CLI with everything a deposit or a withdrawal runs:
# the deposit-prover tools, the aggregator, cast and forge.
FROM runtime-base AS bridge-cli
LABEL org.opencontainers.image.title="GOSH bridge: ackinacki-bridge CLI"
COPY --from=build-prover-libs /out/ackinacki-bridge /out/compute_bridge_anchors /opt/bridge/bin/
COPY --from=build-aggregator /out/aggregate-proof /opt/bridge/bin/
COPY --from=build-deposit-prover /out/fetch_deposit_data /out/export_vk_blob \
     /out/export_blake2b_proof /out/downsize_srs /opt/bridge/bin/
COPY --from=build-deposit-prover /out/circuit_params.json /opt/bridge/deposit-prover/configs/
COPY --from=foundry /out/cast /out/forge /opt/bridge/bin/
COPY contracts/ethereum/verifiers/PrimaryAggregatorVerifier.bin \
     contracts/ethereum/verifiers/FallbackAggregatorVerifier.bin \
     contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.bin \
     contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.bin \
     contracts/ethereum/verifiers/PrimaryAggregatorVerifier.sol \
     contracts/ethereum/verifiers/FallbackAggregatorVerifier.sol \
     contracts/ethereum/verifiers/LayerHashesAggregatorVerifier.sol \
     contracts/ethereum/verifiers/BridgeWithdrawalAggregatorVerifier.sol \
     /opt/bridge/verifiers/
# The CLI looks for the deposit prover's tools inside its prover directory
# (configs/ in the image, data/ the mounted params) and for `aggregate-proof`
# under `<aggregator dir>/target/release`, the layout of a source checkout:
# links give the image that layout. The params are given by flags or env/cli.env.
RUN chmod 0555 /opt/bridge/bin/* && chmod 0444 /opt/bridge/verifiers/* \
    && ln -s /data/params/deposit-prover /opt/bridge/deposit-prover/data \
    && ln -s /opt/bridge/bin/fetch_deposit_data /opt/bridge/deposit-prover/fetch_deposit_data \
    && ln -s /opt/bridge/bin/export_blake2b_proof /opt/bridge/deposit-prover/export_blake2b_proof \
    && mkdir -p /opt/bridge/aggregator/target/release \
    && ln -s /opt/bridge/bin/aggregate-proof /opt/bridge/aggregator/target/release/aggregate-proof \
    && cd /opt/bridge && sha256sum bin/* verifiers/* >IMAGE-SHA256SUMS && chmod 0444 IMAGE-SHA256SUMS
ENV BRIDGE_DEPOSIT_PROVER_DIR=/opt/bridge/deposit-prover \
    BRIDGE_AGGREGATOR_DIR=/opt/bridge/aggregator \
    BRIDGE_VERIFIERS_DIR=/opt/bridge/verifiers
USER bridge
WORKDIR /data/work
ENTRYPOINT ["/opt/bridge/bin/ackinacki-bridge"]
CMD ["--help"]
