# Bridge components

> **Verified against `b651b093`** (PR 139); the executables and state files against `f86d251e`
> (PR 148); the `eth-lc-relayer daemon` rows, its rotate directory and the rotate key cache against
> `d7a14b19` (PR 153); the AAVE V3 pool, aUSDC and the bridge's `approve` against `a61d1e75`
> (PR 142). Every connection and every state file below is drawn from the code that makes or names
> it, not from prose; the tables give each one's `file:line` at that commit.
>
> **Deposits without a ZK proof (bridge-audit#139).** The rows for `deposit-receipt`, the
> `DepositVoucher` citation and the executables bullet describe that change: the deposit prover is
> gone, and the CLI and `deposit-relayer daemon` build the receipt proof with the
> `deposit-receipt` library. Citations into `crates/ackinacki-bridge` and
> `crates/deposit-relayer-daemon` name the function rather than a line, because those crates were
> still changing.

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
- **Dashed arrow** — a dependency: a process the component starts, a library it links, or a contract
  that reads another.
- Writes are drawn to the contract interface they call; physically they go through the chain's node:
  the RPC node on Arbitrum, the Block Manager's `REST /v2/messages` on Acki Nacki. Reads go through the
  RPC node (Arbitrum, Sepolia) or the Block Manager (Acki Nacki). A getter called through the SDK
  downloads the account over `REST /v2/account` and runs locally; it does not go through GraphQL.
- `verify*` — the four verifiers `AckiNackiBridge` calls: `verifyPrimaryAttestation`,
  `verifyFallbackAttestation` and `verifyLayerHashesMovement` for `verifyBlock`, `verifyWithdrawal`
  for `withdrawByProof`. `transfer*` — `transferFrom` on deposit, `transfer` on payout. The bridge
  also calls USDC's `approve`: for the AAVE pool before `supply`, for the successor bridge in
  `migrateTreasury`.
- **test market** — the AAVE V3 deployment on Arbitrum Sepolia. The bridge is deployed with its
  pool and aUSDC (`contracts/ethereum/script/ArbitrumSepoliaAave.sol:12`, `:13`), never a mock.

## Connections

Paths are relative to the repository root.

| From | To | Interface | Call site |
|---|---|---|---|
| `ackinacki-bridge` (CLI) | USDC | `approve` | `crates/ackinacki-bridge/src/deposit/evm.rs:220` |
| `ackinacki-bridge` (CLI) | `AckiNackiBridge` | `deposit` | `crates/ackinacki-bridge/src/deposit/evm.rs:210` |
| `ackinacki-bridge` (CLI) | `deposit-receipt` | library: `fetch` | `deposit_witness` in `crates/ackinacki-bridge/src/deposit/evm.rs` |
| `ackinacki-bridge` (CLI) | `eccBridge` | `finalizeDeposit` | `crates/ackinacki-bridge/src/deposit/an.rs:754` |
| `ackinacki-bridge` (CLI) | user multisig | `sendTransaction` carrying `initiateWithdrawal` | `crates/ackinacki-bridge/src/burn.rs:267`, payload `burn.rs:497` |
| `ackinacki-bridge` (CLI) | Block Manager | `REST /v2/account`, `/v2/messages`: getters, `finalizeDeposit`, `sendTransaction` | getters `crates/ackinacki-bridge/src/deposit/an.rs:484`, `deposit/an_preflight.rs:69`, `:297`, `:349`; messages `deposit/an.rs:837`, `burn.rs:359` |
| `ackinacki-bridge` (CLI) | Block Manager | GraphQL: `account`, its `messages` and `transactions`, `transaction`, `block`, `blockByHeight` | deposit `crates/ackinacki-bridge/src/deposit/an.rs:510`, `:573`, `:593`, `:618`; withdrawal `crates/bridge-relayer-daemon/src/withdraw_e2e/capture.rs:73`, `:142`, `:236`, `crates/bridge-prover-libraries/bridge-event-witness/src/enrich.rs:541`, client at `crates/ackinacki-bridge/src/orchestrator.rs:864` |
| `ackinacki-bridge` (CLI) | WalletConnect relay | wss | `crates/ackinacki-bridge/src/deposit/args.rs:290`, `deposit/wc/mod.rs:168` |
| `ackinacki-bridge` (CLI) | `AckiNackiBridge` | `withdrawByProof` | `crates/bridge-relayer-daemon/src/bridge.rs:1394`, called from `crates/ackinacki-bridge/src/orchestrator.rs:1104` |
| `ackinacki-bridge` (CLI), `relayer daemon-live` | `aggregate-proof` | process | started at `crates/bridge-relayer-daemon/src/aggregator.rs:606`; the CLI hands it the binary at `crates/ackinacki-bridge/src/orchestrator.rs:973`, the daemon at `crates/bridge-relayer-daemon/src/bin/relayer.rs:2525` |
| `deposit-relayer daemon` | `deposit-receipt` | library: `fetch` | `ReceiptWitnessBuilder::build` in `crates/deposit-relayer-daemon/src/witness.rs` |
| `deposit-receipt` | Arbitrum RPC node | JSON-RPC: `eth_getTransactionReceipt`, `eth_getBlockByHash`, `eth_getBlockReceipts` | `crates/deposit-receipt/src/lib.rs:207`, `:219`, `:228`; through the caller's provider, so the same RPC node the CLI or the daemon uses |
| `deposit-relayer daemon` | `eccBridge` | `finalizeDeposit` | `crates/acki-nacki-interface/src/tvm_client.rs:130` |
| `deposit-relayer daemon` | Block Manager | `REST /v2/account`, `/v2/messages`: `isAcceptedBlockHash`, voucher state, `finalizeDeposit` | `crates/deposit-relayer-daemon/src/submitter.rs:713`, `submitter.rs:725`; through `crates/acki-nacki-interface/src/tvm_client.rs:111` (`get_account`), `:159` (`process_message`) |
| `relayer daemon-live` | `AckiNackiBridge` | `verifyBlock` | `crates/bridge-relayer-daemon/src/bridge.rs:1350` |
| `relayer daemon-live` | `AckiNackiBridge` | `applyBkSetUpdate` | `crates/bridge-relayer-daemon/src/bridge.rs:1490` |
| `relayer daemon-live` | Block Manager | GraphQL: `blocks`, `blockByHeight`, `bkSetUpdates` | `crates/bridge-prover-libraries/bridge-prover-lib/src/live_driver/mod.rs:733`, `live_driver/bundle.rs:261`, `live_driver/bk_update.rs:47`, `crates/bridge-prover-libraries/bridge-gql-fetcher/src/bk_set_fetcher.rs:62`; client at `crates/bridge-relayer-daemon/src/bin/relayer.rs:2241` |
| `eth-lc-relayer arbitrum-anchors` | `ArbitrumBlockVerifier` | `proveBlock` | `crates/eth-light-client-relayer/src/arbitrum.rs:1236` |
| `eth-lc-relayer arbitrum-anchors` | Block Manager | `REST /v2/account`, `/v2/messages`: `getL2Verifier`, `getConfig`, `isAcceptedBlockHash`, `proveBlock` | `crates/eth-light-client-relayer/src/arbitrum.rs:1150`, `:1173`, `:1214`, `:1236`; through `crates/acki-nacki-interface/src/tvm_client.rs:111` (`get_account`), `:159` (`process_message`) |
| `eth-lc-relayer daemon` | `EthBeaconLightClient` | `submitUpdate`, `submitAncestry`, `submitRotate` | `crates/eth-light-client-relayer/src/submitter.rs:879`, `:902`, `:890` |
| `eth-lc-relayer daemon` | Block Manager | `REST /v2/account`, `/v2/messages`: light-client getters, `submitUpdate`, `submitAncestry`, `submitRotate` | `crates/eth-light-client-relayer/src/submitter.rs:931`, `:1034`, `:1047`, `:879`, `:902`, `:890`; through `crates/acki-nacki-interface/src/tvm_client.rs:111` (`get_account`), `:159` (`process_message`) |
| `eth-lc-relayer daemon` | `eth-light-client-prover` | process: step (`export_step_vk_blob`); the rotate in three stages, `export_shard_snark`, `export_step_snark`, `rotate_tree_n8` | `crates/eth-light-client-relayer/src/prover.rs:207`; `crates/eth-light-client-relayer/src/rotate.rs:537`, `:543`, `:550` |
| `AckiNackiBridge` | verifiers | `verify*` | `contracts/ethereum/src/AckiNackiBridge.sol:931`, `:939`, `:951`, `:1503` |
| `AckiNackiBridge` | USDC | `approve` | `contracts/ethereum/src/AckiNackiBridge.sol:1997` (the AAVE pool), `:1863` (the successor bridge) |
| `AckiNackiBridge` | Pool (AAVE V3) | `supply`, `withdraw` | `supply`: `contracts/ethereum/src/AckiNackiBridge.sol:1999`, from `deposit` at `:885`. `withdraw`: `:2014`, `:2037`, from `withdrawByProof` at `:1632`; and from the owner's `withdrawFromAave` (`:1712` → `:2037`), `harvestYield` (`:1779`) and `emergencyWithdrawAll` (`:1724`) |
| `AckiNackiBridge` | aUSDC (AAVE V3) | `balanceOf` | `contracts/ethereum/src/AckiNackiBridge.sol:2056` |
| `ArbitrumBlockVerifier` | `eccBridge` | `acceptBlockHashFromL2Verifier` | `contracts/an/exchange/ArbitrumBlockVerifier.sol:136` |
| `EthBeaconLightClient` | `eccBridge` | `acceptBlockHashFromLightClient` | `contracts/an/exchange/EthBeaconLightClient.sol:678` |
| `DepositVoucher` | `eccBridge` | `confirmTokenDeposit` | `contracts/an/exchange/DepositVoucher.sol:53` |

Both the CLI and `deposit-relayer daemon` send `finalizeDeposit` for the same deposit; the second is
a no-op, because the `DepositVoucher` address is derived from the deposit's identity
(`contracts/an/exchange/eccBridge.sol:980`) and a voucher is deployed once.

## Executables and state files

A UML deployment diagram at specification level: which executable manifests which process, and
which directory each process keeps its state in. Servers and containers are not drawn; the
container layout is in [`docker/README.md`](../docker/README.md).

![UML deployment diagram at specification level: executables, the processes they manifest and their state directories](bridge-executables.svg)

- **«executable»** — a UML artifact: the binary file. **«file»** — one file on disk.
  **«directory»** — a directory, with the files it holds drawn as artifacts inside it; `{op}` and
  `<key>` stand for one file per deposit or per withdrawal.
- **`{flag = …, env = …}`** — tagged values: the command-line flag and the environment variable that
  set the directory. `<state-dir>` is that flag's value: the daemons have no default and require it;
  in the containers of `docker/compose.yaml` it is `/data/state`. What each file holds is in the
  table below.
- **«manifest»** — the executable implements the process. **«use»** — the process reads and writes
  the files in the directory.
- A daemon's state is a directory (bridge-audit#132): the state file, its lock and the temporary
  file of its atomic write (`.tmp`, not drawn) are inside it. The flags that named the state file
  are gone; an old command line with `--state`, `DEPOSIT_RELAYER_STATE` or `RELAYER_STATE_PATH`
  fails with a migration hint.
- Every daemon also takes `--work-dir`, the files of a proof in flight, removed once read back, and
  `--log-dir`, its log one file per day and the prover transcripts. Neither holds state, and neither
  is drawn, with one exception: `eth-lc-relayer daemon` keeps a rotate in `<work-dir>/rotate-<P>/`,
  one directory per stage, until the contract has the rotate, so a restart proves only the stage
  that had not finished.
- One `eth-lc-relayer` executable runs two processes, the `daemon` and `arbitrum-anchors` commands.
  `eth-light-client-prover` is four executables from `eth-light-client-prover/examples`: the step
  and the rotate's three stages.
- Not drawn: the provers' own state, and the read-only inputs — SRS and keys (`--params-dir`,
  `--prover-dir`), verifier bytecode, ABIs. The exception is `<prover-dir>/rotate-keys/`: the rotate
  stages write their proving keys there themselves.

| File | Holds | Default path | Code |
|---|---|---|---|
| `~/.bridge-deposit-state/` | `{op}.json`, the record of one deposit; `{op}.lock`, held while a run works on that deposit; `deposit.lock`, held from asking the wallet until the deposit is on chain, so one run does that at a time | `crates/ackinacki-bridge/src/deposit/args.rs:562` | `crates/ackinacki-bridge/src/deposit/store.rs:278`, `deposit/locks.rs:182`, `:153` |
| `~/.bridge-withdraw-state/` | `<key>.json`, the record of one withdrawal; `<key>.lock`, held while a run works on it | `crates/ackinacki-bridge/src/orchestrator.rs:2399` | `crates/ackinacki-bridge/src/idempotency.rs:919`, `:1079` |
| `deposit-relayer-state.json` | deposit ids processed and finalized (or parked), the outstanding ones, the scan cursor | none, `--state-dir` is required: `crates/deposit-relayer-daemon/src/bin/deposit-relayer.rs:71` | `crates/deposit-relayer-daemon/src/dirs.rs:20`, `state.rs:105` |
| `deposit-relayer-state.json.lock` | an exclusive lock held while the daemon runs; a second one refuses to start | in the state directory | `crates/deposit-relayer-daemon/src/state.rs:586` |
| `./state/` | `prover_state.json`: the layer windows, the BK-set commitment, the last key block applied; `prover_bk_set.json`; `bootstrap_seed.json`; `relayer-state.json`, the daemon's cursor, which `daemon-live` keeps only here and refuses another `--state-dir` for | `crates/bridge-relayer-daemon/src/bin/relayer.rs:415` | `crates/bridge-prover-libraries/bridge-prover-lib/src/bridge_state.rs:157`, `paths.rs:83`, `:87`, `:95`; `crates/bridge-relayer-daemon/src/bin/relayer.rs:95`, `:65` |
| `eth-lc-relayer-state.json` | the last finalized slot, the committee period, the header spans still owed, a rotation in flight | none, `--state-dir` is required: `crates/eth-light-client-relayer/src/bin/eth-lc-relayer.rs:74` | `crates/eth-light-client-relayer/src/dirs.rs:21`, `state.rs:30` |
| `eth-lc-relayer-state.json.lock` | an exclusive lock held while the daemon runs; a second one refuses to start | in the state directory | `crates/eth-light-client-relayer/src/state.rs:124` |
| `<work-dir>/rotate-<P>/` | the rotate of period P: one directory per stage, `shard/`, `step/`, `tree/`, each written whole or not at all; removed once the contract is past P | none, `--work-dir` is required: `crates/eth-light-client-relayer/src/bin/eth-lc-relayer.rs:82` | `crates/eth-light-client-relayer/src/rotate.rs:163`, `:177`; removed at `crates/eth-light-client-relayer/src/relayer.rs:697` |
| `<prover-dir>/rotate-keys/` | the proving keys of every rotate stage and tree level, each tied to the build of its prover; written on the first rotate after an install or a binary upgrade, reused after that | none, `--prover-dir` is required when the daemon may rotate: `crates/eth-light-client-relayer/src/bin/eth-lc-relayer.rs:329` | `crates/eth-light-client-relayer/src/rotate.rs:482`; written by `eth-light-client-prover/src/key_cache.rs` |
| `arbitrum-anchors-state.json` | the parent-chain and deposit cursors, the deposit blocks still pending, the slot proofs taken per batch, the proofs ready to send | none, `--state-dir` is required: `crates/eth-light-client-relayer/src/bin/eth-lc-relayer.rs:92` | `crates/eth-light-client-relayer/src/dirs.rs:23`, `arbitrum.rs:228` |
| `arbitrum-anchors-state.json.lock` | an exclusive lock held while the process runs; a second one refuses to start | in the state directory | `crates/eth-light-client-relayer/src/state.rs:124` |

## Not on the diagram

- The user's wallet itself (a WalletConnect app or a local key); only the WalletConnect relay the CLI
  talks to is drawn.
- `BlockHeaderOracle` and the prover's helper daemons.
- What the AAVE pool calls itself: it takes the USDC with `transferFrom` on `supply`, pays it out
  with `transfer` on `withdraw`, and mints and burns aUSDC. That is AAVE's code, not this
  repository's.
- `finalizeDeposit` deploying the `DepositVoucher`: the creation arrow is left out.
