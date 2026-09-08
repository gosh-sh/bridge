"""
Minimal driver that deploys a fresh `UpdateCustodianMultisigWallet` on
the target AN cluster and seeds it with `WITHDRAWAL_AMOUNT` USDC
(ECC[3]) via `USDCBridge.mintAndSend`, then prints two eval-able env
lines on stdout for `ackinacki-bridge withdraw` to consume:

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

Env vars honored:
  BRIDGE_CONFIG           REQUIRED path to a network profile file
                          (config/bridge_config.{shellnet,local,mainnet}
                          or the unsuffixed symlink). All values below
                          come from it unless overridden in the shell.
  NETWORK                 tvm-cli --url target (from profile)
  BRIDGE_GQL_ENDPOINT     GQL endpoint used only for resolving the
                          USDCBridge dapp_id inside `mint_usdc`
                          (from profile)
  USDC_BRIDGE_KEY_PATH    bridge-owner key for USDCBridge.mintAndSend
                          (from profile; local devnet auto-materializes
                          if the profile-declared path is the bundled
                          placeholder and the file has never been
                          rewritten)
  BRIDGE_WORK_DIR         where the multisig .keys.json + deployx
                          artifacts land (default: ./work_dir under the
                          CLI crate root)

Prints diagnostics + tvm-cli output to stderr; only the two `export …`
lines go to stdout.
"""

import os
import sys
import time

# Locate the sibling `python/` tree (helper.msig + helper.bridge_e2e
# + helper.common all live there, and bin/tvm-cli is symlinked below it).
_HERE = os.path.dirname(os.path.abspath(__file__))
_CLI_ROOT = os.path.dirname(_HERE)                          # crates/ackinacki-bridge
_PY_ROOT = os.path.abspath(os.path.join(
    _CLI_ROOT, "..", "bridge-prover-libraries", "python"))         # crates/bridge-prover-libraries/python
if not os.path.isdir(_PY_ROOT):
    print(f"[deploy_msig_and_mint] cannot find {_PY_ROOT}", file=sys.stderr)
    sys.exit(2)
sys.path.insert(0, _PY_ROOT)
os.environ["PATH"] = os.path.join(_PY_ROOT, "bin") + os.pathsep + os.environ.get("PATH", "")

from helper import common                                    # noqa: E402
from helper import bridge_e2e as be                          # noqa: E402
from helper.bridge_e2e import (                              # noqa: E402
    USDC_BRIDGE_ADDRESS,
    materialize_usdc_bridge_key_from_node_config,
    validate_usdc_bridge_key,
)
from helper.msig import deploy_multisig                      # noqa: E402


def _log(msg: str):
    print(msg, file=sys.stderr, flush=True)


def _load_profile(path: str) -> None:
    """Source KEY=VALUE lines from a bash-style env file into os.environ.
    Mirrors `set -a && source <path>` — but shell-set vars still win
    (setdefault, not overwrite), matching the CLI's precedence rule."""
    if not os.path.isfile(path):
        _log(f"BRIDGE_CONFIG={path} not found")
        sys.exit(2)
    for raw in open(path):
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        k = k.strip()
        # Strip surrounding quotes if any; leave inner content untouched.
        v = v.strip().strip('"').strip("'")
        if k:
            os.environ.setdefault(k, v)


def main():
    # Profile resolution: BRIDGE_CONFIG (env) → config/bridge_config
    # (symlink default). Everything else — NETWORK, BRIDGE_GQL_ENDPOINT,
    # USDC_BRIDGE_KEY_PATH — comes from that file.
    cfg_path = os.environ.get(
        "BRIDGE_CONFIG",
        os.path.join(_CLI_ROOT, "config", "bridge_config"),
    )
    _load_profile(cfg_path)

    network = os.environ.get("NETWORK")
    graphql = os.environ.get("BRIDGE_GQL_ENDPOINT")
    usdc_bridge_key = os.environ.get("USDC_BRIDGE_KEY_PATH")
    missing = [k for k, v in [
        ("NETWORK", network),
        ("BRIDGE_GQL_ENDPOINT", graphql),
        ("USDC_BRIDGE_KEY_PATH", usdc_bridge_key),
    ] if not v]
    if missing:
        _log(f"profile {cfg_path} missing required keys: {', '.join(missing)}")
        sys.exit(2)

    # Local devnet is anything on localhost/127.*; used only to decide
    # whether to re-materialize USDCBridge.keys.json from the node config.
    is_local = network.startswith("http://127.") or "localhost" in network
    gql_kwargs = ({} if is_local
                  else {"user_agent": "ackinacki-bridge-deploy-msig/1.0",
                        "timeout": 30})

    work_dir = os.environ.get(
        "BRIDGE_WORK_DIR",
        os.path.join(_CLI_ROOT, "work_dir"),
    )

    os.makedirs(work_dir, exist_ok=True)

    # tvm-cli setup — mirrors test_deploy_and_withdraw_only.py:109-113.
    os.environ["NETWORK"] = network
    common.NETWORK = network
    common.set_config({"async_call": "false"})
    common.setup()
    time.sleep(1)

    tracer = be.Tracer()
    gql = be.GqlClient(graphql, **gql_kwargs)

    tracer.log_phase(f"deploy_msig_and_mint prechecks (profile={cfg_path})")
    tracer.log(f"  NETWORK:              {network}")
    tracer.log(f"  BRIDGE_GQL_ENDPOINT:  {graphql}")
    tracer.log(f"  BRIDGE_WORK_DIR:      {work_dir}")
    tracer.log(f"  USDC_BRIDGE_KEY_PATH: {usdc_bridge_key}")

    if not common.is_account_active(USDC_BRIDGE_ADDRESS):
        _log(f"USDCBridge is not active at {USDC_BRIDGE_ADDRESS}; aborting")
        sys.exit(3)
    tracer.log(f"  USDCBridge active at {USDC_BRIDGE_ADDRESS}")

    # Local devnet: refresh USDCBridge.keys.json from the running node's
    # config on every invocation. Remote profiles (shellnet, mainnet)
    # use the pinned partner-provided file declared in the profile.
    if is_local:
        tracer.log_phase("Materializing USDCBridge.keys.json from local cluster config")
        prover_dir = os.path.dirname(_PY_ROOT)   # crates/bridge-prover-libraries
        materialize_usdc_bridge_key_from_node_config(tracer, prover_dir, usdc_bridge_key)

    tracer.log_phase("Validating USDCBridge owner key against on-chain getOwnerPubkey")
    validate_usdc_bridge_key(tracer, usdc_bridge_key, overridden=False)

    msig_key_path = os.path.join(work_dir, "msig_withdraw_cli.keys.json")
    msig_address, _msig_abi = deploy_multisig(
        tracer, gql,
        work_dir=work_dir, msig_key_path=msig_key_path,
        usdc_bridge_key_path=usdc_bridge_key,
        is_shellnet=not is_local, verbose_faucet=True,
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
