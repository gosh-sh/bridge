# Acki Nacki Bridge — Agent Context

## Documentation status — read before citing any document

**Everything outside `docs/archive/` is current. `docs/archive/` is not.** When code and prose
disagree, the code wins.

- [`DOCS.md`](DOCS.md) is the register of current documents, the rules for writing a new one, and a
  list of claims the archived docs get wrong that have already been checked against the source —
  read it before re-deriving any of them.
- [`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md) is the verified reference for the
  Ethereum contracts. Its bare `src/`, `test/`, `script/`, `verifiers/` paths are relative to
  `contracts/ethereum/`.
- `docs/archive/` is history, scheduled for deletion. Do not cite it, do not follow its procedures,
  and do not repeat its claims without re-checking them against the source.

## Project overview

Cross-chain bridge between Ethereum and [Acki Nacki](https://docs.ackinacki.com/) (TVM-based,
multi-threaded). USDC deposited on Ethereum is proven on Acki Nacki and minted there; Acki Nacki
state is proven on Ethereum, which pays withdrawals out against it. Both directions use Halo2
proofs. [`README.md`](README.md) walks through both directions and the repository.

## Changelog policy

Every branch that is opened as a pull request into `main` must describe its diff
against `main` in [`CHANGELOG.md`](CHANGELOG.md). No PR is complete without it.

### Write for the reader, not for the author

The reader is a devops engineer or a developer who deploys the bridge, runs the
relayers and integrates with the contracts. They did not write the code and will
not read it. Describe the surface they can observe, in plain language:

- contract surface: external and public functions, events, errors, storage
  layout changes, upgrade steps, deployed addresses — for both
  `contracts/ethereum/` (Solidity/Foundry) and `contracts/an/` (TVM)
- proof surface: circuit public inputs, bincode layout, verification keys.
  **A rotated verification key is always a breaking change** — proofs produced
  for the previous circuit stop verifying, and that has to be spelled out.
- relayer and prover surface: CLI subcommands and flags, systemd units, config
  files, environment variables, the JSON artifacts they read and write
  (`proof_<N>.json`, `proof_event_*.json`), retry and idempotency behaviour
- operational surface: `scripts/`, `Makefile` targets, `build.sh` / `setup.sh` /
  `test.sh`, `docker-compose.yml`, the release assets `.woodpecker/release.yaml`
  publishes
- chain and network assumptions: supported L1 chain ids, RPC requirements,
  key-block spacing, anchoring cadence

Say what changed and what the reader has to do about it — redeploy a contract,
rotate a key, regenerate proofs, carry a setting over by hand, upgrade in a
particular order. Name functions, flags, files and options exactly as they
appear in the product.

Leave out internal refactors, private renames, test-only changes and
implementation detail. If nothing observable changed, there is nothing to write.

Sections, most disruptive first: `Breaking Changes`, `Added`, `Changed`,
`Fixed`, `Removed`.

### Versions are assigned late

Release numbers are fixed only when a release is actually cut and tagged. While
work is landing on `main`, nobody knows which release it will ship in.

While working on a branch:

- add entries under `## [Unreleased]` at the top of `CHANGELOG.md`, directly
  above the newest released version; create that section if it is missing
- do not invent a version heading, and do not bump `version` in any
  `Cargo.toml` — neither `workspace.package.version` nor any crate outside the
  root workspace
- add to the existing groups under `## [Unreleased]` rather than starting a
  second copy of them

At release time a human — not an agent — picks the real version number, bumps it
in every affected manifest, renames `## [Unreleased]` to
`## [<version>] – <YYYY-MM-DD>`, and tags the commit.

Sections of already released versions are history. Do not rewrite them, do not
move entries out of them, and do not append new entries to them.

## Git remotes

`origin` is `https://github.com/gosh-sh/bridge.git` — pull requests, merge target `main`, the
pipelines under `.woodpecker/`. A fresh clone needs nothing else. Older checkouts may still have
`origin` at `vcs.modus-ponens.com` and GitHub as a second remote named `github`; that was the
arrangement while GitLab was canonical, and `.gitlab-ci.yml` is what remains of it.

## Repository layout

```
contracts/ethereum/        Solidity (Foundry): AckiNackiBridge, SHPLONK verifier adapters, oracles.
                           verifiers/ holds the production Yul bytecode (.bin) and its source (.sol)
contracts/an/              TVM (gosh-solidity): exchange/ has eccUSDCBridge, DepositVoucher and the
                           Ethereum beacon light client, with the compiled artefacts acki-nacki pins by
                           commit; zerostate/ assembles the bridge account; place.json tells acki-nacki
                           what to place where
deposit-prover/            ETH→AN deposit circuit (Halo2 on axiom-eth), standalone package
eth-light-client-prover/   Ethereum sync-committee light-client circuits (step, rotate), standalone
crates/
  acki-nacki-interface/    traits + mocks for AN node access; BkSetClient for /v2/bk_set{,_update}
  deposit-chain-ids/       the EVM chains the deposit bridge accepts — single source of truth
  eth-frontend/            Ethereum client (alloy)
  deposit-relayer-daemon/  ETH→AN relayer: Deposit logs → deposit-prover → finalizeDeposit on AN
  eth-light-client-relayer/ finality_update → step/rotate proofs → EthBeaconLightClient on AN
  bridge-prover-libraries/ AN→ETH prover workspace (gosh halo2 fork): prover lib and live driver,
                           GraphQL fetcher, prover and verifier daemons, Circuit 4 event witness
                           and prover, snark wrap
  bridge-relayer-daemon/   AN→ETH relayer, `relayer` CLI — symlinked member of the prover workspace
  ackinacki-bridge/        end-user withdrawal CLI — symlinked member of the prover workspace;
                           shipped as a release download, QUICKSTART.md is the operator's entry point
  bridge-snark-utils/      offline SNARK tools: bound Circuit 1A/1B/2 fixtures, Poseidon snark export
  bridge-evm-aggregator/   SHPLONK aggregator: export-inner-aggregator writes the production
                           verifiers, aggregate-proof is the prover subprocess the CLI shells out to
frontend/                  WASM deposit UI (Yew)
scripts/                   operational scripts; CI checks live here too
params/                    SRS files
docs/                      current documentation (see DOCS.md); docs/archive/ is history
.woodpecker/               the CI that runs; .gitlab-ci.yml is the GitLab remote's, nothing runs it
```

## Cargo workspaces

The root workspace holds `crates/eth-frontend`, `crates/acki-nacki-interface` and
`crates/deposit-chain-ids`. Everything else is outside it on purpose: the halo2 forks in play cannot
share a dependency tree. **Do not move a crate into the root workspace, and do not try to reconcile
those dependency versions.**

- `crates/bridge-prover-libraries` is its own workspace. `crates/bridge-relayer-daemon` and
  `crates/ackinacki-bridge` are symlinked members of it and inherit its dependencies, so cargo cannot
  even parse their manifests on their own: every cargo command for them runs from
  `crates/bridge-prover-libraries` with `-p <crate>`.
- `deposit-prover`, `eth-light-client-prover` and `crates/bridge-evm-aggregator` declare their own
  `[workspace]`. `deposit-prover` also has its own `rust-toolchain.toml`.
- `crates/deposit-relayer-daemon`, `crates/eth-light-client-relayer`, `crates/bridge-snark-utils` and
  `frontend` are standalone packages excluded by the root `Cargo.toml`.

## How the two directions work

**Ethereum → Acki Nacki (deposit).**
`AckiNackiBridge.deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount)` takes USDC and emits
`Deposit`. `deposit-prover` proves that event is in a block whose header hashes to the proven block
hash — receipt MPT, log binding, a keccak coprocessor. Its 12 public inputs are defined once, in
`deposit-prover/src/circuit_v2.rs::DEPOSIT_PUBLIC_INPUT_LAYOUT`: `depositId`, `sender`, `amount`,
`contractAddress`, `chainId`, `dappIdHigh`, `dappIdLow`, `anAccountHigh`, `anAccountLow`,
`blockHashHigh`, `blockHashLow`, `promiseCommit`. On Acki Nacki, `eccUSDCBridge.finalizeDeposit`
(`contracts/an/exchange/`):

1. requires `(chainId, contractAddress)` to be a trusted L1 bridge (`setTrustedL1Bridge`);
2. verifies the proof natively with `gosh.zkhalo2VerifyWithVK` (the `ZKHALO2VERIFYWITHVK` opcode,
   from `tvm-sdk`) against the embedded `VK_BLOB`;
3. requires the proven block hash in the anchor set. The proof shows the event is in *a* block, not
   that the block is canonical; anchors are written by the owner (`setAcceptedBlockHash`, until the
   one-way `disableOwnerAnchors()`) and by the Ethereum beacon light client
   (`acceptBlockHashFromLightClient`) — see [`docs/eth-light-client.md`](docs/eth-light-client.md);
4. deploys a `DepositVoucher` from the embedded `_depositVoucherCode`; the voucher is the replay
   slot, and it calls back `confirmDeposit`, which mints.

**Acki Nacki → Ethereum (state and payout).** `AckiNackiBridge` advances its commitment to AN state
with `verifyBlock` (Circuit 1A primary or 1B fallback attestation, plus Circuit 2 layer hashes),
rotates the BK-set commitment with `applyBkSetUpdate`, and pays out with `withdrawByProof` against a
Circuit 4 proof, nullifier-guarded. Each circuit is verified by a SHPLONK aggregator Yul verifier in
`contracts/ethereum/verifiers/`. The proofs come from `crates/bridge-prover-libraries` and the
relayer submits them. Contract detail: [`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md);
custody and accounting: [`docs/EVM-custody-and-accounting.md`](docs/EVM-custody-and-accounting.md).

## Pitfalls

- **Never hand-edit the VkBlob hex** in `eccUSDCBridge.sol`: rotate it with
  `scripts/embed_deposit_vk_blob.py contracts/an/exchange/eccUSDCBridge.sol`; CI runs the same script
  with `--check`. After a deposit-circuit change, run the MockProver pre-flight
  (`cd deposit-prover && cargo run --release --example mock_fixture -- <input.json> [--mutate header-pad]`)
  before paying for keygen.
- **The bridge and its voucher are recompiled and redeployed as a pair.** `eccUSDCBridge` embeds
  `DepositVoucher`'s code, so a change to the voucher's ABI compiles cleanly against a stale
  `_depositVoucherCode` and then aborts every voucher constructor on chain (`exit_code 9`) — no
  deposit mints. `scripts/check_voucher_abi_consistency.py` (in CI) compares the sources and the
  compiled ABIs under `contracts/an/`. It does not cover the ABI copies the tooling loads,
  `crates/ackinacki-bridge/abi/USDCBridge.abi.json` and
  `crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json`, whose `confirmDeposit`
  still lacks `chainId`.
- **The KZG setup is Hermez.** Every prover in this repository loads the Hermez Perpetual Powers of
  Tau (`s_g2` head `928fafb3d0cc`) and refuses anything else: `assert_hermez_ceremony` in
  `crates/bridge-evm-aggregator/src/srs_guard.rs`, `assert_hermez_srs` in
  `crates/bridge-prover-libraries/bridge-prover-lib/src/keys/common.rs`, and
  `load_kzg_params_from_trusted_setup` in `deposit-prover/src/prover.rs`, which reads only
  `data/kzg_params_{k}.srs`. A missing SRS must never be replaced by `gen_srs`: whoever generated it
  knows tau and can forge proofs. The header of `deposit-prover/download_trusted_setup.sh` still
  describes an Acki Nacki chain ceremony (`c6028acf…`) as the production deposit setup; the code does
  not load it.
- **A fresh clone or worktree has no Solidity dependencies** (`lib/` and `node_modules/` are
  gitignored): `cd contracts/ethereum && npm install && forge install --no-git foundry-rs/forge-std`,
  or `make setup`.

## Upstream repositories

Pinned in the `Cargo.toml` files; clone them next to this repository when you need their source.

| Repository | Role |
|---|---|
| `gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits` | The AN→ETH circuits (1A/1B, 2, 4), pinned by `rev` in `crates/bridge-prover-libraries/Cargo.toml`. Private: cargo needs a token to fetch it |
| `tvmlabs/tvm-sdk` | TVM SDK, including the `ZKHALO2VERIFYWITHVK` opcode the AN side verifies deposits with |
| `acki-nacki` | The node. Pins a commit of this repository and places the files `contracts/an/place.json` lists |
| `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381`, `gosh-sh/halo2-axiom`, `gosh-sh/gosh-halo2-crypto-lib`, `gosh-sh/axiom-eth`, `gosh-sh/snark-verifier` | The gosh halo2 forks and chips the circuits and provers build on |

Acki Nacki endpoints: shellnet GraphQL is `https://shellnet.ackinacki.org/graphql`, the default of
every daemon here. REST `/v2/bk_set` and `/v2/bk_set_update` are served by a node directly, on port
8600 — not by `shellnet.ackinacki.org`, which answers 404 for them. A local cluster runs from
`../acki-nacki/nock` with `docker-compose`, node 0 at `http://127.0.0.1:11000`.

## Build and test

```bash
make setup                 # toolchains and Solidity dependencies
make build                 # root workspace + Solidity
make test                  # root workspace tests + forge test
make pre-push              # everything a branch should pass; see CI below

cd contracts/ethereum && forge test                          # fork suites skip without FORK_URL
FOUNDRY_PROFILE=fork FORK_URL=<RPC URL> forge test --match-contract AaveFork

# AN→ETH relayer — only from the prover workspace
cd crates/bridge-prover-libraries && cargo test --locked -p bridge-relayer-daemon
cd crates/bridge-prover-libraries && cargo run -p bridge-relayer-daemon --bin relayer -- --help
# daemon-bridge runs both ETH legs in one process on one EOA: verifyBlock, falling forward through
# proof_<N>.json, and withdrawByProof on proof_event_*.json. daemon-withdraw is the payout leg alone;
# daemon-live proves in-process.
cd crates/bridge-prover-libraries && cargo run -p bridge-relayer-daemon --bin relayer -- daemon-bridge \
    --proofs-dir <prover proofs/> --rpc-url ... --bridge-address ... --private-key ...
# The fixture commands read bound_scenario.json from --fixtures-dir and the R15 calldata from
# --verifiers-dir. Left out, --verifiers-dir is searched for among the ancestors of --fixtures-dir,
# which finds it only when that path is absolute.
cd crates/bridge-prover-libraries && cargo run -p bridge-relayer-daemon --bin relayer -- verify-fixture \
    --fixtures-dir ../bridge-snark-utils/proofs/bound --verifiers-dir ../../contracts/ethereum/verifiers \
    --rpc-url ... --bridge-address ...                   # read-only pre-flight, exits non-zero on mismatch
cd crates/bridge-snark-utils && cargo run --release --bin export-bound-block-proofs   # writes proofs/bound/

# ETH→AN deposit relayer
cd crates/deposit-relayer-daemon && cargo test           # the live test is #[ignore]d
cd crates/deposit-relayer-daemon && cargo run --bin deposit-relayer -- --help   # watch, prove-one, an-preflight, daemon
cd crates/deposit-relayer-daemon && BRIDGE_DEPLOY_BLOCK=<block> SEPOLIA_RPC_URL=<RPC> \
    cargo test --test live_log_discovery -- --ignored --nocapture
```

## CI

### Woodpecker (`.woodpecker/`) — what runs

| Pipeline | Trigger | Checks |
|---|---|---|
| `hygiene.yaml` | PRs, pushes to `main` | `gitleaks` over the whole history (`.gitleaks.toml`, `.gitleaksignore`); `lychee` over the Markdown (`lychee.toml`); English-only text (`scripts/check_english_only.py`, `make english-check`; a `non-english-ok` marker exempts a line) |
| `solidity.yaml` | PRs, pushes to `main` | `forge build`, `forge fmt --check`, `forge test --no-match-contract Fork` |
| `an-contracts.yaml` | PRs, pushes to `main` | What the TVM compiler does not check: the voucher ABI, the embedded deposit VK against `deposit-prover/fixtures/`, the zerostate encoder and its module tests, `contracts/an/place.json` |
| `verifier_sources.yaml` | PRs, pushes touching `contracts/ethereum/verifiers/` | Every `*AggregatorVerifier.sol` compiles with `solc` 0.8.19 to its `.bin` byte for byte; EIP-170 |
| `release.yaml` | tags `v*` | Builds `ackinacki-bridge` and `aggregate-proof` and publishes the three assets `crates/ackinacki-bridge/scripts/install.sh` downloads. Builds only |
| `request_review.yaml`, `notify_review_submitted.yaml` | PRs; cron | Re-requests stale reviews and pings reviewers and authors in Discord |

Each pipeline's header comment has the detail. A secret reaches only the events ticked on it, and a
missing one arrives as an empty string rather than an error.

**No Rust job runs on a PR or on `main`**, and neither do the fork suites or `forge coverage`. They
exist only as GitLab jobs in `.gitlab-ci.yml`, which nothing runs from GitHub, so `make pre-push` is
what stands between a branch and a Rust regression.

### What no pipeline runs

`make pre-push` covers the root workspace, `bridge-relayer-daemon` and `bridge-evm-aggregator`. No
pipeline and no `make` target runs the tests of:

- the other members of `crates/bridge-prover-libraries` — `ackinacki-bridge`, `bridge-prover-lib`,
  `bridge-gql-fetcher`, `bridge-event-prover-lib`, `bridge-event-witness`; run them from that
  directory with `cargo test --locked -p <crate>`;
- the standalone crates, each from its own directory with `cargo test`: `deposit-prover`,
  `eth-light-client-prover`, `deposit-relayer-daemon`, `eth-light-client-relayer`,
  `bridge-snark-utils`, `frontend`.

### Before pushing

`make pre-push` runs `make english-check`, `make format-check`, `make lint`, `make relayer-fmt`,
`make relayer-clippy`, `forge test`, `make coverage-solidity`, `cargo test --workspace --locked`,
`make relayer-test` and `make aggregator-test`. It does not run the `an-contracts.yaml` checks,
`scripts/check_verifier_sources.sh`, gitleaks or lychee.

**It does not go green today:** `make relayer-clippy` and `make relayer-test` fail to compile,
because the `crates/bridge-prover-libraries` workspace does not build against its pinned circuit
revision.

`forge coverage` is there for two failure modes a plain `forge test` can miss. A fuzz test whose
`vm.assume` rejects nearly every input trips Foundry's rejection cap depending on the seed — use
`bound()` or a unit test instead. And coverage turns off the optimizer and `via_ir`, so a function
with more than 16 live locals fails with `Stack too deep` there even though `forge build` passes.

The nightly is pinned in `rust-toolchain.toml` because `rustfmt.toml` uses nightly-only options and
`make lint` runs clippy with `-D warnings`. When bumping it, land the resulting reformat in the same
commit.

## Where to look next

- [`DOCS.md`](DOCS.md) — the register of current documents.
- [`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md),
  [`docs/EVM-custody-and-accounting.md`](docs/EVM-custody-and-accounting.md),
  [`docs/eth-light-client.md`](docs/eth-light-client.md), [`docs/aave-yield.md`](docs/aave-yield.md).
- [`contracts/an/README.md`](contracts/an/README.md),
  [`contracts/ethereum/verifiers/README.md`](contracts/ethereum/verifiers/README.md).
- Crate READMEs: [`deposit-prover`](deposit-prover/README.md),
  [`eth-light-client-prover`](eth-light-client-prover/README.md),
  [`eth-light-client-relayer`](crates/eth-light-client-relayer/README.md),
  [`bridge-evm-aggregator`](crates/bridge-evm-aggregator/README.md),
  [`bridge-prover-libraries`](crates/bridge-prover-libraries/TECHNICAL_README.md),
  [`ackinacki-bridge`](crates/ackinacki-bridge/README.md) and its
  [`QUICKSTART.md`](crates/ackinacki-bridge/QUICKSTART.md).
