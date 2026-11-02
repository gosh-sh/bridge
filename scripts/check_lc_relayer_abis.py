#!/usr/bin/env python3
"""Check the ABIs built into `eth-lc-relayer` against the functions it calls.

The relayer reads no ABI file at runtime (bridge-audit#132): it builds in the
compiled ABIs of `EthBeaconLightClient` and `ArbitrumBlockVerifier`
(`contracts/an/0.81.0_compiled/exchange/`) and the full `eccUSDCBridge` ABI the
CLI ships (`crates/ackinacki-bridge/abi/USDCBridge.abi.json`, kept in sync with
the deployed contract by `scripts/check_bridge_abi_in_sync.sh`). The TVM
function id is a hash of the name and the input and output types, so a function
the relayer calls that is missing from, or renamed in, the built-in ABI only
fails against a live chain. The flip once called `setLightClient`, which
`eccUSDCBridge` 1.5.0 no longer had, and nothing noticed until an operator ran
it.

    scripts/check_lc_relayer_abis.py

For each built-in ABI this requires every function the relayer calls to be
present; for the CLI's `USDCBridge.abi.json`, a copy, it also requires each
such function to be identical (name, inputs, outputs) to the compiled
`eccUSDCBridge` ABI, since a changed type changes the function id. The
`include_str!` paths in `crates/eth-light-client-relayer/src/abi.rs` must name
exactly these files. Exits non-zero listing every disagreement.
"""

import json
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
ABI_RS = REPO / "crates/eth-light-client-relayer/src/abi.rs"

# (built-in ABI, the compiled ABI it must agree with or None when it is the
# compiled file itself, functions `eth-lc-relayer` calls through it — see
# `crates/eth-light-client-relayer/src/submitter.rs` and `src/arbitrum.rs`)
BUILT_IN = [
    (
        REPO / "contracts/an/0.81.0_compiled/exchange/EthBeaconLightClient.abi.json",
        None,
        [
            "submitUpdate",
            "submitRotate",
            "submitAncestry",
            "rePushAnchor",
            "disableOwnerRotation",
            "setCommitteeCommitment",
            "setGloasForkSlot",
            "getGloasForkSlot",
            "getCommitteeState",
            "getConfig",
        ],
    ),
    (
        REPO / "crates/ackinacki-bridge/abi/USDCBridge.abi.json",
        REPO / "contracts/an/0.80.0_compiled/exchange/eccUSDCBridge.abi.json",
        [
            "getLightClient",
            "isOwnerAnchorsEnabled",
            "disableOwnerAnchorsOn",
            "getL2Verifier",
            "isAcceptedBlockHash",
        ],
    ),
    (
        REPO / "contracts/an/0.81.0_compiled/exchange/ArbitrumBlockVerifier.abi.json",
        None,
        ["proveBlock", "getConfig"],
    ),
]


def rel(path: pathlib.Path) -> str:
    return str(path.relative_to(REPO))


def included_files() -> set[pathlib.Path]:
    """The files `abi.rs` builds in, resolved against its own directory."""
    text = ABI_RS.read_text()
    here = ABI_RS.parent
    return {(here / p).resolve() for p in re.findall(r'include_str!\(\s*"([^"]+)"\s*\)', text)}


def check(abi_path: pathlib.Path, compiled_path: pathlib.Path | None, called: list[str]) -> list[str]:
    errors = []
    abi = json.loads(abi_path.read_text())
    fns = {f["name"]: f for f in abi["functions"]}
    compiled = json.loads(compiled_path.read_text())["functions"] if compiled_path else None
    compiled_fns = {f["name"]: f for f in compiled} if compiled is not None else {}
    for name in called:
        fn = fns.get(name)
        if fn is None:
            errors.append(f"{rel(abi_path)}: the relayer calls `{name}`, which the ABI lacks")
            continue
        if compiled is None:
            continue
        other = compiled_fns.get(name)
        if other is None:
            errors.append(f"{rel(compiled_path)}: `{name}` is not in the compiled contract")
        elif fn != other:
            errors.append(
                f"{rel(abi_path)}: `{name}` differs from {rel(compiled_path)}\n"
                f"    built in: {json.dumps(fn, sort_keys=True)}\n"
                f"    compiled: {json.dumps(other, sort_keys=True)}"
            )
    return errors


def main() -> int:
    errors = [e for abi, compiled, called in BUILT_IN for e in check(abi, compiled, called)]
    included = included_files()
    expected = {abi.resolve() for abi, _, _ in BUILT_IN}
    for missing in sorted(expected - included):
        errors.append(f"{rel(ABI_RS)} does not build in {rel(missing)}")
    for extra in sorted(included - expected):
        errors.append(f"{rel(ABI_RS)} builds in {rel(extra)}, which this check does not cover")
    for e in errors:
        print(e, file=sys.stderr)
    if errors:
        return 1
    print(f"{len(BUILT_IN)} built-in relayer ABIs hold every function the relayer calls")
    return 0


if __name__ == "__main__":
    sys.exit(main())
