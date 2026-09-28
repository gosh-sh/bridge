# Vendored files — provenance

All files under `research/vendored/` are verbatim copies from other trees, snapshot on
**2026-09-28** to keep the reachability research self-contained for review. No edits
here; if you find a bug, fix it upstream first, then re-copy.

## Sources

| File(s)                                       | Source path                                                                                        | Source repo commit |
|-----------------------------------------------|----------------------------------------------------------------------------------------------------|--------------------|
| `test_deploy_and_withdraw_only.py`            | `bridge/crates/bridge-prover-libraries/python/test_deploy_and_withdraw_only.py`                    | `6fbd264` (bridge) |
| `tvm-cli.conf.json`                           | `bridge/crates/bridge-prover-libraries/python/tvm-cli.conf.json`                                   | `6fbd264` (bridge) |
| `helper/{__init__,common,bridge_e2e,msig}.py` | `bridge/crates/bridge-prover-libraries/python/helper/`                                             | `6fbd264` (bridge) |
| `contracts/*`                                 | `bridge/crates/bridge-prover-libraries/python/contracts/`                                          | `6fbd264` (bridge) |

## Refresh command

Run from the repo root any time upstream drifts:

```bash
SRC=/Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/crates/bridge-prover-libraries/python
DST=/Users/alinat/HALO2_TVM_EXPERIMENTS/bridge/multithreading/research/vendored
rsync -a --checksum "$SRC/test_deploy_and_withdraw_only.py" "$SRC/tvm-cli.conf.json" "$DST/"
rsync -a --checksum "$SRC/helper/" "$DST/helper/"
rsync -a --checksum "$SRC/contracts/" "$DST/contracts/"
```

Then bump the commit column above and note the refresh date.

## Runtime dependencies not vendored

The scripts shell out to these binaries; must be on `$PATH`:

- `tvm-cli`
- `tvm-debugger`
- `sold`

And `helper/bridge_e2e.py` reads some paths relative to `$ACKI_NACKI_ROOT`
(the acki-nacki checkout — not vendored):

- `./contracts/0.80.0_compiled/exchange/eccUSDCBridge.{tvc,abi.json}`
- `./config/USDCBridge.keys.json` (only used if `USDC_BRIDGE_KEY_PATH` env var is not set)

If you set `USDC_BRIDGE_KEY_PATH` to `research/vendored/contracts/USDCBridge.keys.json`
you avoid depending on the acki-nacki `config/` state (bearing in mind the drift risk
from `bridge_python_orchestrator_usdc_keys.md`).
