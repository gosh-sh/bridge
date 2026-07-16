# Acki Nacki Bridge

Cross-chain bridge between **Ethereum** and [Acki Nacki](https://docs.ackinacki.com/) using zero-knowledge proofs end-to-end.

**Documentation:** [docs/README.md](docs/README.md) — architecture, operations, ZK references, audit material.

## What the bridge does

Two independent directions, each with its own proof system:

| Direction | User action | On-chain entry | Proof consumer |
|-----------|-------------|----------------|----------------|
| **Ethereum → Acki Nacki** | Deposit USDC on Ethereum | `AckiNackiBridge.deposit()` | AN `USDCBridge.finalizeDeposit()` via `ZKHALO2VERIFYWITHVK` |
| **Acki Nacki → Ethereum (state)** | Permissionless relayer | `AckiNackiBridge.verifyBlock()` | R15 SHPLONK aggregator verifiers on Ethereum |
| **Acki Nacki → Ethereum (payout)** | Withdraw on AN, claim on ETH | `AckiNackiBridge.withdrawByProof()` | Circuit 4 SHPLONK verifier on Ethereum |

```
  ETH → AN (deposits)                    AN → ETH (state + payout)
┌─────────────────────┐                ┌──────────────────────────┐
│ AckiNackiBridge     │  Deposit event   │ AN blocks + TokenBridge  │
│   deposit(USDC)     │ ──────────────▶  │   initiateWithdrawal     │
└─────────────────────┘                  └──────────────────────────┘
         │                                          │
         ▼ deposit-prover (Halo2)                   ▼ an-bridge-prover (Circuits 1A/1B/2/4)
         │                                          │
         ▼                                          ▼
┌─────────────────────┐                ┌──────────────────────────┐
│ AN USDCBridge       │                │ bridge-relayer-daemon    │
│ finalizeDeposit     │                │ verifyBlock + withdraw   │
└─────────────────────┘                └──────────────────────────┘
                                                    │
                                                    ▼
                                       ┌──────────────────────────┐
                                       │ AckiNackiBridge (Ethereum)│
                                       └──────────────────────────┘
```

## Repository layout

```
acki-nacki-bridge/
├── contracts/ethereum/          # Solidity (Foundry): AckiNackiBridge + verifiers
├── deposit-prover/              # ETH→AN deposit Halo2 circuit (standalone workspace)
├── crates/
│   ├── acki-nacki-interface/    # AN REST client (BK-set), TVM submitter traits
│   ├── eth-frontend/            # Rust Ethereum client (alloy)
│   ├── deposit-relayer-daemon/  # ETH→AN relayer (listen → prove → finalizeDeposit)
│   ├── bridge-relayer-daemon/   # AN→ETH relayer (verifyBlock + withdrawByProof)
│   ├── bridge-prover-orchestrator/  # Fixture export, R15 aggregator glue
│   ├── an-bridge-prover/        # Live AN→ETH prover sub-workspace
│   └── bridge-evm-aggregator/   # R15 snark-verifier spike (M2)
├── docs/                        # Canonical documentation (see docs/README.md)
├── _archive/                    # Historical / superseded material
├── frontend/                    # WASM UI scaffold (deposit tab only)
└── poseidon-proof/              # Blake2b transcript demo (reference)
```

AN-side contracts (`USDCBridge`, `TokenBridge`, `DepositVoucher`) live in the sibling repo **`acki-nacki`**, not here.

## Quick start

### Prerequisites

Rust (stable), Go 1.21+, Foundry. One-time setup:

```bash
./setup.sh
```

### Build & test

```bash
make build          # Rust workspace + Solidity
make test           # cargo test --workspace + forge test

cd contracts/ethereum && forge test    # ~174 Foundry tests, 21 suites
cd crates/bridge-relayer-daemon && cargo test
cd crates/deposit-relayer-daemon && cargo test
cd deposit-prover && cargo test
```

Pre-push (mirrors CI): `make pre-push`

## Key contracts

| Contract | Role |
|----------|------|
| `AckiNackiBridge.sol` | USDC deposits, `verifyBlock`, `withdrawByProof`, optional AAVE yield |
| `PrimaryAggregatorVerifier.sol` / `FallbackAggregatorVerifier.sol` | Circuit 1A / 1B (production SHPLONK) |
| `LayerHashesAggregatorVerifier.sol` | Circuit 2 |
| `BridgeWithdrawalAggregatorVerifier.sol` | Circuit 4 withdrawal payout |

Groth16 adapter contracts under `src/*Groth16*` are retained for **test coverage only**; production uses the R15 SHPLONK `.bin` verifiers in `contracts/ethereum/verifiers/`.

## ZK summary

| Flow | Proof system | Transcript | On-chain consumer |
|------|-------------|------------|-------------------|
| ETH→AN deposit | Halo2 SHPLONK, K=18 | Keccak (EVM-native) | AN `ZKHALO2VERIFYWITHVK` (11 public inputs) |
| AN→ETH state | Halo2 SHPLONK → R15 aggregator | Blake2b / Poseidon inner | Ethereum SHPLONK Yul verifiers |
| AN→ETH withdraw | Circuit 4, 10 public inputs | Blake2b | `BridgeWithdrawalAggregatorVerifier` |

## Further reading

- [docs/architecture/four_circuit_architecture.md](docs/architecture/four_circuit_architecture.md) — circuit architecture
- [docs/operations/bridge_verification.md](docs/operations/bridge_verification.md) — security invariants
- [docs/operations/evm_an_deposit_e2e_runbook.md](docs/operations/evm_an_deposit_e2e_runbook.md) — deposit E2E operator guide
- [docs/integration/an_partner_integration_plan.md](docs/integration/an_partner_integration_plan.md) — integration roadmap + Decision Log

## License

MIT
