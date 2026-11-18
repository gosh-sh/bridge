#!/usr/bin/env bash
# cli-deposit for NACKL, as a job of its own: its own log, beside the USDC deposit's.
export TOKEN=nackl
exec bash /src/tests/e2e/local-net/prover-host/jobs/cli-deposit.sh
