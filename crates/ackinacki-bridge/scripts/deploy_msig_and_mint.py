"""
Minimal driver that deploys a fresh `UpdateCustodianMultisigWallet` on
the target AN cluster and seeds it with `WITHDRAWAL_AMOUNT` USDC
(ECC[3]) via `USDCBridge.mintAndSend`, then prints two eval-able env
lines on stdout for `bridge-withdraw-e2e-cli` to consume:

    export WITHDRAW_FROM=<dapp_id>::<account_id>
    export WITHDRAW_FROM_KEYS=<absolute path to owner keys.json>

Everything else — tvm-cli output, tracer log lines — goes to stderr so
`eval "$(scripts/deploy_msig_and_mint.sh)"` produces a shell that has
the two vars set with no stray output.

Reuses `python/helper/msig.py::deploy_multisig`, which is the same
function the full-orchestrator drivers (`test_deploy_and_withdraw_only`,
`generate_withdrawals_with_live_event_proving`) use. This driver stops
at the ECC[3] mint — it does NOT fire `initiateWithdrawal`, does NOT
wait for a WithdrawalInitiated event, and does NOT run any Rust binary.

Env vars honored (all optional; safe shellnet defaults):
  MODE                    "shellnet" (default) or "local"
  NETWORK                 tvm-cli --url target (shellnet default is
                          "shellnet.ackinacki.org"; local defaults to
                          "http://127.0.0.1:80")
  GRAPHQL_URL             GQL endpoint used only for resolving the
                          USDCBridge dapp_id inside `mint_usdc`
  WORK_DIR                where the multisig .keys.json + deployx
                          artifacts land (default: ./work_dir under the
                          CLI crate root)
  USDC_BRIDGE_KEY_PATH    override the bundled bridge-owner key path

Prints diagnostics + tvm-cli output to stderr; only the two `export …`
lines go to stdout.
"""

import os
import sys
import time

# Locate the sibling `python/` tree (helper.msig + helper.bridge_e2e
# + helper.common all live there, and bin/tvm-cli is symlinked below it).
_HERE = os.path.dirname(os.path.abspath(__file__))
_CLI_ROOT = os.path.dirname(_HERE)                          # crates/bridge-withdraw-e2e-cli
_PY_ROOT = os.path.abspath(os.path.join(
    _CLI_ROOT, "..", "an-bridge-prover", "python"))         # crates/an-bridge-prover/python
if not os.path.isdir(_PY_ROOT):
    print(f"[deploy_msig_and_mint] cannot find {_PY_ROOT}", file=sys.stderr)
    sys.exit(2)
sys.path.insert(0, _PY_ROOT)
os.environ["PATH"] = os.path.join(_PY_ROOT, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common                                    # noqa: E402
from helper import bridge_e2e as be                          # noqa: E402
from helper.bridge_e2e import (                              # noqa: E402
    USDC_BRIDGE_ADDRESS, USDC_BRIDGE_KEYS, USDC_BRIDGE_KEYS_SHELLNET,
    materialize_usdc_bridge_key_from_node_config,
    validate_usdc_bridge_key,
)
from helper.msig import deploy_multisig                      # noqa: E402


def _log(msg: str):
    print(msg, file=sys.stderr, flush=True)


def main():
    mode = os.environ.get("MODE", "shellnet").lower()
    if mode not in ("shellnet", "local"):
        _log(f"MODE must be 'shellnet' or 'local', got {mode!r}")
        sys.exit(2)
    is_shellnet = mode == "shellnet"

    if is_shellnet:
        default_network = "shellnet.ackinacki.org"
        default_graphql = "https://shellnet.ackinacki.org/graphql"
        default_bridge_key = USDC_BRIDGE_KEYS_SHELLNET
        gql_kwargs = {"user_agent": "bridge-withdraw-e2e-cli-deploy-msig/1.0",
                      "timeout": 30}
    else:
        default_network = "http://127.0.0.1:80"
        default_graphql = "http://localhost/graphql"
        default_bridge_key = USDC_BRIDGE_KEYS
        gql_kwargs = {}

    network = os.environ.get("NETWORK", default_network)
    graphql = os.environ.get("GRAPHQL_URL", default_graphql)
    work_dir = os.environ.get("WORK_DIR", os.path.join(_CLI_ROOT, "work_dir"))
    key_override = os.environ.get("USDC_BRIDGE_KEY_PATH")
    usdc_bridge_key = key_override or default_bridge_key

    os.makedirs(work_dir, exist_ok=True)

    # tvm-cli setup — mirrors test_deploy_and_withdraw_only.py:109-113.
    os.environ["NETWORK"] = network
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()
    time.sleep(1)

    tracer = be.Tracer()
    gql = be.GqlClient(graphql, **gql_kwargs)

    tracer.log_phase(f"deploy_msig_and_mint prechecks (mode={mode})")
    tracer.log(f"  NETWORK:              {network}")
    tracer.log(f"  GRAPHQL_URL:          {graphql}")
    tracer.log(f"  WORK_DIR:             {work_dir}")
    tracer.log(f"  USDC_BRIDGE_KEY_PATH: {usdc_bridge_key}")

    if not common.is_account_active(USDC_BRIDGE_ADDRESS):
        _log(f"USDCBridge is not active at {USDC_BRIDGE_ADDRESS}; aborting")
        sys.exit(3)
    tracer.log(f"  USDCBridge active at {USDC_BRIDGE_ADDRESS}")

    # Local devnet: refresh USDCBridge.keys.json from node config unless
    # the caller explicitly overrode the path. Shellnet uses the pinned
    # partner-provided file.
    if not is_shellnet and key_override is None:
        tracer.log_phase("Materializing USDCBridge.keys.json from local cluster config")
        prover_dir = os.path.dirname(_PY_ROOT)   # crates/an-bridge-prover
        materialize_usdc_bridge_key_from_node_config(tracer, prover_dir, usdc_bridge_key)

    tracer.log_phase("Validating USDCBridge owner key against on-chain getOwnerPubkey")
    validate_usdc_bridge_key(tracer, usdc_bridge_key, overridden=key_override is not None)

    msig_key_path = os.path.join(work_dir, "msig_withdraw_cli.keys.json")
    msig_address, _msig_abi = deploy_multisig(
        tracer, gql,
        work_dir=work_dir, msig_key_path=msig_key_path,
        usdc_bridge_key_path=usdc_bridge_key,
        is_shellnet=is_shellnet, verbose_faucet=True,
    )

    abs_key_path = os.path.abspath(msig_key_path)
    tracer.log_phase("PASS — multisig deployed and USDC-funded")
    tracer.log(f"  WITHDRAW_FROM      = {msig_address}")
    tracer.log(f"  WITHDRAW_FROM_KEYS = {abs_key_path}")

    # stdout: only the eval-able lines. Nothing else.
    print(f"export WITHDRAW_FROM={msig_address}")
    print(f"export WITHDRAW_FROM_KEYS={abs_key_path}")


if __name__ == "__main__":
    main()
