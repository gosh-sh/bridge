# gnark-wrappers — Security Notice

**Status (2026-06-08):** Deprecated for production. Retained temporarily for Foundry fixtures that still exercise the legacy Groth16 adapter path.

## Problem

Each wrapper circuit (`circuit-{1a,1b,2,4}/circuit.go`) implements an **identity stub**:

```go
for i := 0; i < NumPublicInputs; i++ {
    api.AssertIsEqual(PI[i], PI[i])
}
```

The Halo2 SHPLONK proof bytes in the input JSON are **discarded** at prove time. The generated Groth16 proof is a tautology over caller-supplied public inputs.

## Mitigation in progress (R15)

Real verification uses `crates/bridge-evm-aggregator/` → Yul SHPLONK verifier per circuit, wired through `*AggregatorVerifier.sol` adapters on Ethereum.

Circuit 4 gnark wrapper is **removed** once `BridgeWithdrawalAggregatorVerifier` is deployed. Circuits 1A/1B/2 wrappers will be removed after Shplonk aggregated bound fixtures replace Groth16 in CI.

## Proving key handling

- `proving.key` is listed in `.gitignore` — never commit.
- Treat host copies as sensitive until stubs are retired (forgery was possible with PK + stub circuit).
- Rotate/isolate keys if a host was shared outside the bridge team.
