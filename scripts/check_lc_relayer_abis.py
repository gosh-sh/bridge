#!/usr/bin/env python3
"""Check the trimmed ABIs `eth-lc-relayer` ships against the compiled contracts.

`crates/eth-light-client-relayer/abi/` holds cut-down copies of the
`eccUSDCBridge` and `EthBeaconLightClient` ABIs, and the operator env files point
`AN_USDC_ABI_PATH` / `AN_LC_ABI_PATH` at them. Nothing compiles against these
copies, so they drift silently: a function the relayer calls can be missing
from them, or can describe a contract that is no longer deployed. The TVM
function id is a hash of the name and the input and output types, so a stale
entry addresses a function the contract does not have; the relayer also reads
getter outputs by name. The flip once called `setLightClient`, which
`eccUSDCBridge` 1.5.0 no longer has, and nothing noticed until an operator ran it.

    scripts/check_lc_relayer_abis.py

For each trimmed ABI this requires the same `ABI version`, `version` and
`header` as the compiled one, every function in it to be identical to the
compiled function (names and types of inputs and outputs), and every function
the relayer calls to be present. Exits non-zero listing every disagreement.
"""

import json
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
TRIMMED = REPO / "crates/eth-light-client-relayer/abi"

# (trimmed ABI, compiled ABI, functions `eth-lc-relayer` calls through it —
# see `crates/eth-light-client-relayer/src/submitter.rs`)
PAIRS = [
    (
        TRIMMED / "USDCBridge.abi.json",
        REPO / "contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json",
        ["getAnchorConfig", "disableOwnerAnchors"],
    ),
    (
        TRIMMED / "EthBeaconLightClient.abi.json",
        REPO / "contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.abi.json",
        [
            "submitUpdate",
            "submitRotate",
            "submitAncestry",
            "rePushAnchor",
            "disableOwnerRotation",
            "setCommitteeCommitment",
        ],
    ),
]


def rel(path: pathlib.Path) -> str:
    return str(path.relative_to(REPO))


def check(trimmed_path: pathlib.Path, compiled_path: pathlib.Path, called: list[str]) -> list[str]:
    trimmed = json.loads(trimmed_path.read_text())
    compiled = json.loads(compiled_path.read_text())
    where = f"{rel(trimmed_path)} vs {rel(compiled_path)}"
    errors = []
    for key in ("ABI version", "version", "header"):
        if trimmed.get(key) != compiled.get(key):
            errors.append(f"{where}: `{key}` is {trimmed.get(key)!r}, compiled {compiled.get(key)!r}")
    compiled_fns = {f["name"]: f for f in compiled["functions"]}
    trimmed_fns = {f["name"]: f for f in trimmed["functions"]}
    for name, fn in trimmed_fns.items():
        other = compiled_fns.get(name)
        if other is None:
            errors.append(f"{where}: `{name}` is not in the compiled contract")
        elif fn != other:
            errors.append(
                f"{where}: `{name}` differs\n"
                f"    trimmed:  {json.dumps(fn, sort_keys=True)}\n"
                f"    compiled: {json.dumps(other, sort_keys=True)}"
            )
    for name in called:
        if name not in trimmed_fns:
            errors.append(f"{where}: the relayer calls `{name}`, which the trimmed ABI lacks")
    return errors


def main() -> int:
    errors = [e for pair in PAIRS for e in check(*pair)]
    for e in errors:
        print(e, file=sys.stderr)
    if errors:
        return 1
    print(f"{len(PAIRS)} trimmed relayer ABIs match the compiled contracts")
    return 0


if __name__ == "__main__":
    sys.exit(main())
