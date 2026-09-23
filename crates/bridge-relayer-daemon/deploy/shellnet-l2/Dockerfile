# syntax=docker/dockerfile:1.7

FROM ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc

ARG SOURCE_COMMIT=local
ARG RELAYER_UID=998
ARG RELAYER_GID=998

LABEL org.opencontainers.image.title="GOSH shellnet to Sepolia L2 relayer" \
      org.opencontainers.image.revision="$SOURCE_COMMIT" \
      org.opencontainers.image.source="https://github.com/gosh-sh/bridge"

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
      ca-certificates curl jq libgcc-s1 xxd \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid "$RELAYER_GID" gosh-relayer \
    && useradd --uid "$RELAYER_UID" --gid "$RELAYER_GID" \
      --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin gosh-relayer

RUN install -d -m 0755 \
      /opt/gosh-relayer/bin \
      /opt/gosh-relayer/config \
      /opt/gosh-relayer/prover \
      /opt/gosh-relayer/aggregator/target/release \
      /opt/gosh-relayer/verifiers

COPY --from=relayer_release /relayer /opt/gosh-relayer/bin/relayer
COPY --from=aggregator_release /aggregate-proof \
  /opt/gosh-relayer/aggregator/target/release/aggregate-proof
COPY --from=verifier_bins /PrimaryAggregatorVerifier.bin \
  /opt/gosh-relayer/verifiers/PrimaryAggregatorVerifier.bin
COPY --from=verifier_bins /FallbackAggregatorVerifier.bin \
  /opt/gosh-relayer/verifiers/FallbackAggregatorVerifier.bin
COPY --from=verifier_bins /LayerHashesAggregatorVerifier.bin \
  /opt/gosh-relayer/verifiers/LayerHashesAggregatorVerifier.bin
COPY --from=verifier_bins /BridgeWithdrawalAggregatorVerifier.bin \
  /opt/gosh-relayer/verifiers/BridgeWithdrawalAggregatorVerifier.bin
COPY --from=verifier_bins /PrimaryAggregatorVerifier.sol \
  /opt/gosh-relayer/verifiers/PrimaryAggregatorVerifier.sol
COPY --from=verifier_bins /FallbackAggregatorVerifier.sol \
  /opt/gosh-relayer/verifiers/FallbackAggregatorVerifier.sol
COPY --from=verifier_bins /LayerHashesAggregatorVerifier.sol \
  /opt/gosh-relayer/verifiers/LayerHashesAggregatorVerifier.sol
COPY --from=verifier_bins /BridgeWithdrawalAggregatorVerifier.sol \
  /opt/gosh-relayer/verifiers/BridgeWithdrawalAggregatorVerifier.sol
COPY --from=foundry_bin /cast /opt/gosh-relayer/bin/cast

COPY crates/bridge-prover-libraries/bk_set.shellnet.json \
  /opt/gosh-relayer/config/bk_set.shellnet.json
COPY crates/bridge-relayer-daemon/deploy/shellnet-l2/scripts/container-entrypoint.sh \
  /opt/gosh-relayer/bin/container-entrypoint.sh
COPY crates/bridge-relayer-daemon/deploy/shellnet-l2/scripts/container-paths.sh \
  /opt/gosh-relayer/bin/container-paths.sh
COPY crates/bridge-relayer-daemon/deploy/shellnet-l2/scripts/preflight.sh \
  /opt/gosh-relayer/bin/preflight.sh
COPY crates/bridge-relayer-daemon/deploy/shellnet-l2/scripts/verify-params.sh \
  /opt/gosh-relayer/bin/verify-params.sh

RUN printf '%s\n' "$SOURCE_COMMIT" >/opt/gosh-relayer/SOURCE_COMMIT \
    && chmod 0555 /opt/gosh-relayer/bin/relayer \
      /opt/gosh-relayer/aggregator/target/release/aggregate-proof \
      /opt/gosh-relayer/bin/container-entrypoint.sh \
      /opt/gosh-relayer/bin/container-paths.sh \
      /opt/gosh-relayer/bin/preflight.sh \
      /opt/gosh-relayer/bin/verify-params.sh \
      /opt/gosh-relayer/bin/cast \
    && cd /opt/gosh-relayer \
    && sha256sum \
      bin/relayer \
      aggregator/target/release/aggregate-proof \
      verifiers/PrimaryAggregatorVerifier.bin \
      verifiers/FallbackAggregatorVerifier.bin \
      verifiers/LayerHashesAggregatorVerifier.bin \
      verifiers/BridgeWithdrawalAggregatorVerifier.bin \
      verifiers/PrimaryAggregatorVerifier.sol \
      verifiers/FallbackAggregatorVerifier.sol \
      verifiers/LayerHashesAggregatorVerifier.sol \
      verifiers/BridgeWithdrawalAggregatorVerifier.sol \
      config/bk_set.shellnet.json \
      bin/cast bin/container-entrypoint.sh bin/container-paths.sh \
      bin/preflight.sh bin/verify-params.sh >IMAGE-SHA256SUMS \
    && chmod 0444 /opt/gosh-relayer/SOURCE_COMMIT \
      /opt/gosh-relayer/IMAGE-SHA256SUMS \
      /opt/gosh-relayer/config/bk_set.shellnet.json \
      /opt/gosh-relayer/verifiers/*.bin \
      /opt/gosh-relayer/verifiers/*.sol \
    && sha256sum --check --strict IMAGE-SHA256SUMS

WORKDIR /opt/gosh-relayer/prover
ENV PATH="/opt/gosh-relayer/bin:${PATH}"
USER ${RELAYER_UID}:${RELAYER_GID}
ENTRYPOINT ["/opt/gosh-relayer/bin/container-entrypoint.sh"]
CMD ["run"]
