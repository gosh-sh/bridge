# M7 ETH-side prover + runtime aggregation — status (2026-07-07)

Closes the novel half of R15/M7: **runtime aggregation** of a Poseidon inner
snark into the EVM calldata the *already-deployed* SHPLONK aggregator verifier
accepts on-chain, driven from **real Acki Nacki data**.

## What is DONE and proven

### 1. Runtime aggregation binary — `aggregate-proof`
`crates/bridge-evm-aggregator/src/bin/aggregate_proof.rs`

- Input: a Poseidon inner `.snark`. Output: EVM calldata `instances ‖ proof`
  (22 instances = 12 KZG accumulator limbs + 10 Circuit-4 public inputs; 3616 B).
- **Self-check**: regenerates the aggregator Yul `.bin` for the supplied inner
  snark and refuses to emit calldata unless it is **byte-identical** to the
  committed `contracts/ethereum/verifiers/<name>.bin`. VK drift ⇒ hard error.

### 2. ETH-side Circuit-4 prover (`our_side_reprove`)
`crates/bridge-prover-orchestrator/src/bin/export_c4_poseidon_snark.rs`

- New `--fixture <PrivateWitness.json>`: re-proves a **real** withdrawal witness
  (from the live `bridge-event-witness-builder`) with a **Poseidon** transcript
  and exports the aggregator inner `.snark`. The AN-side default is Blake2b; the
  aggregator only consumes Poseidon — so the ETH leg re-proves on our side.
- `--seed` retained for synthetic distinct-PI snarks (universality checks).

### 3. Relayer wiring — `crates/bridge-relayer-daemon/src/aggregator.rs`
- `Circuit4SnarkProver` (+ `SubprocessCircuit4SnarkProver` → `export-c4-poseidon-snark --fixture`, + `MockCircuit4SnarkProver`).
- `ProofAggregator` (+ `SubprocessAggregator` → `aggregate-proof`, + `MockAggregator`).
- `Circuit4ShplonkPipeline`: witness → Poseidon snark → aggregate → calldata,
  with `calldata_binds_instances` cross-check (calldata words 12..21 BE must equal
  the ten LE public inputs) before surfacing the proof.
- CLI: `relayer prove-withdraw-shplonk` → writes a `proof_event.json` whose
  `proof_hex` is the aggregator calldata; `submit-withdraw` / `daemon-withdraw` /
  `daemon-bridge` consume it unchanged (validation already accepts ≥704 B calldata).
- 56 relayer unit tests green (6 new), clippy + fmt clean.

### 4. Proven on-chain with REAL shellnet data (n14, 2026-07-07)
Real withdrawal witness from ursus
(`work-shellnet/event_000000/witness.json`, live shellnet `WithdrawalInitiated`):

| Step | Result |
|------|--------|
| `export-c4-poseidon-snark --fixture real_witness.json` | Poseidon SELF_VERIFY PASS, 10 PIs |
| Our 10 PIs vs partner `proof_event_000000.json` | **all 10 MATCH** byte-for-byte |
| `aggregate-proof` | VK match: regenerated == committed `BridgeWithdrawalAggregatorVerifier.bin` (20987 B) |
| Foundry `AckiNackiBridgeProductionWithdrawByProof` on the real calldata | **3/3 PASS** — deployed real verifier accepts; tamper/mismatch rejected |

Universality (2 distinct inner snarks, different PIs) also confirmed earlier:
both regenerate the identical committed `.bin` and both calldata verify on-chain.

## Still BLOCKED — 1A / 1B / 2 live → Poseidon

`bridge-prover-daemon` is a **binary-only crate** (no `lib.rs`): the live-witness
code (`gql_client`, `attestation_fetcher`, `real_chain_builder`, `block_id_tree`)
is private to that binary, so it cannot be reused to build a Poseidon inner snark
from a live key block the way Circuit 4's `bridge-event-prover-lib` +
`bridge-event-witness` split allows.

Two ways forward (partner-side, either works):
1. Expose the daemon's witness-fetch as a small `lib.rs` (so orchestrator can
   fetch a live block's witness and call `generate_{primary,fallback,layer}_proof_with_transcript(Poseidon)`), or
2. Add a Poseidon-snark emit mode to `bridge-prover-daemon` itself (it already
   holds the live witness; it just proves Blake2b today).

Circuit 4 needed neither — its witness builder (`bridge-event-witness`) already
ships as usable binaries + a `PrivateWitness` JSON seam.

## D — shellnet withdraw-bridge redeploy (dry-run green, broadcast gated)

`script/DeployShellnetE2EBridge.s.sol` deploys all four real verifiers
(1A/1B/2 + C4) + bridge in one batch. Local simulate (dummy env) succeeds,
gas ≈ 21.3M, EIP-170 OK. Real broadcast recipe (gated — needs correct shellnet
genesis anchor + real `WITHDRAW_ACC_FR` + deploy key + spends Sepolia ETH +
changes the canonical withdraw-bridge address):

```bash
cd contracts/ethereum
PRIVATE_KEY=<deploy key> \
GENESIS_BK_SET_COMMITMENT=<shellnet anchor> \
GENESIS_PREV_MAX_LEVEL_LAYER_HASH=<shellnet anchor> \
WIRE_WITHDRAW_BY_PROOF=true WITHDRAW_ACC_FR=<real accFr, PI[7]> \
START_PAUSED=true \
forge script script/DeployShellnetE2EBridge.s.sol \
  --rpc-url <alchemy sepolia> --broadcast
```

After deploy: update `ubuntu@ursus-tools.dev:config/bridge-relayer.env`
`BRIDGE_ADDRESS`, unpause after forgery + E2E sign-off.
