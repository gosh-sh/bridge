# Acki Nacki Bridge

A cross-chain bridge between Ethereum and [Acki Nacki](https://docs.ackinacki.com/), with
zero-knowledge proofs on both directions of travel. USDC is custodied by `AckiNackiBridge.sol` on
Ethereum; Acki Nacki state is committed on-chain from Halo2 proofs verified by SHPLONK aggregator
verifiers deployed as Yul bytecode.

> **Where the truth is.** This README orients you; it is not a specification. The document derived
> from the sources line by line, with a `file:line` citation for every behavioural claim, is
> [`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md); [`DOCS.md`](DOCS.md) is the register of
> current documentation. When any prose disagrees with the code, the code wins.

---

## The two directions

**Ethereum → Acki Nacki (deposit).** The user approves USDC and calls
`deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount)`. The contract takes custody via
`transferFrom` and emits a `Deposit` event carrying the Acki Nacki destination. Nothing is verified on
the Ethereum side. Off-chain, `deposit-prover/` proves the event's presence in a real Ethereum block
(receipt-trie inclusion + log binding, with a keccak coprocessor), and the AN-side `USDCBridge`
consumes that Halo2 proof natively through the `ZKHALO2VERIFYWITHVK` TVM opcode, then mints. The
per-transaction cap is `type(uint64).max` units — sized so the amount fits the AN mint path, not as a
TVL limit.

**Acki Nacki → Ethereum (state attestation + payout).** Three permissionless entrypoints:

| Call | Does |
|---|---|
| `verifyBlock` | Advances the rolling commitment to AN state from a cross-bound pair of proofs: Circuit 1A (≥ 2/3 attestation quorum) **or** 1B (> 1/2 split), plus Circuit 2 (layer-hash movement). |
| `applyBkSetUpdate` | Rotates the AN validator-set (BK-set) commitment through a 16-leaf, depth-4 block-id tree. |
| `withdrawByProof` | Pays out USDC against a Circuit-4 proof of a `WithdrawalInitiated` event, anchored into state that `verifyBlock` already recorded. Nullifier-guarded and chain-id scoped. |

There is no refund-style `withdraw(depositId, …)`; it was retired. There is no pause switch and no
upgrade path — every verifier binding is `immutable`, so replacing a verifier means deploying a new
bridge.

Idle USDC can be routed into AAVE V3 by the owner. That module cannot reach user principal: yield
collection is bounded by the surplus above the book value of user deposits, and the functions that
collect it do not appear in the principal-accounting equation at all. Contract detail in
[`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) §10; the operator runbook is
[`docs/aave-yield.md`](docs/aave-yield.md).

---

## Architecture

```
                 Ethereum → Acki Nacki (deposit)
  user ──approve+deposit──▶ AckiNackiBridge ──Deposit event──▶ tx receipt log
                                  │                                 │
                            custody (USDC)                    MPT receipt proof
                                  │                                 ▼
                            AAVE V3 (optional)          deposit-prover (Halo2, off-chain)
                                                                    │
                                                        SHPLONK proof + 12 public inputs
                                                                    ▼
                                                    AN USDCBridge.finalizeDeposit
                                                    (ZKHALO2VERIFYWITHVK opcode)

                 Acki Nacki → Ethereum (state + payout)
  AN node ──GraphQL──▶ bridge-prover-libraries ──Circuits 1A/1B, 2, 4──▶ bridge-relayer-daemon
                                                                          │
                                              verifyBlock / applyBkSetUpdate / withdrawByProof
                                                                          ▼
                                                                  AckiNackiBridge
                                                                          │
                                    Primary / Fallback / LayerHashes / BridgeWithdrawal adapters
                                                                          ▼
                                              ShplonkHalo2Verifier ──▶ Yul Halo2Verifier (CREATE)
```

---

## Repository layout

| Path | What lives there |
|---|---|
| `contracts/ethereum/src/` | The bridge, four verifier adapters, the SHPLONK shim, oracles. Solidity 0.8.19, Foundry. |
| `contracts/ethereum/verifiers/` | Production SHPLONK Yul **creation bytecode** (`.bin`) per circuit, the generated Solidity source (`.sol`) it compiles from, and reference calldata. The `.bin` is what deploys; the `.sol` is what `aggregate-proof` self-checks a proof against. |
| `contracts/ethereum/script/` | Deploy scripts; `ShplonkDeployLib.sol` wires `.bin` → shim → typed adapter. |
| `deposit-prover/` | ETH → AN deposit proof (Halo2 on axiom-eth: receipt MPT, log binding, keccak coprocessor). |
| `crates/deposit-relayer-daemon/` | Watches the `Deposit` log, drives the prover, submits `finalizeDeposit` on AN. |
| `crates/deposit-chain-ids/` | The single allowlist of deposit source chains, shared by prover and relayer. |
| `crates/bridge-prover-libraries/` | AN-side prover: block-id tree, bridge state, live driver, Circuit-4 event witness. Standalone workspace. |
| `crates/bridge-relayer-daemon/` | AN → ETH relayer. `src/withdraw_e2e/` is the in-process withdrawal pipeline behind `relayer withdraw-e2e`. |
| `crates/ackinacki-bridge/` | End-user CLI: withdraws USDC from an Acki Nacki multisig to an EVM recipient. Counterpart to the relayer — the daemon owns bundle proving, this owns one withdrawal. Installed from published releases, see [`QUICKSTART.md`](crates/ackinacki-bridge/QUICKSTART.md). |
| `crates/bridge-snark-utils/`, `crates/bridge-evm-aggregator/` | Prover orchestration and the R15 SHPLONK aggregator. |
| `frontend/` | WASM deposit UI (Yew). |
| `docs/` | [`EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) and the rest of the current documentation; the register is [`DOCS.md`](DOCS.md). |

**Cargo workspaces.** The root workspace holds `crates/eth-frontend`, `crates/acki-nacki-interface`
and `crates/deposit-chain-ids`. Everything else is excluded and built standalone, because the Halo2
forks in play cannot share a dependency tree: `deposit-prover/` (axiom-eth), `crates/bridge-prover-libraries/`
(gosh-fork halo2-base), `crates/bridge-snark-utils/`, both relayer daemons, and `frontend/`.

---

## Build and test

```bash
make setup                 # toolchains and dependencies
make build                 # Rust workspace + Solidity
make test                  # both test suites
make check                 # format-check + lint + test
```

Run `make check` before pushing.

The end-user withdrawal CLI is not built from here at all —
[`crates/ackinacki-bridge/scripts/install.sh`](crates/ackinacki-bridge/scripts/install.sh) downloads
the published binaries (the CLI, the prover subprocess it shells out to, and the verifier bytecode
and sources), so an operator needs neither Rust nor a checkout. Start at
[`QUICKSTART.md`](crates/ackinacki-bridge/QUICKSTART.md).

Contracts on their own:

```bash
cd contracts/ethereum
forge test                                                   # full suite
forge test --match-contract AckiNackiBridgeWithdrawByProof -vv
FOUNDRY_PROFILE=fork forge test --match-contract AaveFork     # needs a mainnet RPC
```

The suite is inventoried per file, with what each one covers, in
[`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) §13. Two build settings there are
deliberate and worth knowing before you touch them: `optimizer_runs = 1` and `via_ir = true`. The
bridge sits close to the EIP-170 size limit, and several functions are otherwise stack-too-deep.

Standalone crates build from their own directories:

```bash
cd crates/bridge-prover-libraries && cargo build --release
cd crates/bridge-relayer-daemon && cargo test
```

---

## Running it against a live network

Deployment, environment variables and genesis parameters: `docs/EVM-contracts-spec.md` §12. One
constraint bites early — the genesis seed must sit on a key-block boundary, currently
`W·P = 128 × 8 = 1024`.

Operating procedures — bootstrapping the prover daemons, the two live proving lanes, deploy timing and
recovery — are documented next to the code they describe: the crate READMEs under
`crates/bridge-prover-libraries/`, and each daemon's own `--help`.

---

## Status

Shellnet / Sepolia. Not audited for mainnet. Known trade-offs and limitations are enumerated in
[`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) §15 — read that section before making any
claim about the bridge's security properties.
