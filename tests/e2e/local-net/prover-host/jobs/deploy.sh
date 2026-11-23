#!/usr/bin/env bash
# A fresh AckiNackiBridge on Arbitrum Sepolia, paired with this run's local network.
# Input: the deployer's private key. Writes evm.env: BRIDGE_ADDRESS, BRIDGE_DEPLOY_BLOCK.
. /src/tests/e2e/local-net/prover-host/jobs/common.sh
load genesis.env
export PRIVATE_KEY; PRIVATE_KEY=$(tr -d '[:space:]' </run/input)
export GENESIS_LAST_SEEN_BLOCK_SEQNO=$GENESIS_SEED_SEQNO
export WITHDRAW_ACC_FR=0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a WITHDRAW_DAPP_FR=0
export USDC_TOKEN_ID=3 USDC_ADDRESS=$ARB_USDC USDC_TVL_CAP=10000000000 USDC_DAILY_LIMIT=10000000000
# This run's local network has paid nothing through an earlier bridge.
export PREDECESSOR_BRIDGE=0x0000000000000000000000000000000000000000
rm -rf "$WORK/evm" && cp -r /bin-ci/evm "$WORK/evm" && cd "$WORK/evm"
log "deployer $(cast wallet address --private-key "$PRIVATE_KEY"), $(cast balance --ether "$(cast wallet address --private-key "$PRIVATE_KEY")" --rpc-url "$ARB_RPC") ETH"
forge script script/DeployShellnetE2EBridge.s.sol:DeployShellnetE2EBridge --rpc-url "$ARB_RPC" --broadcast --slow \
  | tee "$WORK/forge.out"
# evm.env: the bridge, its deploy block, and every address the script logs ("Name: 0x…").
python3 - "$WORK/evm/broadcast/DeployShellnetE2EBridge.s.sol/421614/run-latest.json" "$WORK/forge.out" \
  >"$OUT/evm.env" <<'PY'
import json, re, sys
run = json.load(open(sys.argv[1]))
bridge = [t["contractAddress"] for t in run["transactions"] if t.get("contractName") == "AckiNackiBridge"][-1]
blocks = [int(r["blockNumber"], 16) for r in run["receipts"]]
print(f"BRIDGE_ADDRESS={bridge}\nBRIDGE_DEPLOY_BLOCK={min(blocks)}")
for name, addr in re.findall(r"^\s+([A-Za-z]+): (0x[0-9a-fA-F]{40})$", open(sys.argv[2]).read(), re.M):
    # Logged, not deployed by this run: Circle's USDC, the AAVE aToken and the (here zero) predecessor bridge.
    if name not in ("USDC", "aUSDC", "predecessor"):
        print(f"ADDR_{name.upper()}={addr}")
PY
export NACKL_BRIDGE; NACKL_BRIDGE=$(sed -n 's/^BRIDGE_ADDRESS=//p' "$OUT/evm.env")
export NACKL_INITIAL_HOLDER; NACKL_INITIAL_HOLDER=$(cast wallet address --private-key "$PRIVATE_KEY")
export NACKL_INITIAL_SUPPLY=1000000000000 NACKL_LIST=true NACKL_DAILY_LIMIT=1000000000000
forge script script/DeployNacklToken.s.sol:DeployNacklToken --rpc-url "$ARB_RPC" --broadcast --slow \
  | tee "$WORK/forge-nackl.out"
nackl=$(sed -n 's/^ *NacklToken: \(0x[0-9a-fA-F]\{40\}\)$/\1/p' "$WORK/forge-nackl.out" | tail -n 1)
[ -n "$nackl" ] || { echo "deploy.sh: DeployNacklToken logged no address" >&2; exit 1; }
echo "NACKL_ADDRESS=$nackl" >>"$OUT/evm.env"
cat "$OUT/evm.env"
