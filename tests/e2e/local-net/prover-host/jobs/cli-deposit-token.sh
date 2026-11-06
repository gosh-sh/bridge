#!/usr/bin/env bash
# cli-deposit for the run's second token (TOKEN), under its own job name so that it runs beside
# the USDC deposit. Input and arguments: as cli-deposit.sh.
exec bash /src/tests/e2e/local-net/prover-host/jobs/cli-deposit.sh
