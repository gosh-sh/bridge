# Acki Nacki Bridge

A cross-chain bridge between Ethereum and [Acki Nacki](https://docs.ackinacki.com/). USDC is
custodied by `AckiNackiBridge.sol` on Ethereum; Acki Nacki state is committed on-chain from Halo2
proofs verified by SHPLONK aggregator verifiers deployed as Yul bytecode. In the other direction,
Acki Nacki checks a deposit's receipt-trie proof itself against an Ethereum block hash it has
accepted.

> **Where the truth is.** This README orients you; it is not a specification. The document derived
> from the sources line by line, with a `file:line` citation for every behavioural claim, is
> [`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md); [`DOCS.md`](DOCS.md) is the register of
> current documentation. When any prose disagrees with the code, the code wins.

---

## The current deployment

Arbitrum Sepolia ↔ shellnet, deployed from tag `v0.9.0`.
The CLI, relayers and web form take their addresses from configuration.

### Arbitrum Sepolia (chain id 421614)

| | |
|---|---|
| `AckiNackiBridge` | `0xaf413BC4BFeb55b5dd31dD48D7AD75EC47A21aDe` |
| deployment block | `328488622` |
| USDC | `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d` |
| AAVE V3 pool | `0xBfC91D59fdAA134A4ED45f7B584cAf96D7792Eff` |
| aUSDC | `0x460b97BD498E1157530AEb3086301d5225b91216` |

### Acki Nacki (shellnet)

| | |
|---|---|
| `eccBridge` | `0:1a1a…1a1a` |
| version | `1.17.0`, code hash `c25d1ac0…` |
| Arbitrum block verifier (421614, parent chain Sepolia) | `0:f1def367fa6bc245589bfa7bcf54937f5c926624d29e59e19826ee9033f14108` |
| Sepolia beacon light client | `0:9121fc37c8ac55a4fc42a73bccaf725cb0d87b9a8c7dfbc6dd2be7d1690d8462` |
| GraphQL | `https://shellnet.ackinacki.org/graphql` |

### What to set

```bash
BRIDGE_ADDRESS=0xaf413BC4BFeb55b5dd31dD48D7AD75EC47A21aDe
BRIDGE_DEPLOY_BLOCK=328488622
FROM_BLOCK=328488622
RPC_URL=https://arbitrum-sepolia-rpc.publicnode.com
```

`eth-lc-relayer arbitrum-anchors` needs an Arbitrum RPC that keeps a few minutes of state; the
public endpoints do not (`crates/deposit-relayer-daemon/docs/operator_runbook.md`, section 9).

---

## The two directions

**Ethereum → Acki Nacki (deposit).** The user approves USDC and calls
`deposit(address token, uint256 amount, bytes32 anAccount)`. The contract takes custody via
`transferFrom` and emits a `Deposit` event carrying the Acki Nacki destination. Nothing is verified on
the Ethereum side. There is no deposit proof to generate: the relayer or the CLI collects the block
header, the receipt-trie path to the deposit transaction's receipt and the log's index in it
(`crates/deposit-receipt/`), and the AN-side `eccBridge` checks them itself in `finalizeDeposit`. It
hashes the header with the VM's keccak, requires the hash to be a block hash accepted for that chain
id (anchored by the owner, the Ethereum light client or the Arbitrum block verifier), proves the
receipt against the header's receipts root, and reads every deposit field out of the log, which must
be a `Deposit` event emitted by a trusted EVM bridge. Then it mints.
Each transfer is capped at 50 USDC, and the outstanding principal at 10,000 USDC
per L1 chain.

**Acki Nacki → Ethereum (state attestation + payout).** Three permissionless entrypoints:

| Call | Does |
|---|---|
| `verifyBlock` | Advances the rolling commitment to AN state from a cross-bound pair of proofs: Circuit 1A (≥ 2/3 attestation quorum) **or** 1B (> 1/2 split), plus Circuit 2 (layer-hash movement). |
| `applyBkSetUpdate` | Rotates the AN validator-set (BK-set) commitment through a 16-leaf, depth-4 block-id tree. |
| `withdrawByProof` | Pays out USDC against a Circuit-4 proof of a `WithdrawalInitiated` event, anchored into state that `verifyBlock` already recorded. Nullifier-guarded and chain-id scoped. |

There is no refund-style `withdraw(depositId, …)`; it was retired. The owner can pause the bridge
(`pause()` / `unpause()`, read back with `paused()`): while it is paused, `deposit`, `verifyBlock`,
`applyBkSetUpdate` and `withdrawByProof` revert with `BridgePaused()`, and the owner's AAVE
management stays available. There is no upgrade path — every verifier binding is `immutable`, so
replacing a verifier means deploying a new bridge.

Idle USDC can be routed into AAVE V3 by the owner. That module cannot reach user principal: yield
collection is bounded by the surplus above the book value of user deposits, and the functions that
collect it do not appear in the principal-accounting equation at all. Contract detail in
[`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) §10; the operator runbook is
[`docs/aave-yield.md`](docs/aave-yield.md).

---

## Architecture

Every component and the interface each connection goes through, with its call site in the code:
[`docs/bridge-components.md`](docs/bridge-components.md).

```
                 Ethereum → Acki Nacki (deposit)
  user ──approve+deposit──▶ AckiNackiBridge ──Deposit event──▶ tx receipt log
                                  │                                 │
                            custody (USDC)                    MPT receipt proof
                                  │                                 ▼
                            AAVE V3 (optional)          deposit-receipt (off-chain, RPC)
                                                                    │
                                              block header + receipt proof + log index
                                                                    ▼
                                                    AN eccBridge.finalizeDeposit
                                          (keccak(header) is an accepted block hash,
                                           receipt proven against its receipts root)

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
| `crates/deposit-receipt/` | Builds the `finalizeDeposit` arguments for one deposit from an EVM RPC (block header, receipt-trie proof, log index), checking them the way the AN bridge will. |
| `crates/deposit-relayer-daemon/` | Watches the `Deposit` log, builds the receipt proof, submits `finalizeDeposit` on AN. |
| `crates/deposit-chain-ids/` | The single allowlist of deposit source chains, shared by the relayer and the CLI. |
| `crates/bridge-prover-libraries/` | AN-side prover: block-id tree, bridge state, live driver, Circuit-4 event witness. Standalone workspace. |
| `crates/bridge-relayer-daemon/` | AN → ETH relayer. `src/withdraw_e2e/` is the in-process withdrawal pipeline behind `relayer withdraw-e2e`. |
| `crates/ackinacki-bridge/` | End-user CLI: `withdraw` moves USDC from an Acki Nacki multisig to an EVM recipient, `deposit` from an EVM wallet to an Acki Nacki account. Counterpart to the relayers — the daemons own the continuous streams, this owns one withdrawal or one deposit. Installed from published releases, see [`QUICKSTART.md`](crates/ackinacki-bridge/QUICKSTART.md). |
| `crates/bridge-snark-utils/`, `crates/bridge-evm-aggregator/` | Prover orchestration and the R15 SHPLONK aggregator. |
| `frontend/` | WASM deposit UI (Yew). |
| `docs/` | [`EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) and the rest of the current documentation; the register is [`DOCS.md`](DOCS.md). |

**Cargo workspaces.** The root workspace holds `crates/eth-frontend`, `crates/acki-nacki-interface`,
`crates/deposit-chain-ids` and `crates/deposit-receipt`. Everything else is excluded and built
standalone, because the Halo2 forks in play cannot share a dependency tree:
`crates/bridge-prover-libraries/` (gosh-fork halo2-base), `crates/bridge-snark-utils/`,
`crates/deposit-relayer-daemon/`, and `frontend/`.
The AN→ETH relayer `crates/bridge-relayer-daemon/` and the end-user CLI `crates/ackinacki-bridge/`
are symlinked members of `crates/bridge-prover-libraries/` and build only from there.

---

## Build and test

```bash
make setup                 # toolchains and dependencies
make build                 # Rust workspace + Solidity
make test                  # both test suites
make test-all              # every crate's tests + forge test
make check                 # format-check + lint + test
```

Run `make pre-push` before pushing: on top of `make check` it covers the relayer, the aggregator and
`forge coverage`, none of which any GitHub pipeline runs. The CI section of [AGENTS.md](AGENTS.md)
lists what it does and does not cover.

The end-user CLI is not built from here at all —
[`crates/ackinacki-bridge/scripts/install.sh`](crates/ackinacki-bridge/scripts/install.sh) downloads
the published binaries (the CLI, the prover subprocess it shells out to for withdrawals, and the
verifier bytecode and sources), so an operator needs neither Rust nor a checkout. Start at
[`QUICKSTART.md`](crates/ackinacki-bridge/QUICKSTART.md).

Contracts on their own:

```bash
cd contracts/ethereum
forge test                                                   # full suite
forge test --match-contract AckiNackiBridgeWithdrawByProof -vv
FOUNDRY_PROFILE=fork FORK_URL=https://sepolia-rollup.arbitrum.io/rpc forge test --match-contract ArbSepoliaAaveFork  # Arbitrum Sepolia; CI runs it too
```

The suite is inventoried per file, with what each one covers, in
[`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) §13. Two build settings there are
deliberate and worth knowing before you touch them: `optimizer_runs = 1` and `via_ir = true`. The
bridge sits close to the EIP-170 size limit, and several functions are otherwise stack-too-deep.

Standalone crates build from their own directories; the relayer and the end-user CLI from the
prover workspace they belong to:

```bash
cd crates/bridge-prover-libraries && cargo build --release
cd crates/bridge-prover-libraries && cargo test --locked -p bridge-relayer-daemon
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
