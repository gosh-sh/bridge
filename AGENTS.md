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
proofs. [`README.md`](README.md) walks through both directions and the repository. In more depth:
the deposit circuit and its 12 public inputs in [`deposit-prover/README.md`](deposit-prover/README.md),
the Acki Nacki contracts in [`contracts/an/README.md`](contracts/an/README.md), the Ethereum contracts
in [`docs/EVM-contracts-spec.md`](docs/EVM-contracts-spec.md).

## Issues, branches and pull requests

One Linear issue (or one GitHub issue) → one branch and one pull request → one assignee.

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
  bridge-circuits/         AN→ETH halo2 circuits (1A/1B, 2, 4, poseidon reference, test-data-gen),
                           vendored from gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits;
                           its own sub-workspace with a distinct halo2 backend
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
- `deposit-prover`, `eth-light-client-prover`, `crates/bridge-evm-aggregator` and
  `crates/bridge-circuits` declare their own `[workspace]`. `deposit-prover` also has its own
  `rust-toolchain.toml`. `bridge-circuits` uses the gosh-fork halo2 backend that the prover pins;
  the root's halo2-axiom cannot coexist with it, which is why the root workspace excludes it.
- `crates/deposit-relayer-daemon`, `crates/eth-light-client-relayer`, `crates/bridge-snark-utils` and
  `frontend` are standalone packages excluded by the root `Cargo.toml`.

## Upstream repositories

Pinned in the `Cargo.toml` files; clone them next to this repository when you need their source.

| Repository | Role |
|---|---|
| `gosh-sh/acki-nacki-to-eth-bridge-halo2-circuits` | The AN→ETH circuits (1A/1B, 2, 4). Vendored under `crates/bridge-circuits/` — nothing in this repository fetches it any more; a clone of the upstream repo is only useful if you want its issue tracker or its pre-vendor history |
| `tvmlabs/tvm-sdk` | TVM SDK, including the `ZKHALO2VERIFYWITHVK` opcode the AN side verifies deposits with |
| `acki-nacki` | The node. Pins a commit of this repository and places the files `contracts/an/place.json` lists |
| `gosh-sh/halo2-lib-zkevm-sha256-and-bls12-381`, `gosh-sh/halo2-axiom`, `gosh-sh/gosh-halo2-crypto-lib`, `gosh-sh/axiom-eth`, `gosh-sh/snark-verifier` | The gosh halo2 forks and chips the circuits and provers build on |

Acki Nacki endpoints and ports are in [`docs/eth-light-client.md`](docs/eth-light-client.md). REST
`/v2/bk_set` and `/v2/bk_set_update` are served only by a node's own API on port 8600;
`shellnet.ackinacki.org` answers 404 for them.

## Build and test

```bash
make setup                 # toolchains and Solidity dependencies
make build                 # root workspace + Solidity
make test                  # root workspace tests + forge test
make test-all              # every crate's tests + forge test; lists what failed at the end
make pre-push              # everything a branch should pass; see CI below
make format                # rustfmt every crate, then forge fmt

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
| `bridge-circuits.yaml` | PRs, pushes to `main` | Vendored halo2 circuits under `crates/bridge-circuits/`: fast MockProver step (Circuit 4 + cross-circuit block-id) plus a heavy step (attestation-BLS, layer-hashes movement, poseidon, test-data-gen) serialised with `RUST_TEST_THREADS=1` |
| `release.yaml` | tags `v*` | Builds `ackinacki-bridge` and `aggregate-proof` and publishes the three assets `crates/ackinacki-bridge/scripts/install.sh` downloads. Builds only |
| `request_review.yaml`, `notify_review_submitted.yaml` | PRs; cron | Re-requests stale reviews and pings reviewers and authors in Discord |

Each pipeline's header comment has the detail. A secret reaches only the events ticked on it, and a
missing one arrives as an empty string rather than an error.

**The only Rust job that runs on a PR or on `main` is `bridge-circuits.yaml`** (added when the
circuits were vendored — previously they lived in a private repo, so no CI here could reach
them). Every other Rust crate — including the root workspace, `bridge-relayer-daemon`,
`bridge-evm-aggregator`, `deposit-prover` and friends — has no PR-triggered pipeline and neither
do the fork suites or `forge coverage`. Those exist only as GitLab jobs in `.gitlab-ci.yml`, which
nothing runs from GitHub, so `make pre-push` is what stands between a branch and a Rust regression
in the three crates it explicitly runs — the root workspace, `bridge-relayer-daemon` and
`bridge-evm-aggregator`. Everything else (see the next section) is on the person pushing.

### What no pipeline runs

`make pre-push` covers the root workspace, `bridge-relayer-daemon` and `bridge-evm-aggregator`. No
pipeline runs the tests of the crates below; `make test-all` runs them with everything else, keeps
going past a failing suite and lists the failures at the end, leaving `#[ignore]`d tests skipped. To
run one on its own:

- the other members of `crates/bridge-prover-libraries` — `ackinacki-bridge`, `bridge-prover-lib`,
  `bridge-gql-fetcher`, `bridge-event-prover-lib`, `bridge-event-witness`; run them from that
  directory with `cargo test --locked -p <crate>`;
- the standalone crates, each from its own directory with `cargo test`: `deposit-prover`,
  `eth-light-client-prover`, `deposit-relayer-daemon`, `eth-light-client-relayer`,
  `bridge-snark-utils`, `frontend`.

`crates/bridge-circuits` is different: `bridge-circuits.yaml` runs its fast + heavy `#[test]`
suites automatically on every PR and every push to `main`, so that sub-workspace is *not* in the
uncovered set above. What still needs a manual run is the `#[ignore]`d `test_real_prover_*` tests
gated for CI hygiene — trigger them per-crate when the corresponding circuit constraints change,
from `crates/bridge-circuits/`. Weight varies by circuit and by case:

```
# Circuit 4, K=19 real keygen + proof — the lightest of the three (well under
# the >14 GB weight class of the attestation-BLS cases below); a bare
# `--ignored` here picks up the single `real_proof_for_fixed_k` test.
cargo test -p bridge-event-prove-circuit -- --ignored

# Layer-hashes real prover sweep — tens of seconds per proof, aggregator-lite
# (see the per-test doc-comment); also comfortably under the >14 GB weight
# class. Bare `--ignored` picks up the single sweep test.
cargo test -p historical-layer-hashes-movement-checker-circuit -- --ignored
```

**Attestation-BLS is a special case.** A bare `cargo test -p attestation-bls-checker-circuit --
--ignored` sweeps in every `test_real_prover_primary_max_{300,500,1000,2000}`; every case is K=20,
and per `PARALLEL_BENCHMARK_N14_REPORT.md:76-79` the first-proof RSS grows steeply with
`max_signers` (~15.6 GB at 300, ~37.6 GB at 1000, ~77.7 GB at 2000). Do not run the bare set on a
workstation without explicit intent — the 2000-signer case alone will OOM a 64 GB host. Prefer
naming the case:

```
cargo test -p attestation-bls-checker-circuit -- --ignored \
    test_real_prover_primary_max_500 \
    test_real_prover_fallback_multi_bk_set
```

### Before pushing

`make pre-push` runs `make english-check`, `make format-check`, `make lint`, `make relayer-fmt`,
`make aggregator-fmt`, `make relayer-clippy`, `forge test`, `make coverage-solidity`,
`cargo test --workspace --locked`, `make relayer-test` and `make aggregator-test`. It does not run
the `an-contracts.yaml` checks, `scripts/check_verifier_sources.sh`, gitleaks or lychee.

**It does not go green today.** `make aggregator-fmt` fails because
`crates/bridge-evm-aggregator` is not formatted yet, and `make relayer-clippy` fails on a stray
`clippy::useless_conversion` (`useless u8::from(ev.layer)`) that the `-D warnings` gate turns into
an error. The prior wording of this paragraph blamed a pinned circuit revision that the workspace
could not build against; that claim was accurate when the circuits lived in a private git repo,
but it is no longer — since the circuits were vendored, `crates/bridge-prover-libraries` reaches
its circuit deps by path (see [Cargo workspaces](#cargo-workspaces) above), and the build failures
that remain are the fmt drift and the one clippy warning above, not a revision mismatch.

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
