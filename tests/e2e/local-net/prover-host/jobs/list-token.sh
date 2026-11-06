#!/usr/bin/env bash
# The run's second token (ListE2EToken.s.sol): a fresh ERC-20 on Arbitrum Sepolia, listed on the
# run's bridge as ECC id 4 with deposits open. Input: the deployer's private key. Writes
# token.env: TOKEN_ADDRESS.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load evm.env
export PRIVATE_KEY; PRIVATE_KEY=$(tr -d '[:space:]' </run/input)
cd "$WORK/evm"
forge script script/ListE2EToken.s.sol:ListE2EToken --rpc-url "$ARB_RPC" --broadcast --slow | tee "$WORK/list-token.out"
sed -n 's/^ *Token: \(0x[0-9a-fA-F]\{40\}\)$/TOKEN_ADDRESS=\1/p' "$WORK/list-token.out" >"$OUT/token.env"
cat "$OUT/token.env"
