# bridge-EVM — Review Questions

**To:** Pruvendo / Sergey Egorov  
**From:** Alina (AN-side circuits + prover)  
**Scope:** `bridge` review from the AN→ETH side.

## AB-Q5 — Stale `*Groth16*` files in `contracts/ethereum/src/`: follow the 1B retirement pattern for 1A/2/4?

Commit `e2a962b` (2026-06-23) cleanly retired the entire Circuit 1B Groth16 stack (generated verifier + interface + adapter + test + gnark wrapper). The same cleanup was **not** applied to Circuits 1A, 2, or 4 — `ShplonkDeployLib` is the only production path, but Groth16 surface for those three circuits still sits in `src/`.

**Inventory of Groth16-side artifacts under `contracts/ethereum/src/`:**

| File | Used by | Status |
|---|---|---|
| `BridgeWithdrawalVerifier.sol` + `IBridgeWithdrawalGroth16Verifier.sol` | **nothing** (grep: zero `new …(` across `src/`, `script/`, `test/`) | **Dead.** The interface NatSpec promises a `BridgeWithdrawalGroth16VerifierGenerated.sol` landing that never happened — Circuit 4 went straight to Shplonk |
| `PrimaryVerifier.sol` + `IPrimaryGroth16Verifier.sol` + `PrimaryGroth16VerifierGenerated.sol` | Foundry tests only (`test/PrimaryVerifier.t.sol`, `test/AckiNackiBridgeVerifyBlock.t.sol`, `test/FuzzAckiNackiBridgeVerifyBlock.t.sol`) | Test-only; production = `PrimaryAggregatorVerifier.sol` (same `IPrimaryVerifier`) |
| `LayerHashesMovementVerifier.sol` + `ILayerHashesGroth16Verifier.sol` + `LayerHashesGroth16VerifierGenerated.sol` | Foundry tests only (`test/LayerHashesMovementVerifier.t.sol`, `test/AckiNackiBridge*VerifyBlock.t.sol`) | Test-only; production = `LayerHashesAggregatorVerifier.sol` (same `ILayerHashesMovementVerifier`) |

**Two issues with the test-only adapters for 1A/2:**

- Naming shadows production: `src/PrimaryVerifier.sol` (test-only Groth16) and `src/PrimaryAggregatorVerifier.sol` (production Shplonk) both implement `IPrimaryVerifier` and sit side-by-side in `src/`. Same for the Layer-hashes pair. A new reader can't tell which is which without grep.
- NatSpec actively misleads: `PrimaryVerifier.sol:8-9` presents itself as the Circuit 1A adapter with no "test-only" note; `BridgeWithdrawalVerifier.sol:23-28` calls itself the *"single biggest open mainnet blocker"* despite being unused; `IBridgeWithdrawalGroth16Verifier.sol:8` promises a superseded landing.

**Bonus:** `script/DeployRealBridge.s.sol:17, :193` still say *"Groth16 for 1B fallback"* / *"FallbackVerifier (Groth16):"* — operator-visible labels, wrong after `e2a962b`.

**Questions.**

1. **Circuit 4 dead pair.** Can `src/BridgeWithdrawalVerifier.sol` + `src/IBridgeWithdrawalGroth16Verifier.sol` be deleted (1B-style retirement)? Anything I'm missing that justifies keeping them?
2. **Circuits 1A/2.** Is the Groth16 stack for these retained only as Foundry test fixtures, or is there a wiring path I haven't found?
3. **If test-only**, do you prefer: (a) full retirement matching 1B, or (b) renaming the adapters to `*Groth16Adapter.sol` (or relocating under `test/legacy/`) so `src/` no longer carries two implementations of the same interface?
4. **NatSpec + deploy-log labels.** Will you sweep the stale comments (`PrimaryVerifier.sol:8-18`, `LayerHashesMovementVerifier.sol`, `BridgeWithdrawalVerifier.sol:23-28`, `IBridgeWithdrawalGroth16Verifier.sol:5-10`, `DeployRealBridge.s.sol:17, :193`) in the same pass?
5. **Policy reaffirmation.** Round-1 Q1 response said *"no stubs in production"*. Confirm this means no Groth16 adapter (`PrimaryVerifier.sol`, `LayerHashesMovementVerifier.sol`, `BridgeWithdrawalVerifier.sol`) will ever be wired into a live `AckiNackiBridge` constructor on any network (Sepolia included)?
