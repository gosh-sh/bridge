# Bridge components

> **Verified against `447aa635`** (branch `feature/l2-block-proof`). Every connection below is drawn
> from the call site that makes it, not from prose; the table gives each one's `file:line` at that
> commit.

A UML component diagram of the whole bridge as deployed on Arbitrum Sepolia: the contracts on the
three chains, the relayers and the end-user CLI between them, and the interfaces that connect them.

![UML component diagram of the bridge](bridge-components.svg)

## Notation

- **«component»** — a program: a relayer, the CLI, a prover, an RPC node. **«component, contract»**
  — a smart contract on a chain.
- **Frame with a tab** — a chain. The contracts sit inside their chain; the relayers and the CLI sit
  between the chains.
- **Ball** — an interface a component provides, labelled with its name. **Socket** (the half circle
  around a ball) — a component that uses it. When several components use one interface, each gets its
  own ball with the same label.
- **Color** — each relayer and the CLI has its own: its box and every connection it makes.
- **Dashed arrow** — a dependency: a process the component starts, or a contract that reads another.
- Writes are drawn to the contract interface they call. Reads go through the chain's RPC node
  (Arbitrum, Sepolia) or GraphQL node (Acki Nacki).
- `verify*` — the four verifiers `AckiNackiBridge` calls: `verifyPrimaryAttestation`,
  `verifyFallbackAttestation` and `verifyLayerHashesMovement` for `verifyBlock`, `verifyWithdrawal`
  for `withdrawByProof`. `transfer*` — `transferFrom` on deposit, `transfer` on payout.

## Connections

Paths are relative to the repository root.

| From | To | Interface | Call site |
|---|---|---|---|
| `ackinacki-bridge` (CLI) | USDC | `approve` | `crates/ackinacki-bridge/src/deposit/evm.rs:220` |
| `ackinacki-bridge` (CLI) | `AckiNackiBridge` | `deposit` | `crates/ackinacki-bridge/src/deposit/evm.rs:210` |
| `ackinacki-bridge` (CLI) | `deposit-prover` | process | `crates/ackinacki-bridge/src/deposit/prover.rs:206` |
| `ackinacki-bridge` (CLI) | `eccUSDCBridge` | `finalizeDeposit` | `crates/ackinacki-bridge/src/deposit/an.rs:754` |
| `ackinacki-bridge` (CLI) | user multisig | `sendTransaction` carrying `initiateWithdrawal` | `crates/ackinacki-bridge/src/burn.rs:267`, payload `burn.rs:497` |
| `ackinacki-bridge` (CLI) | Acki Nacki node | GraphQL getters | `crates/ackinacki-bridge/src/deposit/an_preflight.rs:69`, `:288`, `:340` |
| `ackinacki-bridge` (CLI) | `AckiNackiBridge` | `withdrawByProof` | `crates/bridge-relayer-daemon/src/bridge.rs:1394`, called from `crates/ackinacki-bridge/src/orchestrator.rs:1104` |
| `ackinacki-bridge` (CLI), `relayer daemon-live` | `aggregate-proof` | process | started at `crates/bridge-relayer-daemon/src/aggregator.rs:606`; the CLI hands it the binary at `crates/ackinacki-bridge/src/orchestrator.rs:973`, the daemon at `crates/bridge-relayer-daemon/src/bin/relayer.rs:2525` |
| `deposit-relayer daemon` | `deposit-prover` | process | `crates/deposit-relayer-daemon/src/prover.rs:313` |
| `deposit-relayer daemon` | `eccUSDCBridge` | `finalizeDeposit` | `crates/acki-nacki-interface/src/tvm_client.rs:130` |
| `deposit-relayer daemon` | Acki Nacki node | GraphQL: `isAcceptedBlockHash`, voucher state | `crates/deposit-relayer-daemon/src/submitter.rs:713`, `submitter.rs:725` |
| `relayer daemon-live` | `AckiNackiBridge` | `verifyBlock` | `crates/bridge-relayer-daemon/src/bridge.rs:1350` |
| `relayer daemon-live` | `AckiNackiBridge` | `applyBkSetUpdate` | `crates/bridge-relayer-daemon/src/bridge.rs:1490` |
| `relayer daemon-live` | Acki Nacki node | GraphQL | `crates/bridge-relayer-daemon/src/bin/relayer.rs:2241` |
| `deposit-relayer arbitrum-anchors` | `ArbitrumBlockVerifier` | `proveBlock` | `crates/deposit-relayer-daemon/src/arbitrum.rs:1044` |
| `deposit-relayer arbitrum-anchors` | Acki Nacki node | GraphQL: `isAcceptedBlockHash` and verifier getters | `crates/deposit-relayer-daemon/src/arbitrum.rs:987`, `arbitrum.rs:1022` |
| `eth-lc-relayer` | `EthBeaconLightClient` | `submitUpdate`, `submitAncestry` | `crates/eth-light-client-relayer/src/submitter.rs:789`, `submitter.rs:812` |
| `eth-lc-relayer` | Acki Nacki node | GraphQL: light-client getters | `crates/eth-light-client-relayer/src/submitter.rs:841`, `submitter.rs:944` |
| `AckiNackiBridge` | verifiers | `verify*` | `contracts/ethereum/src/AckiNackiBridge.sol:931`, `:939`, `:951`, `:1503` |
| `ArbitrumBlockVerifier` | `eccUSDCBridge` | `acceptBlockHashFromL2Verifier` | `contracts/an/exchange/ArbitrumBlockVerifier.sol:136` |
| `EthBeaconLightClient` | `eccUSDCBridge` | `acceptBlockHashFromLightClient` | `contracts/an/exchange/EthBeaconLightClient.sol:678` |
| `DepositVoucher` | `eccUSDCBridge` | `confirmDeposit` | `contracts/an/exchange/DepositVoucher.sol:44` |

Both the CLI and `deposit-relayer daemon` send `finalizeDeposit` for the same deposit; the second is
a no-op, because the `DepositVoucher` address is derived from the deposit's identity
(`contracts/an/exchange/eccUSDCBridge.sol:1040`) and a voucher is deployed once.

## Not on the diagram

- The wallet the CLI signs the deposit with (WalletConnect or a local key).
- `BlockHeaderOracle`, the Aave module, and the prover's helper daemons.
- `finalizeDeposit` deploying the `DepositVoucher`: the creation arrow is left out.
