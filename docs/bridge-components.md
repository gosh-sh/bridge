# Bridge components

> **Verified against `b651b093`** (PR 139). Every connection and every state file below is drawn
> from the code that makes or names it, not from prose; the tables give each one's `file:line` at
> that commit.

A UML component diagram of the whole bridge as deployed on Arbitrum Sepolia: the contracts on the
three chains, the relayers and the end-user CLI between them, and the interfaces that connect them.

![UML component diagram of the bridge](bridge-components.svg)

## Notation

- **«component»** — a program: a relayer, the CLI, a prover, an RPC node. **«component, contract»**
  — a smart contract on a chain. **«external»** — a third-party service.
- **Frame with a tab** — a chain. The contracts sit inside their chain; the relayers and the CLI sit
  between the chains.
- **Ball** — an interface a component provides, labelled with its name. **Socket** (the half circle
  around a ball) — a component that uses it. When several components use one interface, each gets its
  own ball with the same label.
- **Color** — each relayer and the CLI has its own: its box and every connection it makes.
- **Dashed arrow** — a dependency: a process the component starts, or a contract that reads another.
- Writes are drawn to the contract interface they call; physically they go through the chain's node:
  the RPC node on Arbitrum, the Block Manager's `REST /v2/messages` on Acki Nacki. Reads go through the
  RPC node (Arbitrum, Sepolia) or the Block Manager (Acki Nacki). A getter called through the SDK
  downloads the account over `REST /v2/account` and runs locally; it does not go through GraphQL.
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
| `ackinacki-bridge` (CLI) | Block Manager | `REST /v2/account`, `/v2/messages`: getters, `finalizeDeposit`, `sendTransaction` | getters `crates/ackinacki-bridge/src/deposit/an.rs:484`, `deposit/an_preflight.rs:69`, `:297`, `:349`; messages `deposit/an.rs:837`, `burn.rs:359` |
| `ackinacki-bridge` (CLI) | Block Manager | GraphQL: `account`, its `messages` and `transactions`, `transaction`, `block`, `blockByHeight` | deposit `crates/ackinacki-bridge/src/deposit/an.rs:510`, `:573`, `:593`, `:618`; withdrawal `crates/bridge-relayer-daemon/src/withdraw_e2e/capture.rs:73`, `:142`, `:236`, `crates/bridge-prover-libraries/bridge-event-witness/src/enrich.rs:541`, client at `crates/ackinacki-bridge/src/orchestrator.rs:864` |
| `ackinacki-bridge` (CLI) | WalletConnect relay | wss | `crates/ackinacki-bridge/src/deposit/args.rs:290`, `deposit/wc/mod.rs:168` |
| `ackinacki-bridge` (CLI) | `AckiNackiBridge` | `withdrawByProof` | `crates/bridge-relayer-daemon/src/bridge.rs:1394`, called from `crates/ackinacki-bridge/src/orchestrator.rs:1104` |
| `ackinacki-bridge` (CLI), `relayer daemon-live` | `aggregate-proof` | process | started at `crates/bridge-relayer-daemon/src/aggregator.rs:606`; the CLI hands it the binary at `crates/ackinacki-bridge/src/orchestrator.rs:973`, the daemon at `crates/bridge-relayer-daemon/src/bin/relayer.rs:2525` |
| `deposit-relayer daemon` | `deposit-prover` | process | `crates/deposit-relayer-daemon/src/prover.rs:313` |
| `deposit-prover` | Arbitrum RPC node | JSON-RPC | `deposit-prover/src/ethereum_fetcher.rs:32`; the URL from `crates/deposit-relayer-daemon/src/prover.rs:382` and `crates/ackinacki-bridge/src/deposit/prover.rs:450` |
| `deposit-relayer daemon` | `eccUSDCBridge` | `finalizeDeposit` | `crates/acki-nacki-interface/src/tvm_client.rs:130` |
| `deposit-relayer daemon` | Block Manager | `REST /v2/account`, `/v2/messages`: `isAcceptedBlockHash`, voucher state, `finalizeDeposit` | `crates/deposit-relayer-daemon/src/submitter.rs:713`, `submitter.rs:725`; through `crates/acki-nacki-interface/src/tvm_client.rs:111` (`get_account`), `:159` (`process_message`) |
| `relayer daemon-live` | `AckiNackiBridge` | `verifyBlock` | `crates/bridge-relayer-daemon/src/bridge.rs:1350` |
| `relayer daemon-live` | `AckiNackiBridge` | `applyBkSetUpdate` | `crates/bridge-relayer-daemon/src/bridge.rs:1490` |
| `relayer daemon-live` | Block Manager | GraphQL: `blocks`, `blockByHeight`, `bkSetUpdates` | `crates/bridge-prover-libraries/bridge-prover-lib/src/live_driver/mod.rs:733`, `live_driver/bundle.rs:261`, `live_driver/bk_update.rs:47`, `crates/bridge-prover-libraries/bridge-gql-fetcher/src/bk_set_fetcher.rs:62`; client at `crates/bridge-relayer-daemon/src/bin/relayer.rs:2241` |
| `eth-lc-relayer arbitrum-anchors` | `ArbitrumBlockVerifier` | `proveBlock` | `crates/eth-light-client-relayer/src/arbitrum.rs:1236` |
| `eth-lc-relayer arbitrum-anchors` | Block Manager | `REST /v2/account`, `/v2/messages`: `getL2Verifier`, `getConfig`, `isAcceptedBlockHash`, `proveBlock` | `crates/eth-light-client-relayer/src/arbitrum.rs:1150`, `:1173`, `:1214`, `:1236`; through `crates/acki-nacki-interface/src/tvm_client.rs:111` (`get_account`), `:159` (`process_message`) |
| `eth-lc-relayer daemon` | `EthBeaconLightClient` | `submitUpdate`, `submitAncestry` | `crates/eth-light-client-relayer/src/submitter.rs:789`, `submitter.rs:812` |
| `eth-lc-relayer daemon` | Block Manager | `REST /v2/account`, `/v2/messages`: light-client getters, `submitUpdate`, `submitAncestry` | `crates/eth-light-client-relayer/src/submitter.rs:841`, `:944`, `:789`, `:812`; through `crates/acki-nacki-interface/src/tvm_client.rs:111` (`get_account`), `:159` (`process_message`) |
| `eth-lc-relayer daemon` | `eth-light-client-prover` | process: step, rotate | `crates/eth-light-client-relayer/src/prover.rs:201`, `:327` |
| `AckiNackiBridge` | verifiers | `verify*` | `contracts/ethereum/src/AckiNackiBridge.sol:931`, `:939`, `:951`, `:1503` |
| `ArbitrumBlockVerifier` | `eccUSDCBridge` | `acceptBlockHashFromL2Verifier` | `contracts/an/exchange/ArbitrumBlockVerifier.sol:136` |
| `EthBeaconLightClient` | `eccUSDCBridge` | `acceptBlockHashFromLightClient` | `contracts/an/exchange/EthBeaconLightClient.sol:678` |
| `DepositVoucher` | `eccUSDCBridge` | `confirmDeposit` | `contracts/an/exchange/DepositVoucher.sol:44` |

Both the CLI and `deposit-relayer daemon` send `finalizeDeposit` for the same deposit; the second is
a no-op, because the `DepositVoucher` address is derived from the deposit's identity
(`contracts/an/exchange/eccUSDCBridge.sol:1048`) and a voucher is deployed once.

## Executables and state files

A UML deployment diagram at specification level: which executable manifests which process, and
which files each process keeps its state in. Servers and containers are not drawn.

![UML deployment diagram at specification level: executables, the processes they manifest and their state files](bridge-executables.svg)

- **«executable»** — a UML artifact: the binary file. **«file»** — a file or directory on disk; its
  label gives the flag and environment variable that set the path, and what the file holds.
- **«manifest»** — the executable implements the process. **«use»** — the process reads and writes
  the file.
- One `eth-lc-relayer` executable runs two processes, the `daemon` and `arbitrum-anchors` commands.
  `deposit-prover` is three executables from `deposit-prover/examples`, `eth-light-client-prover`
  two from `eth-light-client-prover/examples`.
- `deposit-relayer daemon`, `eth-lc-relayer daemon` and `eth-lc-relayer arbitrum-anchors` hold an
  exclusive lock on `<state file>.lock` next to their state file and refuse to start while another
  process holds it. The CLI locks each deposit (`{op}.lock`) and each withdrawal (`<key>.lock`) beside
  its record, and holds `deposit.lock` from asking the wallet until the deposit is on chain, so one
  run does that at a time.
- Paths are the defaults. Not drawn: the provers' own state, and the read-only inputs — SRS and
  keys (`--params-dir`), verifier bytecode, ABIs.

| File | Default path | Contents |
|---|---|---|
| `~/.bridge-deposit-state/` | `crates/ackinacki-bridge/src/deposit/args.rs:555` | `{op}.json` `crates/ackinacki-bridge/src/deposit/store.rs:278`; `{op}.lock` `deposit/locks.rs:182`; `deposit.lock` `deposit/locks.rs:153` |
| `~/.bridge-withdraw-state/` | `crates/ackinacki-bridge/src/orchestrator.rs:2399` | `<key>.json` `crates/ackinacki-bridge/src/idempotency.rs:919`; `<key>.lock` `idempotency.rs:1079` |
| `deposit-relayer-state.json` | `crates/deposit-relayer-daemon/src/bin/deposit-relayer.rs:54` | `crates/deposit-relayer-daemon/src/state.rs:105`; lock `state.rs:580` |
| `./state/` | `crates/bridge-relayer-daemon/src/bin/relayer.rs:401` | `crates/bridge-prover-libraries/bridge-prover-lib/src/bridge_state.rs:157`, `paths.rs:83` |
| `eth-lc-relayer-state.json` | `crates/eth-light-client-relayer/src/bin/eth-lc-relayer.rs:236` | `crates/eth-light-client-relayer/src/state.rs:30`; lock `state.rs:124` |
| `arbitrum-anchors-state.json` | `crates/eth-light-client-relayer/src/bin/eth-lc-relayer.rs:364` | `crates/eth-light-client-relayer/src/arbitrum.rs:228`; lock `crates/eth-light-client-relayer/src/state.rs:124` |

## Not on the diagram

- The user's wallet itself (a WalletConnect app or a local key); only the WalletConnect relay the CLI
  talks to is drawn.
- `BlockHeaderOracle`, the Aave module, and the prover's helper daemons.
- `finalizeDeposit` deploying the `DepositVoucher`: the creation arrow is left out.
