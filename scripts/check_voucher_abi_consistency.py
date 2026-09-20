#!/usr/bin/env python3
"""Check that `USDCBridge` and `DepositVoucher` still agree on the deposit
callback signature — in the sources and in the compiled artifacts.

`USDCBridge.finalizeDeposit` deploys a `DepositVoucher` whose code is embedded in
the bridge's *data* (`_depositVoucherCode`), and the voucher calls back into
`confirmDeposit`. Three copies of one argument list, and a mismatch is not a
compile error: the deploy succeeds, the voucher constructor aborts on cell
underflow (`exit_code 9`) before it ever reaches `confirmDeposit`, and deposits
simply stop minting. That is exactly what happened on 2026-07-02, when
`USDCBridge.sol` was recompiled after its ABI shrank and `DepositVoucher.sol` was
not, so the fresh bridge re-embedded the previous 6-argument voucher. Nothing in
the toolchain noticed; a day went into the diagnosis.

    scripts/check_voucher_abi_consistency.py
    scripts/check_voucher_abi_consistency.py --source DIR \
        --compiled-bridge FILE --compiled-voucher FILE

Exits non-zero listing every disagreement. Run it after touching either
contract, and before deploying whatever is in the compiled directory.
"""

import argparse
import json
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_SOURCE = REPO / "contracts/an/exchange"
DEFAULT_COMPILED_BRIDGE = REPO / "contracts/an/0.82.0_compiled/exchange/eccUSDCBridge.abi.json"
DEFAULT_COMPILED_VOUCHER = REPO / "contracts/an/0.82.0_compiled/exchange/DepositVoucher.abi.json"

# The voucher constructor's leading parameters are the deposit identity plus the
# payout; `confirmDeposit` must accept the same list, in the same order.
Params = list[tuple[str, str]]  # [(type, name), ...]


def strip_comments(src: str) -> str:
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.DOTALL)
    return re.sub(r"//[^\n]*", "", src)


def parse_params(raw: str) -> Params:
    out: Params = []
    for part in raw.split(","):
        tokens = part.split()
        if len(tokens) >= 2:
            out.append((tokens[0], tokens[-1]))
        elif tokens:
            raise SystemExit(f"cannot parse parameter {part.strip()!r}")
    return out


def source_signature(path: pathlib.Path, header: str) -> Params:
    """Parameters of `header(` — e.g. "constructor" or "function confirmDeposit"."""
    src = strip_comments(path.read_text())
    match = re.search(re.escape(header) + r"\s*\(([^)]*)\)", src)
    if not match:
        raise SystemExit(f"{path}: no `{header}(` found")
    return parse_params(match.group(1))


def source_deploy_arity(path: pathlib.Path) -> int:
    """Number of arguments the `new DepositVoucher{...}(...)` call passes."""
    src = strip_comments(path.read_text())
    match = re.search(
        r"new\s+DepositVoucher\s*\{[^}]*\}\s*\(([^;]*)\)\s*;", src, flags=re.DOTALL
    )
    if not match:
        raise SystemExit(f"{path}: no `new DepositVoucher{{...}}(...)` call found")
    args = [a for a in (a.strip() for a in match.group(1).split(",")) if a]
    return len(args)


def abi_signature(path: pathlib.Path, function: str) -> Params:
    abi = json.loads(path.read_text())
    for entry in abi.get("functions", []):
        if entry["name"] == function:
            return [(i["type"], i["name"]) for i in entry["inputs"]]
    raise SystemExit(f"{path}: no `{function}` in the ABI")


def render(params: Params) -> str:
    return "(" + ", ".join(f"{t} {n}" for t, n in params) + ")"


def compare(problems: list[str], label: str, left: Params, right: Params) -> None:
    if left != right:
        problems.append(f"{label}:\n      {render(left)}\n  vs  {render(right)}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", type=pathlib.Path, default=DEFAULT_SOURCE)
    ap.add_argument(
        "--compiled-bridge",
        type=pathlib.Path,
        default=DEFAULT_COMPILED_BRIDGE,
        help="compiled eccUSDCBridge.abi.json",
    )
    ap.add_argument(
        "--compiled-voucher",
        type=pathlib.Path,
        default=DEFAULT_COMPILED_VOUCHER,
        help="compiled DepositVoucher.abi.json",
    )
    ap.add_argument(
        "--source-only",
        action="store_true",
        help="skip the compiled artefacts (use when they have not been built yet)",
    )
    args = ap.parse_args()

    bridge_src = args.source / "eccUSDCBridge.sol"
    voucher_src = args.source / "DepositVoucher.sol"
    for path in (bridge_src, voucher_src):
        if not path.is_file():
            raise SystemExit(f"{path}: not found (pass --source)")

    src_ctor = source_signature(voucher_src, "constructor")
    src_confirm = source_signature(bridge_src, "function confirmDeposit")
    deploy_arity = source_deploy_arity(bridge_src)

    problems: list[str] = []
    print(f"source {args.source}")
    print(f"  DepositVoucher.constructor  {render(src_ctor)}")
    print(f"  eccUSDCBridge.confirmDeposit   {render(src_confirm)}")
    print(f"  new DepositVoucher(...)     {deploy_arity} args")

    compare(
        problems,
        "SOURCE: voucher constructor and confirmDeposit disagree",
        src_ctor,
        src_confirm,
    )
    if deploy_arity != len(src_ctor):
        problems.append(
            f"SOURCE: finalizeDeposit passes {deploy_arity} args to a "
            f"{len(src_ctor)}-parameter voucher constructor"
        )

    if not args.source_only:
        bridge_abi = args.compiled_bridge
        voucher_abi = args.compiled_voucher
        for path in (bridge_abi, voucher_abi):
            if not path.is_file():
                raise SystemExit(
                    f"{path}: not found (pass --compiled-bridge/--compiled-voucher or --source-only)"
                )

        abi_ctor = abi_signature(voucher_abi, "constructor")
        abi_confirm = abi_signature(bridge_abi, "confirmDeposit")
        print(f"compiled {bridge_abi}")
        print(f"         {voucher_abi}")
        print(f"  DepositVoucher.constructor  {render(abi_ctor)}")
        print(f"  eccUSDCBridge.confirmDeposit   {render(abi_confirm)}")

        # The pairwise check is what would have caught 2026-07-02 on its own.
        compare(
            problems,
            "COMPILED: voucher constructor and confirmDeposit disagree — the two "
            "artefacts were not built from the same sources, and every deposit "
            "will abort with exit_code 9 in the voucher constructor",
            abi_ctor,
            abi_confirm,
        )
        compare(
            problems,
            "STALE: compiled voucher constructor does not match the source — "
            "rebuild both artefacts",
            abi_ctor,
            src_ctor,
        )
        compare(
            problems,
            "STALE: compiled confirmDeposit does not match the source — "
            "rebuild both artefacts",
            abi_confirm,
            src_confirm,
        )

    if problems:
        print(f"\n{len(problems)} problem(s):", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1

    print("\nOK — all copies of the deposit callback signature agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
