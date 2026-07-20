# AN-side USDCBridge ABI (canonical)

Use **`scripts/ursus/USDCBridge.abi.json`** as the source of truth for operator tooling.

This copy is kept in sync for the partner Python scripts under `an-bridge-prover/python/`.

## `finalizeDeposit` (production)

The live shellnet contract exposes a **2-argument** call:

```text
finalizeDeposit(bytes proof, bytes publicInputs)
```

The legacy 7-scalar-argument ABI (separate `srcDappId`, `srcSender`, …) is **deprecated** and must not be used for manual submits or runbooks.

Regenerate or refresh this file from `scripts/ursus/USDCBridge.abi.json` after any USDCBridge redeploy.
