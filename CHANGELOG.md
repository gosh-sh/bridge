# Acki Nacki Bridge Release Notes

All notable changes to the bridge are documented in this file — the contracts on
both chains, the ZK circuits and verification keys, the relayer and prover
binaries, and the deployment and operational surface around them.

Written for the people who deploy and run the bridge, not for the people who
wrote it. See the changelog policy in [AGENTS.md](AGENTS.md) for what belongs
here and how versions are assigned.

## [Unreleased]

<!--
Add entries here, grouped under the headings below, most disruptive first.
Drop a heading if it has no entries. Do not add a version number — a human
assigns it when the release is tagged.

### Breaking Changes
### Added
### Changed
### Fixed
### Removed
-->

### Breaking Changes

- **Sub-workspace directory renamed `crates/an-bridge-prover/` → `crates/bridge-prover-libraries/`.**
  The prover-side sub-workspace root (the one that holds
  `bridge-prover-lib`, `bridge-prover-daemon`, `bridge-verifier-daemon`,
  `bridge-event-witness`, `bridge-event-prover-lib`,
  `bridge-event-halo2-prover`, `bridge-gql-fetcher`, `bridge-snark-wrap`
  and the `bridge-relayer-daemon` / `ackinacki-bridge` symlinks) moves
  under a self-describing name. The workspace exclude in the root
  `Cargo.toml`, every path-dep resolving through the sub-workspace,
  every runbook + script + env file + CI reference has been repointed.
  All Cargo package names inside the sub-workspace are unchanged (only
  the parent directory moved), so `cargo -p bridge-prover-lib` etc. work
  unchanged. Concrete migrations:
    - `cd crates/an-bridge-prover` → `cd crates/bridge-prover-libraries`
      everywhere (docs, scripts, systemd unit files, personal shell
      history).
    - `relayer prove-withdraw --an-bridge-prover-dir …` →
      `--bridge-prover-libraries-dir …`; env var
      `AN_BRIDGE_PROVER_DIR` → `BRIDGE_PROVER_LIBRARIES_DIR`.
    - `BRIDGE_PARAMS_DIR=../an-bridge-prover/params` (in the standalone
      CLI `bridge_config`) → `../bridge-prover-libraries/params`.
    - Compose / systemd bind-mounts pointing at
      `crates/an-bridge-prover/…` need the same path swap.
    - Any personal `.env.local` or shell profile setting
      `AN_BRIDGE_PROVER_DIR=…` needs renaming to
      `BRIDGE_PROVER_LIBRARIES_DIR=…`.

- **CLI crate + binary renamed `bridge-withdraw-e2e-cli` → `ackinacki-bridge`.**
  Cargo package name, `cargo -p …` selector, `[[bin]]` name, on-disk
  crate directory (`crates/bridge-withdraw-e2e-cli/` → `crates/ackinacki-bridge/`),
  and the `bridge-prover-libraries` sub-workspace symlink all move together.
  All existing functionality is now nested under the `withdraw`
  subcommand (already the case in earlier `[Unreleased]` entries — the
  rename is purely cosmetic, made ahead of adding sibling subcommands
  like `deposit`). Concrete migrations:
    - Build: `cargo build -p ackinacki-bridge` (was `-p bridge-withdraw-e2e-cli`),
      run from `crates/bridge-prover-libraries/`.
    - Invoke: `./target/release/ackinacki-bridge withdraw --from … --to …`
      (was `./target/release/bridge-withdraw-e2e-cli withdraw …`).
    - Log filter: `RUST_LOG=ackinacki_bridge=info` (was `bridge_withdraw_e2e_cli=info`).
    - Config: `config/bridge_config` moved with the crate to
      `crates/ackinacki-bridge/config/bridge_config`; contents unchanged.
    - Smoke scripts (`scripts/local_smoke.sh`, `live_smoke.sh`,
      `deploy_msig_and_mint.{sh,py}`) moved with the crate; behaviour unchanged.

- **`contracts/ethereum/.env.shellnet` and `.env.shellnet.l2` deleted.**
  The single shared shellnet burner (`0xb586…2307`) now lives once, in
  `crates/bridge-prover-libraries/shellnet.common` under `RELAYER_PRIVATE_KEY`,
  and covers both the deployer and the relayer role. Manual deploy
  paths that used to `set -a && source contracts/ethereum/.env.shellnet
  && forge script …` are retired — use
  `PRIVATE_KEY=$RELAYER_PRIVATE_KEY LEVEL={1,2}
  ./scripts/deploy_bridge_bundle.sh` from `crates/bridge-prover-libraries/`
  instead (the wrapper re-derives genesis anchors from live chain head
  and writes `L{1,2}_config/env` atomically). Per-deploy provenance
  (Deploy #8 / #12 anchors) remains in git history.

- **Per-mode config directory layout in `crates/bridge-prover-libraries/`.**
  The parallel `state/` + `state_l2/` + `proofs/` + `proofs_l2/` +
  `.env.shellnet` + `.env.shellnet.l2` layout is retired. Runtime data
  now lives under `L1_config/{env,state,proofs,work_dir}` and
  `L2_config/{env,state,proofs,work_dir}`. Shared env lines are
  extracted to a single `shellnet.common` sourced by each per-mode
  `env` file. Operators must migrate any existing on-disk state before
  restarting the daemon (`mv state L1_config/state && mv proofs
  L1_config/proofs`); the file-first startup guard reads only the new
  paths.
- **The first withdrawal after this upgrade regenerates the Circuit-4
  proving keys.** Cached keys now carry an `event_manifest.json` naming the
  circuit revision they were built for, the format version of the manifest
  itself, and a SHA-256 of each of the three key files. Every cache written
  before this release has none — so it is treated as cold and rebuilt.
  There is no way to migrate in place: nothing in the old three files
  records which circuit produced them, which is the gap being closed.

  Cached key artifacts are now written atomically — temp file, fsync,
  rename — so an interrupted or out-of-space keygen leaves no half-written
  key behind. That is **four** files for the event circuit
  (`event_pk.bin`, `event_vk.bin`, `event_config_params.json` and the new
  `event_manifest.json`) and **three** for each of primary, fallback and
  layer: those circuits do not version their keys, so they get no manifest.

  **File modes are unchanged.** A new file gets the mode `File::create`
  would have given it under your umask; rewriting an existing one leaves
  its mode alone; and a regenerated manifest takes the mode of the config
  beside it, since it is deleted and recreated within the keygen and has no
  previous mode of its own. So a `params/` shared with another service
  account keeps working. Nothing here is secret — a verifying key is
  published — and this is worth stating because the obvious implementation
  of an atomic write silently re-modes every artifact to `0600`.

  **Ownership is not preserved, and cannot be.** A rename publishes a new
  inode owned by whoever wrote it, and a non-root process cannot chown it
  back. If any file under `--params-dir` is owned by a different account —
  seeded by a provisioning script, or by root — the first regeneration
  after this release moves it to the account that runs keygen. Readers are
  unaffected as long as they relied on the mode rather than on ownership;
  if something relied on ownership, `chown` it back after the first run.

  The manifest is fail-closed in both directions: a manifest carrying a
  field this build does not know, or a `manifest_format` it does not write,
  is refused rather than partly believed. A build that reads a manifest it
  does not understand treats the cache as cold and regenerates — which
  means **two different builds must not share one `params_dir`**, or each
  will keep rebuilding over the other's keys. Point a new build at its own
  directory until every consumer is upgraded.

  **Required, before the first run on each host:**

  ```bash
  # ~4.3 GB free on the --params-dir filesystem, and it must be writable.
  # (The pk cache lives inside params/ in the shipped profile, so the two
  # requirements share one filesystem and add up — see below.)
  #
  # Run from crates/ackinacki-bridge/.
  # One subshell, so a failed check exits IT and not the shell you pasted
  # into. `exit 1` at the top level of an interactive session closes the
  # session — a rough way to learn your config is wrong.
  (
    set -e
    # Resolve BRIDGE_PARAMS_DIR the way the CLI does — `shell env > profile`.
    # `dotenvy::from_path` does NOT overwrite what the shell already set
    # (`ackinacki-bridge/src/main.rs:33-37`; the overriding variant is
    # `from_path_override`, which it does not use), so a plain
    # `. "$BRIDGE_CONFIG"` inverts the precedence and lets the profile beat an
    # explicit export. Harmless for a `df`; for `--repair` it means clearing
    # the keys in the profile's directory while you were pointing at another.
    #
    # `--params-dir` is NOT covered here and cannot be: it belongs to a run
    # that has not happened yet. If you intend to pass it, pass the same path
    # to these commands.
    #
    # `printenv`, NOT `${BRIDGE_PARAMS_DIR+x}`. The shell's `+x` test is true
    # for a variable that was assigned but never exported, and clap reads
    # `std::env::var` — the process ENVIRONMENT, which a bare
    # `BRIDGE_PARAMS_DIR=/foo` at your prompt does not enter. So `+x` here
    # takes /foo while the CLI, seeing nothing, falls back to the profile:
    # the exact inversion this block exists to prevent, and on the `--repair`
    # copy below it clears a directory the next run will not even open.
    # (Verified: `sh -c 'echo ${BRIDGE_PARAMS_DIR+set}'` prints nothing after
    # a non-exported assignment; `printenv` agrees with the child.)
    #
    # `printenv NAME` exits 0 for an exported-but-EMPTY variable and prints a
    # blank line, which is the distinction that matters. Empty is never a
    # directory: `params_dir: Option<PathBuf>` with `env =` (`args.rs:210`)
    # makes clap refuse an exported-empty value at parse time — "a value is
    # required for '--params-dir <PARAMS_DIR>' but none was supplied" — which
    # is a different message from the missing-plumbing refusal an UNSET
    # variable gets. Rejecting it here fails in the same place the run would,
    # instead of falling back to the profile and silently naming a different
    # directory than the run uses.
    if BRIDGE_PARAMS_DIR=$(printenv BRIDGE_PARAMS_DIR); then
      # In the environment. Reject empty rather than guess.
      [ -n "$BRIDGE_PARAMS_DIR" ] ||
        { echo "BRIDGE_PARAMS_DIR is exported but empty — unset it or give it a path" >&2; exit 1; }
    else
      # `printenv` here too, and for the same reason: `main.rs:38` reads
      # `std::env::var("BRIDGE_CONFIG")`, so a shell variable that was never
      # exported does not reach it. `${BRIDGE_CONFIG:?}` accepts one, and
      # then this block sources a profile the CLI will not load at all —
      # the run falls through to the compiled default while `--repair`
      # deletes keys somewhere else entirely. Fixing only BRIDGE_PARAMS_DIR
      # left exactly half of that hole open.
      BRIDGE_CONFIG=$(printenv BRIDGE_CONFIG) && [ -n "$BRIDGE_CONFIG" ] ||
        { echo "BRIDGE_CONFIG is not set to a path in the environment. A plain" >&2
          echo "  BRIDGE_CONFIG=./config/bridge_config" >&2
          echo "at your prompt is a shell variable, not an environment one, and the" >&2
          echo "CLI reads the environment (main.rs:38) — so it would load no profile" >&2
          echo "at all and fall back to its compiled default. Use \`export\`, or" >&2
          echo "pass --params-dir explicitly and give this command the same path." >&2
          exit 1; }
      # Refuse the constructs where `.` and dotenvy disagree, rather than
      # silently resolving to whichever one this shell happens to produce.
      # `dotenvy` parses KEY=value; `.` EXECUTES the file, so `$(...)`,
      # backticks and `$VAR` expand here and do not there (dotenvy would hand
      # the CLI the literal characters). The shipped profile is plain
      # assignments, so this fires only on a hand-edited one.
      grep -E '^[[:space:]]*(export[[:space:]]+)?BRIDGE_PARAMS_DIR=' "$BRIDGE_CONFIG" |
        grep -q '[$`]' &&
        { echo "BRIDGE_PARAMS_DIR in $BRIDGE_CONFIG uses \$ or backticks; the CLI's" >&2
          echo "dotenvy parser and this shell would not agree on its value." >&2
          echo "Pass --params-dir explicitly, and give these commands the same path." >&2
          exit 1; }
      # Subshell, so nothing else from the profile leaks into this one.
      BRIDGE_PARAMS_DIR=$( set -a; . "$BRIDGE_CONFIG"; printf '%s' "${BRIDGE_PARAMS_DIR-}" )
      [ -n "$BRIDGE_PARAMS_DIR" ] ||
        { echo "BRIDGE_PARAMS_DIR is absent from $BRIDGE_CONFIG" >&2; exit 1; }
    fi
    # Say which directory this resolved to. Every command below acts on it,
    # and one of them deletes files.
    echo "BRIDGE_PARAMS_DIR -> $BRIDGE_PARAMS_DIR" >&2
    df -h "$BRIDGE_PARAMS_DIR"
  )
  ```

  A host whose `params/` is read-only or short on space now **refuses in
  stage 1** instead of failing after the burn. That is the improvement, and
  it also means an upgrade can turn a previously-working host into one that
  refuses until it is given room.

  **`bridge-verifier-daemon` must not be restarted until this has been
  done.** It reads the verifying key and never generates one: against a
  cache with no manifest it exits with "event VK not found … run the event
  prover (Circuit 4) first". Order the upgrade as regenerate → restart, not
  the other way round. The CLI is unaffected — it regenerates on its own —
  and the bundle relayer is unaffected because Circuit 4 is not its
  circuit.

  **The keygen itself still runs after the burn**, in stage 5, exactly as
  before — this release does not move it, it makes stage 1 refuse when it
  would fail. To do the regeneration up front instead — which is also how
  you prepare the daemon — run the event prover directly. It takes no `--params-dir` and reads `./params` relative to the
  working directory (`bridge-event-halo2-prover/src/main.rs:38,151`), so
  point that at your params dir:

  ```bash
  # Run from the REPO ROOT; the subshell cds from there. The work has to
  # happen in crates/ackinacki-bridge/ because BRIDGE_PARAMS_DIR in the
  # shipped profile is `../bridge-prover-libraries/params`
  # (bridge_config.shellnet:65), relative to THAT directory — from the
  # repo root the same string resolves outside the repository and
  # realpath fails. (The instruction used to say "run from
  # crates/ackinacki-bridge/" and then cd into it, which is a cd that
  # fails whenever the instruction is followed.)
  (
    # The whole block is one subshell, so the EXIT trap dies with it. A
    # bare `trap … EXIT` pasted into an interactive shell survives until
    # that shell exits and expands $WORK only when it fires — so a WORK
    # reused later for something else gets rm -rf'd on logout.
    set -e
    # INSIDE the subshell, and after `set -e`. Outside it the cd is
    # unguarded: run this from anywhere but the repo root and it prints
    # one line to stderr, everything after it proceeds in whatever
    # directory you were standing in, and it also leaves your interactive
    # shell somewhere you did not ask to be.
    cd crates/ackinacki-bridge
    # BRIDGE_PARAMS_DIR is not in the shell's environment unless you put it
    # there: the profile is read by the Rust CLI at startup, not by your
    # shell, and the README only asks you to export BRIDGE_CONFIG.
    #
    # Resolve BRIDGE_PARAMS_DIR the way the CLI does — `shell env > profile`.
    # `dotenvy::from_path` does NOT overwrite what the shell already set
    # (`ackinacki-bridge/src/main.rs:33-37`; the overriding variant is
    # `from_path_override`, which it does not use), so a plain
    # `. "$BRIDGE_CONFIG"` inverts the precedence and lets the profile beat an
    # explicit export. Harmless for a `df`; for `--repair` it means clearing
    # the keys in the profile's directory while you were pointing at another.
    #
    # `--params-dir` is NOT covered here and cannot be: it belongs to a run
    # that has not happened yet. If you intend to pass it, pass the same path
    # to these commands.
    #
    # `printenv`, NOT `${BRIDGE_PARAMS_DIR+x}`. The shell's `+x` test is true
    # for a variable that was assigned but never exported, and clap reads
    # `std::env::var` — the process ENVIRONMENT, which a bare
    # `BRIDGE_PARAMS_DIR=/foo` at your prompt does not enter. So `+x` here
    # takes /foo while the CLI, seeing nothing, falls back to the profile:
    # the exact inversion this block exists to prevent, and on the `--repair`
    # copy below it clears a directory the next run will not even open.
    # (Verified: `sh -c 'echo ${BRIDGE_PARAMS_DIR+set}'` prints nothing after
    # a non-exported assignment; `printenv` agrees with the child.)
    #
    # `printenv NAME` exits 0 for an exported-but-EMPTY variable and prints a
    # blank line, which is the distinction that matters. Empty is never a
    # directory: `params_dir: Option<PathBuf>` with `env =` (`args.rs:210`)
    # makes clap refuse an exported-empty value at parse time — "a value is
    # required for '--params-dir <PARAMS_DIR>' but none was supplied" — which
    # is a different message from the missing-plumbing refusal an UNSET
    # variable gets. Rejecting it here fails in the same place the run would,
    # instead of falling back to the profile and silently naming a different
    # directory than the run uses.
    if BRIDGE_PARAMS_DIR=$(printenv BRIDGE_PARAMS_DIR); then
      # In the environment. Reject empty rather than guess.
      [ -n "$BRIDGE_PARAMS_DIR" ] ||
        { echo "BRIDGE_PARAMS_DIR is exported but empty — unset it or give it a path" >&2; exit 1; }
    else
      # `printenv` here too, and for the same reason: `main.rs:38` reads
      # `std::env::var("BRIDGE_CONFIG")`, so a shell variable that was never
      # exported does not reach it. `${BRIDGE_CONFIG:?}` accepts one, and
      # then this block sources a profile the CLI will not load at all —
      # the run falls through to the compiled default while `--repair`
      # deletes keys somewhere else entirely. Fixing only BRIDGE_PARAMS_DIR
      # left exactly half of that hole open.
      BRIDGE_CONFIG=$(printenv BRIDGE_CONFIG) && [ -n "$BRIDGE_CONFIG" ] ||
        { echo "BRIDGE_CONFIG is not set to a path in the environment. A plain" >&2
          echo "  BRIDGE_CONFIG=./config/bridge_config" >&2
          echo "at your prompt is a shell variable, not an environment one, and the" >&2
          echo "CLI reads the environment (main.rs:38) — so it would load no profile" >&2
          echo "at all and fall back to its compiled default. Use \`export\`, or" >&2
          echo "pass --params-dir explicitly and give this command the same path." >&2
          exit 1; }
      # Refuse the constructs where `.` and dotenvy disagree, rather than
      # silently resolving to whichever one this shell happens to produce.
      # `dotenvy` parses KEY=value; `.` EXECUTES the file, so `$(...)`,
      # backticks and `$VAR` expand here and do not there (dotenvy would hand
      # the CLI the literal characters). The shipped profile is plain
      # assignments, so this fires only on a hand-edited one.
      grep -E '^[[:space:]]*(export[[:space:]]+)?BRIDGE_PARAMS_DIR=' "$BRIDGE_CONFIG" |
        grep -q '[$`]' &&
        { echo "BRIDGE_PARAMS_DIR in $BRIDGE_CONFIG uses \$ or backticks; the CLI's" >&2
          echo "dotenvy parser and this shell would not agree on its value." >&2
          echo "Pass --params-dir explicitly, and give these commands the same path." >&2
          exit 1; }
      # Subshell, so nothing else from the profile leaks into this one.
      BRIDGE_PARAMS_DIR=$( set -a; . "$BRIDGE_CONFIG"; printf '%s' "${BRIDGE_PARAMS_DIR-}" )
      [ -n "$BRIDGE_PARAMS_DIR" ] ||
        { echo "BRIDGE_PARAMS_DIR is absent from $BRIDGE_CONFIG" >&2; exit 1; }
    fi
    # Say which directory this resolved to. Every command below acts on it,
    # and one of them deletes files.
    echo "BRIDGE_PARAMS_DIR -> $BRIDGE_PARAMS_DIR" >&2

    MANIFEST=$(realpath ../bridge-prover-libraries/Cargo.toml)
    PARAMS=$(realpath "$BRIDGE_PARAMS_DIR")
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    ln -s "$PARAMS" "$WORK/params"
    # Both paths are absolute, so the cd cannot disturb them.
    cd "$WORK" && cargo run --release --manifest-path "$MANIFEST" \
      -p bridge-event-halo2-prover -- --selftest
  )
  ```

  `probe_event_keys --params-dir <dir>` (same `--manifest-path` prefix as
  below) reports what the CLI will decide without changing anything; it
  does not generate.

  It reports one of four states, and the first word of the output is the
  one to read:

  - `warm` — nothing to do.
  - `cold` — nothing to do either: the next run regenerates. A manifest
    that will not parse lands here, not in a refusal.
  - `corrupt` — a truncated, unreadable, or digest-mismatched key file.
    `--repair` clears it and the next run regenerates. This is the case
    below.
  - `blocked` — a directory is sitting where a key file belongs, usually a
    bind mount whose host path does not exist. `--repair` refuses this one
    without touching anything; remove the directory by hand and re-probe.

  For `corrupt`, clear it with the same tool:

  ```bash
  # From crates/ackinacki-bridge/, as everything else in this section is.
  # Without --manifest-path cargo picks up a different workspace here —
  # `ackinacki-bridge` is a symlink member of bridge-prover-libraries and
  # is not buildable from its own directory.
  # This one deletes files, so getting the directory right matters more
  # here than anywhere else.
  # One subshell, so a failed check exits IT and not the shell you pasted
  # into. `exit 1` at the top level of an interactive session closes the
  # session — a rough way to learn your config is wrong.
  (
    set -e
    # Resolve BRIDGE_PARAMS_DIR the way the CLI does — `shell env > profile`.
    # `dotenvy::from_path` does NOT overwrite what the shell already set
    # (`ackinacki-bridge/src/main.rs:33-37`; the overriding variant is
    # `from_path_override`, which it does not use), so a plain
    # `. "$BRIDGE_CONFIG"` inverts the precedence and lets the profile beat an
    # explicit export. Harmless for a `df`; for `--repair` it means clearing
    # the keys in the profile's directory while you were pointing at another.
    #
    # `--params-dir` is NOT covered here and cannot be: it belongs to a run
    # that has not happened yet. If you intend to pass it, pass the same path
    # to these commands.
    #
    # `printenv`, NOT `${BRIDGE_PARAMS_DIR+x}`. The shell's `+x` test is true
    # for a variable that was assigned but never exported, and clap reads
    # `std::env::var` — the process ENVIRONMENT, which a bare
    # `BRIDGE_PARAMS_DIR=/foo` at your prompt does not enter. So `+x` here
    # takes /foo while the CLI, seeing nothing, falls back to the profile:
    # the exact inversion this block exists to prevent, and on the `--repair`
    # copy below it clears a directory the next run will not even open.
    # (Verified: `sh -c 'echo ${BRIDGE_PARAMS_DIR+set}'` prints nothing after
    # a non-exported assignment; `printenv` agrees with the child.)
    #
    # `printenv NAME` exits 0 for an exported-but-EMPTY variable and prints a
    # blank line, which is the distinction that matters. Empty is never a
    # directory: `params_dir: Option<PathBuf>` with `env =` (`args.rs:210`)
    # makes clap refuse an exported-empty value at parse time — "a value is
    # required for '--params-dir <PARAMS_DIR>' but none was supplied" — which
    # is a different message from the missing-plumbing refusal an UNSET
    # variable gets. Rejecting it here fails in the same place the run would,
    # instead of falling back to the profile and silently naming a different
    # directory than the run uses.
    if BRIDGE_PARAMS_DIR=$(printenv BRIDGE_PARAMS_DIR); then
      # In the environment. Reject empty rather than guess.
      [ -n "$BRIDGE_PARAMS_DIR" ] ||
        { echo "BRIDGE_PARAMS_DIR is exported but empty — unset it or give it a path" >&2; exit 1; }
    else
      # `printenv` here too, and for the same reason: `main.rs:38` reads
      # `std::env::var("BRIDGE_CONFIG")`, so a shell variable that was never
      # exported does not reach it. `${BRIDGE_CONFIG:?}` accepts one, and
      # then this block sources a profile the CLI will not load at all —
      # the run falls through to the compiled default while `--repair`
      # deletes keys somewhere else entirely. Fixing only BRIDGE_PARAMS_DIR
      # left exactly half of that hole open.
      BRIDGE_CONFIG=$(printenv BRIDGE_CONFIG) && [ -n "$BRIDGE_CONFIG" ] ||
        { echo "BRIDGE_CONFIG is not set to a path in the environment. A plain" >&2
          echo "  BRIDGE_CONFIG=./config/bridge_config" >&2
          echo "at your prompt is a shell variable, not an environment one, and the" >&2
          echo "CLI reads the environment (main.rs:38) — so it would load no profile" >&2
          echo "at all and fall back to its compiled default. Use \`export\`, or" >&2
          echo "pass --params-dir explicitly and give this command the same path." >&2
          exit 1; }
      # Refuse the constructs where `.` and dotenvy disagree, rather than
      # silently resolving to whichever one this shell happens to produce.
      # `dotenvy` parses KEY=value; `.` EXECUTES the file, so `$(...)`,
      # backticks and `$VAR` expand here and do not there (dotenvy would hand
      # the CLI the literal characters). The shipped profile is plain
      # assignments, so this fires only on a hand-edited one.
      grep -E '^[[:space:]]*(export[[:space:]]+)?BRIDGE_PARAMS_DIR=' "$BRIDGE_CONFIG" |
        grep -q '[$`]' &&
        { echo "BRIDGE_PARAMS_DIR in $BRIDGE_CONFIG uses \$ or backticks; the CLI's" >&2
          echo "dotenvy parser and this shell would not agree on its value." >&2
          echo "Pass --params-dir explicitly, and give these commands the same path." >&2
          exit 1; }
      # Subshell, so nothing else from the profile leaks into this one.
      BRIDGE_PARAMS_DIR=$( set -a; . "$BRIDGE_CONFIG"; printf '%s' "${BRIDGE_PARAMS_DIR-}" )
      [ -n "$BRIDGE_PARAMS_DIR" ] ||
        { echo "BRIDGE_PARAMS_DIR is absent from $BRIDGE_CONFIG" >&2; exit 1; }
    fi
    # Say which directory this resolved to. Every command below acts on it,
    # and one of them deletes files.
    echo "BRIDGE_PARAMS_DIR -> $BRIDGE_PARAMS_DIR" >&2
    cargo run --release --manifest-path ../bridge-prover-libraries/Cargo.toml \
      -p bridge-prover-lib --bin probe_event_keys -- \
      --params-dir "$BRIDGE_PARAMS_DIR" --repair
  )
  ```

  Nothing needs to be deleted by hand; `--repair` removes the manifest
  first, so an interrupted clear cannot leave a cache that still looks
  trustworthy.

  **During a rollout, do not point two different builds at one
  `--params-dir`.** Nothing locks that directory, and a keygen from one
  build interleaving with a keygen from another can leave a cache that
  passes every check while holding the other build's keys. Give the new
  build its own directory until every consumer has been upgraded.
- **`--from-keys` must now be mode `0400`, not `0600`.** The CLI reads the
  multisig owner's key file and never writes it, so read-only-to-owner is
  the tightest mode that works, and it is now an exact requirement rather
  than a "no group or world bits" range. **Every existing key file needs
  one command**, because the previous documentation told operators to set
  `0600`:

  ```bash
  chmod 400 /path/to/owner.keys.json
  ```

  The refusal names the mode it found and the exact remedy, so a run that
  hits this is one copy-paste from working. `scripts/deploy_msig_and_mint.sh`
  now emits `0400` directly. Unchanged: idempotency state files under
  `--state-dir` stay `0600` — those the CLI does write.

### Added

- **New binary `ackinacki-bridge`** — end-user CLI for withdrawing
  USDC from an Acki Nacki multisig to an EVM recipient via the bridge.
  Third-party-operator-facing counterpart to `daemon-live`: the daemon
  owns bundle proving (running anywhere — not necessarily on the same
  host); this CLI owns per-withdrawal composition. Runs the six-stage
  pipeline (preflight → idempotency reserve → burn → capture (with
  resurrect + coverage-wait as stage 4b) → Circuit-4 SHPLONK proof →
  `withdrawByProof`). **No local `prover_state.json` is read** — the
  CLI's only view of prover state is the on-chain contract, so it works
  against any deploy the operator has RPC + `--bridge-address` for.

  Subcommand surface:
  ```
  ackinacki-bridge withdraw \
    --from <dapp_id>::<account_id> --from-keys /path/to/owner.keys.json \
    --to 0xRecipient --to-chain 11155111 --amount 1.000000
  ```
  Global flags: `--json` (one-line JSON on stdout, human logs on stderr),
  `--yes` (skip prompt), `--non-interactive` (refuse instead of
  prompting), `--dry-run` (**preflight-only** — validates flags, key
  file perms, single-custodian check, USDCBridge resolution, and the
  ECC[3] balance, then stops; does NOT compose the burn, produce the
  Circuit-4 proof, or run `dry_run_withdraw` on the EVM side),
  `--allow-retry` (blunt override of the duplicate-in-flight refusal;
  will be replaced by `--resume` in v2).

  Env vars consumed (each has a matching `--flag`): `BRIDGE_GQL_ENDPOINT`,
  `USDC_BRIDGE_ACCOUNT_ID` (shellnet default `1a1a…1a1a`),
  `RPC_URL`, `BRIDGE_ADDRESS`,
  `BURNER_PRIVATE_KEY` (signer for `withdrawByProof`; distinct from
  the AN multisig owner key), `BRIDGE_AGGREGATOR_DIR`,
  `BRIDGE_VERIFIERS_DIR`, `BRIDGE_PARAMS_DIR`, `BRIDGE_PK_CACHE_DIR`,
  and `BRIDGE_WITHDRAW_STATE_DIR` (new — per-withdrawal idempotency
  state table; defaults to `$CONFIG_DIR/withdraw-state/`).

  Exit codes distinguish "nothing broadcast" from "broadcast, outcome
  unknown" so operator scripts don't blind on a single non-zero:
  `0` success (or dry-run OK), `2` preflight refused,
  `3` duplicate in-flight refused, `10` AN burn broadcast but final
  outcome unknown (reconcile via GQL), `11` capture timeout,
  `12` proof failed, `13` `withdrawByProof` reverted / dry-run reverted.

  Idempotency: SHA-256 dedup key on `(from, to, to_chain, amount)`. State
  files under `$BRIDGE_WITHDRAW_STATE_DIR` hold only chain-observable
  identifiers (AN tx hash, `WithdrawalInitiated` msg id, block seq no,
  ETH tx hash) — never key material. `--from-keys` and
  `--eth-private-key` contents are never logged, printed, or persisted.

  Burn payload defaults to `bounce = true` so USDC returns to the source
  multisig on any bridge revert (the historical Python driver used
  `bounce = false`; the Rust CLI's default is the safer of the two).

- **Helper scripts under `crates/ackinacki-bridge/scripts/`**:
  `local_smoke.sh` (dry-run wrapper: full pipeline including
  `dry_run_withdraw`, no broadcast) and `live_smoke.sh` (real submit).
  Both source `$BRIDGE_CONFIG_DIR/env` (default `L1_config/env`) for
  daemon-shared plumbing and expect the caller to export the
  per-withdrawal identity vars: `WITHDRAW_FROM`, `WITHDRAW_FROM_KEYS`,
  `WITHDRAW_TO`, `WITHDRAW_TO_CHAIN`, `WITHDRAW_AMOUNT`. Both emit
  per-mode absolute `--snark-dir` so the aggregate-proof subprocess
  finds the intermediate `.snark` file. Multisig deploy and EVM-side
  USDC treasury seeding are intentionally not scripted here — the
  vendored Python driver and `cast` remain the source of truth for
  those one-time setup steps.

- **`crates/ackinacki-bridge/scripts/deploy_msig_and_mint.{sh,py}`**
  — one-shot helper that deploys a fresh single-custodian
  `UpdateCustodianMultisigWallet` on the target AN cluster (default
  MODE=shellnet) and seeds it with 1 USDC on ECC[3] via
  `USDCBridge.mintAndSend`. Reuses `python/helper/msig.py::deploy_multisig`
  (same code path the full-orchestrator drivers use); stops at the
  ECC[3] mint — does NOT fire `initiateWithdrawal`, does NOT run any
  Rust binary. Emits eval-able `export WITHDRAW_FROM=…` and `export
  WITHDRAW_FROM_KEYS=…` lines on stdout (everything else on stderr),
  so the caller can `eval "$(scripts/deploy_msig_and_mint.sh)"` and
  proceed directly to `scripts/local_smoke.sh`. Fills the previous
  gap where the CLI's scripts dir assumed the multisig was already
  deployed by hand. Env overrides: `MODE`, `NETWORK`, `GRAPHQL_URL`,
  `WORK_DIR`, `USDC_BRIDGE_KEY_PATH`.

- Production-oriented Docker Compose kit for the shellnet → Sepolia L2
  relayer under `crates/bridge-relayer-daemon/deploy/shellnet-l2/`. It includes
  a non-root read-only runtime image, external secret env template, bind-mount
  layout, full artifact/on-chain preflight, parameter finalization and an
  operator status command. The service uses `restart: unless-stopped` for
  host/Docker recovery and reruns the fail-closed preflight on every start.
- The `bridge-evm-aggregator` lockfile is now tracked so target-host and image
  builds can use `cargo build --locked` reproducibly.
- New environment variables consumed by `bridge_prover_lib::paths`:
  `BRIDGE_CONFIG_DIR` (broad selector — resolves both state and proofs
  under `$BRIDGE_CONFIG_DIR/`), `BRIDGE_STATE_DIR` and `BRIDGE_PROOFS_DIR`
  (narrower overrides, win over `BRIDGE_CONFIG_DIR` when set).
  Operators must `export BRIDGE_CONFIG_DIR=./L1_config` (or
  `./L2_config`) **before** sourcing the per-mode env file; the env
  files no longer set `BRIDGE_CONFIG_DIR` themselves.
- New launch scripts under `crates/bridge-prover-libraries/scripts/` that
  source the per-mode env file: `launch_withdraw_e2e.sh` (dry-run L1),
  `launch_withdraw_e2e_real.sh` (real submit L1),
  `launch_withdraw_e2e_l2.sh` (dry-run L2), `replay_withdraw_shplonk.sh`
  (offline replay against a retained enriched witness). All emit
  per-mode absolute snark-dir paths so the aggregate-proof subprocess
  finds the intermediate `.snark` file.


- `scripts/check_bridge_abi_in_sync.sh` — `cmp`-based guard that the
  two runtime `USDCBridge.abi.json` copies stay byte-identical.
- `crates/ackinacki-bridge/scripts/check_fixture_prereqs.sh` — run it
  before `scripts/deploy_msig_and_mint.sh`. It refuses if `tvm-cli` is
  absent from `PATH` (and `CLI_NAME` unset) or if the `tvm-cli` it finds
  cannot execute on this platform — the usual cause being a binary built
  for another architecture, which otherwise surfaces as an opaque failure
  part-way through the deploy. Honours `CLI_NAME` to point at a specific
  binary; also runnable in CI.
- **`lint:rust:ackinacki-bridge:{fmt,clippy}`.** The relayer has had both
  since it landed; this crate had neither, so 118 formatting diffs and
  four clippy warnings were invisible on every MR. Both are
  `allow_failure: false`, and both are green — added last in the series
  that fixed what they check, because a gate nobody can pass is a gate
  people learn to ignore. The clippy job uses `--all-targets` so the new
  `tests/cli.rs` is linted too.

- **The key-cache CI job runs the keygen-lock and sweep tests, and
  asserts how many tests its filters select.** Those seven live in
  `keys::state::tests::` and `keys::common::tests::`, which none of the
  four existing substring filters matched, so they ran only in the
  nightly job — while guarding a second keygen writing over the first and
  `--repair` deleting a live keygen's 2.65 GB temp file. A libtest filter
  matching NOTHING still exits 0, so a renamed test silently stops
  running and the job stays green; the job now counts what the filters
  select and fails if that number moves.

- **Four CI jobs for the `ackinacki-bridge` crate.** The crate lives in
  the `bridge-prover-libraries` sub-workspace, so none of the existing
  `*:rust:*` jobs reached it and every one of its tests was unrun in CI.

  | Job | Runs on | What it covers |
  |-----|---------|----------------|
  | `build:rust:ackinacki-bridge` | every pipeline | `cargo build --locked -p ackinacki-bridge --all-targets` |
  | `test:rust:ackinacki-bridge` | every pipeline | the crate's own suite, plus the `bridge-prover-lib` key-cache and ceremony probes that guard a post-burn failure and need no ceremony on disk |
  | `test:rust:ackinacki-bridge:enospc` | opt-in (see below) | the reserve-under-ENOSPC test, against a real 1 MiB tmpfs |
  | `test:rust:ackinacki-bridge:keycache` | scheduled, or manual on an MR | the fixture-dependent key-cache and alternate-keyset tests — a ~464 MB Hermez ceremony and two ~7 min keygens, cached under `bridge-keycache-v1` |

  The last two are deliberately not per-MR gates: the keycache job takes
  ~20 minutes, and the ENOSPC job needs `CAP_SYS_ADMIN` to mount its
  tmpfs. **This leaves a real gap between merge time and the nightly
  schedule**; it is named here rather than papered over.

- **Two CI variables**, both set in the project's CI settings, not in
  `.gitlab-ci.yml`:
  - `BRIDGE_ENOSPC_RUNNER` — set to `1` once a runner carrying the
    `privileged` tag exists. Until then `test:rust:ackinacki-bridge:enospc`
    does not run at all. The variable is the switch and the tag is not,
    because a job whose tag no runner carries does not fail — it sits
    `pending` and the scheduled pipeline never finishes.
  - `BRIDGE_ALT_KEYS_REF` — a commit that contains
    `crates/bridge-prover-libraries` with a *different* event keyset. When
    unset, `test:rust:ackinacki-bridge:keycache` skips its three
    alternate-keyset regressions (including the only coverage of
    `bridge-verifier-daemon`'s `vk_opt()` consumer) and says so. When set
    to a ref that does not contain that path, the job fails rather than
    passing empty.

  Two further variables are consumed by the tests themselves and set by
  the jobs, not by an operator: `BRIDGE_TEST_FULL_FS` (the tmpfs mount
  point) and `BRIDGE_TEST_EVENT_KEYS` / `BRIDGE_TEST_EVENT_KEYS_ALT` (the
  two provisioned keysets).
- **README documents the one-time KZG ceremony provisioning** (`Step 0`).
  `crates/bridge-prover-libraries/params/` is gitignored, and no
  operator-facing document previously said how to create it — the only
  description lived in the prover sub-workspace's `TECHNICAL_README.md`. A
  withdrawal needs exactly one file, `kzg_bn254_21.srs` (~256 MB); lower
  degrees are derived from it automatically. The `~17 GB` figure in the
  flags table referred to a `params/` shared with a bundle relayer and has
  been corrected — a withdraw-only machine needs roughly 3 GB (ceremony +
  `event_pk.bin`) plus the aggregator's `pk_cache/`. The advanced runbook's
  disk figures were corrected to match, and "params missing" was dropped
  from its post-burn prover-failure triggers: preflight now catches that
  before anything is broadcast.

### Changed

- **`ackinacki-bridge withdraw` invocation reduced to five per-request
  flags; all network endpoints/plumbing resolved from `$BRIDGE_CONFIG`
  profile file.**
  The withdraw call is now:
  ```
  ackinacki-bridge withdraw \
      --from <dapp_id::account_id> --from-keys <path> \
      --to <0x…> --to-chain <chain-id> --amount <usdc> \
      [--dry-run] [--yes] [--json]
  ```
  Everything else (`--rpc-url`, `--bridge-address`, `--gql-endpoint`,
  `--anchor-layer`, `--i-know-the-wait`, `--params-dir`,
  `--aggregator-dir`, `--verifiers-dir`, `--work-dir`, `--snark-dir`,
  `--pk-cache-dir`, `--state-dir`, `--usdc-bridge-account`) is picked
  up from the profile file pointed to by `$BRIDGE_CONFIG` — the CLI
  auto-sources it at startup via `dotenvy` before clap reads any
  `env=` attr. Precedence: **explicit `--flag` > shell env > profile
  file > compiled default**. Existing `--flag`-heavy invocations
  continue to work.
    - `config/bridge_config` is now a **symlink** to the new
      `config/bridge_config.shellnet` (identical content to the
      pre-split file). Two sibling profiles ship alongside:
      `bridge_config.local` (local docker-compose devnet) and
      `bridge_config.mainnet` (commented-out placeholder for the
      future mainnet deploy). Switching network is a one-liner:
      `export BRIDGE_CONFIG=./config/bridge_config.local`.
    - Four flags gained `env=` attrs so they can live in the profile
      file instead of the CLI invocation: `BRIDGE_ANCHOR_LAYER`,
      `BRIDGE_I_KNOW_THE_WAIT` (accepts `true`/`false`/`1`/`0`),
      `BRIDGE_WORK_DIR`, `BRIDGE_SNARK_DIR`.
    - The hardcoded shellnet default on `--usdc-bridge-account`
      (`1a1a…1a1a`) is removed from `src/args.rs`; the value now
      comes from `USDC_BRIDGE_ACCOUNT_ID` in each profile (so a
      wrong-network profile fails loudly with a clap "missing
      required argument" instead of silently talking to the shellnet
      canonical account).
    - `scripts/local_smoke.sh` and `scripts/live_smoke.sh` drop
      ~90 lines each — they now just export `$BRIDGE_CONFIG`,
      canonicalize `$BRIDGE_SNARK_DIR` to absolute (the aggregator
      subprocess CWD-changes), and pass only the 5 intent flags.
      The old `BRIDGE_CONFIG_DIR` env-var fallback for relayer-style
      `L{1,2}_config/env` sourcing is removed — self-deploy users
      should either point `$BRIDGE_CONFIG` at their local profile or
      shadow `BRIDGE_ADDRESS` in the shell.
    - `scripts/deploy_msig_and_mint.py` drops its `MODE=shellnet|local`
      branch and reads `NETWORK` / `BRIDGE_GQL_ENDPOINT` /
      `USDC_BRIDGE_KEY_PATH` from the same profile file. Local
      detection is now derived from the resolved `NETWORK` URL
      (`127.*` / `localhost`), not a mode flag. Adding a new network
      is a single new profile file — no Python edit.
    - New dep: `dotenvy = "0.15"` in `crates/ackinacki-bridge/Cargo.toml`.

- **`ackinacki-bridge` docs split into a default-user runbook (README)
  and an advanced self-deploy runbook.**
  The previous single `docs/live_cli_withdraw_runbook.md` was one long
  file covering both audiences (default users hitting the pinned
  shellnet deploy AND advanced users deploying their own bridge +
  relayer). It is now split so each audience gets a doc scoped to
  their path:
    - `crates/ackinacki-bridge/README.md` — default-user runbook.
      Wallet setup → `scripts/deploy_msig_and_mint.sh` (fresh
      single-custodian AN multisig + 1 USDC seed) → treasury check
      → dry-run → real submit, with the full exemplary `cargo run`
      command inlining every real value (RPC/GQL URLs, pinned
      `BRIDGE_ADDRESS 0x0F4F…fc7`, relative paths for
      `--params-dir`, `--aggregator-dir`, `--verifiers-dir`).
      Includes simplified timing model (L2 only), exit codes,
      per-scenario error summaries, idempotency semantics, safety,
      file layout.
    - `crates/ackinacki-bridge/docs/live_cli_withdraw_runbook.md` →
      renamed to `docs/advanced_user_withdraw_runbook.md`. Scope
      narrowed to what advanced users need beyond the README: full
      L1 vs L2 timing model, self-deploy sequence
      (Steps L0–L5: `compute_bridge_anchors` →
      `deploy_bridge_bundle.sh` → treasury seed → cold-start
      daemon → wait for first bundle → CLI), stress-test loop,
      and the deep failure-mode catalog (revert-selector table,
      cast-trace decoding, USDCBridge keypair-drift diagnostic).
      Cross-references the README for the CLI invocation shape
      instead of duplicating it.
    - Internal references in `crates/ackinacki-bridge/config/bridge_config`
      updated to point at the new file names.


- **`ackinacki-bridge/config/bridge_config` pinned `BRIDGE_ADDRESS`
  rotated to the newly team-deployed L2 shellnet bridge
  `0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7`** (was
  `0x8D9190666128ab897C5ABd8C107239A197e08467`). The new deploy's
  `usdc()` points at a **real Circle FiatToken**
  (`0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`), not the previous
  bridge's mint-anyone test token (`0x94a9D9…5e4C8`) — so Case 1a
  Step 2 (treasury seeding) can no longer use the
  `0xC959…3f42D` faucet's `mint(address,address,uint256)`. Confirm
  the correct token with `cast call $BRIDGE_ADDRESS 'usdc()(address)'`
  and use whatever balance the burner already holds on that token, or
  ask a Circle-token minter for more. First successful E2E withdraw
  against the new deploy: AN burn
  `0xf2c407d2476803970f0ef68c4d3f8e888f9d0b34e23e532210d9b2426f498769`
  → ETH tx
  `0x7ccefdae4cca5812d339c969e69ab4242f7c4d6cf6c503265b658201fe30bf1b`
  (0.9 USDC, ~25 min wall time — the 23 min tail was remote-relayer
  latency waiting for the covering L2 boundary at seq 12,812,288).

- **`ackinacki-bridge/scripts/local_smoke.sh` and `live_smoke.sh`
  now source the standalone `config/bridge_config` by default** instead
  of the relayer's `L1_config/env`. Aligns the scripts with the
  runbook (§"Quick resume checklist") — the CLI is a standalone tool
  with its own single config, so its smoke wrappers should not depend
  on a co-located relayer deploy. Escape hatch: set
  `BRIDGE_CONFIG_DIR=/path/to/L{1,2}_config` to fall back to the
  relayer-style env. Both scripts now `cd` to
  `crates/ackinacki-bridge/` (was `bridge/`), so relative
  `BRIDGE_PARAMS_DIR=../bridge-prover-libraries/params` in `bridge_config`
  resolves correctly. Both scripts also now require
  `BURNER_PRIVATE_KEY` in caller env (mirrors the deliberate omission
  in `bridge_config` — see runbook §"Wallet setup"). `live_smoke.sh`
  additionally passes `--anchor-layer 2 --i-know-the-wait` by default
  to match the L2-anchored reference deploy pinned in `bridge_config`
  (`0x3fB082…8638`); override with `ANCHOR_LAYER=1` in caller env for
  advanced-user L1 deploys.

- **`ackinacki-bridge --allow-retry` now resumes in place
  instead of overwriting the state file.** v1 used to rewrite the prior
  record with a fresh `Reserved` on `--allow-retry`, dropping the
  stored `an_tx_hash`; the orchestrator then unconditionally re-fired
  `burn::fire`, causing a second multisig `sendTransaction` for the
  same withdrawal (a double-spend on the AN side, since the multisig
  has no EVM-style nonce guard). Post-fix, `--allow-retry` keeps the
  prior record verbatim and the orchestrator skips any stage whose
  outputs are already on file — burn is never re-broadcast once
  `an_tx_hash` is set. `Confirmed` and `Submitted` are terminal states
  that refuse `--allow-retry` outright (`Confirmed` already paid out;
  `Submitted` has an unresolved in-flight EVM tx that must be
  reconciled on-chain before any retry). `Failed` with an
  `an_tx_hash` on file resumes verbatim without `--allow-retry` (the
  only production writer of `Failed` is the post-burn
  `withdrawByProof` revert path, so the AN burn is already done —
  wiping would double-burn on retry). `Failed` without `an_tx_hash`
  (defensive branch for manual state edits) still triggers a
  clean-slate restart. See runbook §Idempotency semantics for the
  full state transition table.

- **`ackinacki-bridge` idempotency state file only transitions
  to `Submitted` after `withdrawByProof` returns a tx hash.**
  Previously the record was flipped to `Submitted` immediately before
  the `submit_withdraw` call, so an RPC error, wallet reject, or gas
  estimation failure would leave the on-disk record falsely claiming
  a tx was broadcast — subsequent runs would refuse-duplicate on
  `Submitted` even though nothing ever hit Sepolia. Post-fix,
  `Submitted → Confirmed` are both written only inside the `Paid`
  branch after the tx hash is in hand.

- **`ackinacki-bridge` prompts before the AN burn unless
  `--yes` is passed.** The `--yes` and `--non-interactive` flags,
  previously accepted by clap but never consulted, now gate a live
  stdin confirmation immediately before `burn::fire`. The prompt
  prints the source multisig, recipient + chain, USDC amount, target
  bridge, and anchor-mode wait estimate, then reads y/yes from stdin.
  Non-interactive stdin (no TTY) without `--yes` refuses with exit 2.
  `--non-interactive` without `--yes` continues to refuse upstream in
  `main::dispatch`.

- **`ackinacki-bridge` capture-stage errors now map to exit
  code 11 (`CaptureTimeout`) instead of 12 (`ProofFailed`).**
  `capture_targeted_withdrawal_event` failures — GQL unreachable,
  event never emitted, poll ceiling exceeded — are a
  reconcile-and-resume situation, not a Circuit-4 prover problem. The
  prior catch-all `ProofFailed` mapping misled operators into
  debugging the aggregator subprocess when the burn was in fact
  bounced or the AN GQL was down. The `wait_for_coverage` step (stage
  4b) still maps to `ProofFailed` because "waiting for the covering
  bundle" is a prove-path prerequisite.

- **`ackinacki-bridge --dry-run` docs corrected to
  preflight-only scope.** The README, runbook Step 4, runbook Case 8,
  and `scripts/local_smoke.sh` header previously claimed `--dry-run`
  ran the full pipeline up to and including `dry_run_withdraw`. The
  code (since the CLI landed) has always stopped after preflight,
  returning a stub `WithdrawSuccess`. Docs now match the code:
  argument shape, key perms, single-custodian check, USDCBridge
  resolution, ECC[3] balance — then stop. Exit codes 10–13 are
  unreachable under `--dry-run`.

- **`ackinacki-bridge` capture is now multi-user safe.**
  The CLI no longer youngest-picks a shared `USDCBridge → ExtOut` queue
  after firing its burn. Instead it chain-follows the multisig transaction
  hash (`an_tx_hash` returned by `sendTransaction`) through the two GraphQL
  hops the AN node exposes: `transaction(hash: an_tx_hash).out_messages`
  → pick outbound whose `dst == USDCBridge` → `message(hash:
  msig_out_msg_id).dst_transaction.out_messages` → pick outbound whose
  `dst == makeAddrExtern(618)` → the WithdrawalInitiated msg_id. The
  msg_id and its block_seq_no are persisted at the `Captured` idempotency
  stage. Concurrent operators can no longer capture each other's events
  even in the sub-second window between burns. New functions:
  `bridge_gql_fetcher::gql_client::{query_tx_out_messages,
  query_msg_dst_tx_out_messages, query_bridge_extout_by_id}` and
  `bridge_relayer_daemon::withdraw_e2e::capture_targeted_withdrawal_event`.
  The daemon's `run_once` still uses the baseline-snapshot path via
  `capture_next_withdrawal_event` — no behavior change there.

- `bridge-relayer-daemon` docs Case 1 (`live_relayer_bridge_verifyBlock_runbook.md`)
  is now a single unified cold-start section with a
  `BRIDGE_CONFIG_DIR=./L1_config` / `BRIDGE_CONFIG_DIR=./L2_config`
  selector at the top. Case 7 collapses to a table of
  the four operator-visible L2 deltas (W²-aligned bootstrap seqno, up
  to ~101 min first-verify wait, `layers=2` log field, ~15
  sub-bundle diagnostic window). `live_withdrawByProof_runbook.md`
  Case 8 was updated to source `L2_config/env` instead of
  `.env.shellnet.l2`.
- `bridge-prover-daemon` and `bridge-verifier-daemon` binaries no
  longer hardcode `./state/` and `proofs/` at compile time; they
  resolve paths at runtime via `bridge_prover_lib::paths`. Defaults
  match the pre-refactor literals (`./state`, `./proofs`,
  `./state/bootstrap_seed.json`), so existing operators who do not set
  `BRIDGE_CONFIG_DIR` see no behavioral change.

- Python E2E drivers deduplicated. `deploy_multisig` / `mint_usdc`
  moved to new `crates/bridge-prover-libraries/python/helper/msig.py`;
  `materialize_usdc_bridge_key_from_node_config` /
  `validate_usdc_bridge_key` moved to `helper/bridge_e2e.py`. Both
  `test_deploy_and_withdraw_only.py` and
  `generate_withdrawals_with_live_event_proving.py` now import the
  shared implementations. Fresher variant kept in the merge (explicit
  `RuntimeError` on multisig-materialization timeout, richer
  owner-key mismatch hints). Also fixes a stray syntax break in
  `GqlClient.fetch_bridge_extouts` introduced by an earlier
  docstring trim.

### Fixed

- **`ackinacki-bridge withdraw`: two concurrent runs of the same
  withdrawal no longer both burn.** The reservation answered the same way
  whether it had created the record or found somebody else's, so two runs
  with `--allow-retry` and no prior record both proceeded: the first won
  the create and entered the multi-second broadcast, the second read the
  first's record — no AN tx hash yet, because the first had not returned —
  and sent a second `initiateWithdrawal`. The multisig has no replay
  guard, so that is a second irreversible burn. **Finding a record that
  someone else created, with no hash on it, is now exit 3
  (duplicate-refused)**, and inspecting the record's fields is no longer
  how the two cases are told apart — the field that would distinguish
  them is written only after the send returns.

- **A ceremony file that is present but unreadable is reported as that,
  not as "no ceremony".** The halo2 readers panic on malformed input
  rather than returning an error, and the wrappers around them discarded
  the panic's message. It was not entirely lost — the default panic
  handler still printed it — but it appeared as a raw Rust panic naming a
  file inside a dependency, beside a calm refusal that did not mention
  it. The reason now travels with the error, so the refusal reads
  "…kzg_bn254_21.srs: SRS is truncated or malformed — the halo2 reader
  panicked: failed to fill whole buffer" instead of "no Hermez ceremony
  of degree >= 21", which sent operators to provision a file that was
  already there. Proving- and verifying-key reader panics are logged with
  the same detail beside their verdict.

  The same file is also no longer read twice. The lookup tried the exact
  degree and then re-scanned the directory including that same path, so
  one bad file cost two full passes — 256 MB at k=21 — and printed the
  identical panic twice for one problem.

- **Importing the Python helpers no longer runs `tvm-cli`.**
  `helper/common.py` resolved the binary at module import and then ran
  `tvm-cli version` through a shell, neither call with a timeout: a
  candidate on `PATH` that blocks (a wrapper reading stdin, a stale
  network mount) hung anything that merely imported the module.
  Resolution is now lazy, cached, bounded by a five-second timeout with
  stdin closed, and announces itself on stderr. `common.TVM_CLI` and
  `from helper.common import TVM_CLI` both keep working.

  That banner used to print to **stdout**, which broke
  `deploy_msig_and_mint.py`'s stated contract that its stdout holds
  nothing but two eval-able `export` lines — README Step 2 pipes it
  straight into `eval`, so `Checking cli specified in library: …` was
  being handed to the shell as a command. The script now redirects
  everything to stderr for the duration of the run and writes the two
  lines to the stdout it captured first, so a helper that prints cannot
  break it by accident.

- **The burn's outcome interpretation is covered by tests.** Everything
  `send` does after the SDK returns — whether the transaction says the
  burn happened, and what to call it — was reachable by no test: the only
  mention of `send` in the suite is a compile-only guard that is never
  called, and `process_message` needs a live node. That logic is now its
  own function, and its tests pin that a reverted transaction never
  becomes a receipt, that an unclassifiable one is exit 10, and that no
  refusal on that path can read as "nothing was sent" — the burn is on
  the wire by then, and a message suggesting otherwise sends an operator
  to re-run.

- **`scripts/deploy_msig_and_mint.py` quotes what it prints, and the
  Python helpers no longer `cd` through a shell.** README Step 2 is
  `eval "$(scripts/deploy_msig_and_mint.sh)"`, so every character the
  script writes to stdout becomes shell code in the operator's session —
  and neither printed value was a literal. `WITHDRAW_FROM_KEYS` derives
  from `BRIDGE_WORK_DIR`, so an ordinary path with a space produced a
  broken `export` and one containing `;` or `$(…)` executed;
  `WITHDRAW_FROM` comes back from `tvm-cli`'s JSON and was printed
  unchecked. The path is `shlex.quote`d now, and the address is refused
  unless it is `<64-hex>::<64-hex>` — emitting something the CLI would
  reject later is worse than failing before anything reaches a shell.

  Separately, `helper/common.py` built `cd {work_dir} && {command}` and
  ran it with `shell=True`, so **every** call through those two helpers
  broke on a work directory containing a space, and would have executed
  one containing shell metacharacters. They pass `cwd=` now, which hands
  the path to the kernel rather than to a parser. The command string
  itself is still shell-interpreted — the callers build `tvm-cli …`
  strings with interpolated paths, and converting those to argument lists
  is a larger change than this; that is the remaining exposure and it is
  now the only one.

- **`--json` errors now carry the cause chain.** The envelope rendered
  only the outermost `Display`, so everything a stage had wrapped — the
  aggregator's stderr, a `RelayerError`, the keygen-lock timeout naming
  the process to wait for — was printed in human mode and dropped
  entirely under `--json`. A script reading the machine output got
  `"withdraw-e2e pipeline failed"` and nothing about why.

  A `causes` array is added alongside `message`, outermost first, `[]`
  when the error has no source. **`message` is unchanged**: consumers
  already match on it, and folding the chain in would silently change
  what those patterns see. Both output modes now walk the chain through
  one function, so they cannot disagree about the cause of a failure.

  Sixteen error mappings also flattened their cause at construction
  (`anyhow!("{e}")` keeps a rendered string and no source); they now
  preserve it, so there is a chain for the envelope to carry. Three of
  those were already `anyhow::Error` values with `.context()` hops, where
  the flattening had been discarding the most.

- **`--dry-run` no longer needs `HOME`.** The refusal added for an unset
  `HOME` was raised before the check that skips idempotency state
  entirely, so under systemd, cron and most Docker images a dry run
  failed with a double-burn refusal about a directory it never touches —
  the one command whose purpose is to be safe to run anywhere. Resolved
  only when the run will actually use it.

- **Witness and proof files are named after the event again.** The seq_no
  stamped into `event_<seq>_witness.json` and `proof_event_<seq>.json` was
  hard-coded to `0`, so every withdrawal wrote `event_000000_witness.json`
  and `proof_event_000000.json`. Two withdrawals through one `--work-dir`
  overwrote each other's witness — which the runbook simultaneously told
  operators to keep, because regenerating it is expensive — and every
  documented `<seq>` path was wrong. It is the event's block seq_no now.
  The runbook and README also had the witness filename backwards
  (`witness_event_<seq>.json`); it is `event_<seq>_witness.json`.

- **A reservation that fails after publishing says so.** If the directory
  fsync failed after the record's name was already linked, the refusal
  still read "no partial record was left behind" — while a complete
  record sat on disk. An operator read that as "nothing is there" and the
  next run then refused with exit 3 about a record they had been told did
  not exist. The two cases now say different things, and the record is
  deliberately NOT removed in the second: it is complete and already
  visible to other processes, and unlinking a published reservation is
  the double-burn the file exists to prevent.

- **The exit-3 refusal now says what to do, and the CLI can tell you
  whether another run is executing the withdrawal.** A record that reads
  `reserved` with no `an_tx_hash` has two meanings and the file cannot
  separate them: a run is inside the burn right now and has not returned
  to write the hash, or a run died in that window. The refusal named
  neither, rendered the missing hash as `Prior AN tx: None` — which reads
  as "nothing was sent" — and advised "re-run with `--allow-retry` to
  override", which is the flag the operator had already passed to get
  there. The only escape left to find was deleting the record, i.e. the
  guard against a second burn.

  A withdrawal now holds an advisory `flock` on
  `<state-dir>/<key>.lock` from before the reservation until after the
  burn's hash is written. A later run reports which case it is: "another
  process on this host is executing this withdrawal RIGHT NOW" — wait —
  or "the record was left by a run that has already exited". The kernel
  releases the lock when the holder dies, so this is a fact rather than a
  guess about how old the record is. The refusal also names the record's
  path, states plainly that `--allow-retry` does not override it, and
  gives the two branches: hash found on chain → write it in and resume;
  nothing broadcast **and** no live holder → delete and re-run.

  A second run that arrives while the lock is held is refused before it
  reserves, so it no longer races the first through capture and submit.
  The lock is advisory and per-host — `flock` is unavailable on some
  network filesystems, and the refusal says so rather than claiming
  knowledge it does not have. The record's own cross-field guards are
  unchanged and remain what holds in that case.

- **Documentation: there is no age at which deleting a `Reserved` record
  is safe.** README and the runbook both said `Reserved` files over 24
  hours old with no `an_tx_hash` were safe to prune, "the burn never
  happened" — asserting as fact exactly the half the code says is
  unknowable, about the file that prevents a second burn. Both now state
  the two real conditions (on-chain reconciliation showing no
  `initiateWithdrawal`, and no process holding the withdrawal) and point
  at the exit-3 refusal that answers the second one.

  The runbook's exit-10/11 recovery is corrected to match: it said
  `--allow-retry` would compose a second burn on a hashless record (it
  now refuses with exit 3) and prescribed that same flag as the fix for
  "nothing was sent" (it is not). An operator who reconciles correctly
  now has a documented way forward for both outcomes. `--allow-retry`
  also cannot re-open a `Confirmed` record, which one remediation step
  suggested; changing the amount is the way to a fresh identity.

- **`ackinacki-bridge withdraw`: a failed state-file write after the burn
  no longer reports exit 2.** Exit 2 means "refused before sending,
  nothing left the machine". Four sites reported it after the burn was on
  the wire, and the last two after `withdrawByProof` had paid out — i.e.
  after USDC had moved on both chains. Those now report the exit code of
  the stage they are in, and name what has already happened.

- **`ackinacki-bridge withdraw` refuses when `HOME` is unset instead of
  putting its state directory in the current directory.** Under systemd,
  cron, `sudo` without `-H`, and many Docker images, the fallback made
  the double-burn guard depend on where the operator was standing: the
  same command from two directories found no prior record either time.
  Pass `--state-dir` (or `BRIDGE_WITHDRAW_STATE_DIR`) in those
  environments.

- **`ackinacki-bridge withdraw` refuses a state record that claims a burn
  it cannot name.** A record with no `an_tx_hash` whose status is any of
  `burned`, `captured`, `proved`, `submitted`, `failed` or `confirmed`, or
  one whose `key` disagrees with the filename it was read from, is now
  rejected on read. Every one of those statuses is written only after a
  burn was broadcast, so "no hash" and "past reserved" cannot both be
  true. All are reachable by hand, and the CLI's own post-burn recovery
  message asks operators to edit exactly those fields.

  `failed` is in that list, and was the one initially left out of it. The
  gap was not cosmetic: with `failed`/no-hash readable, the reservation
  wiped the record and reported the result as a NEW reservation — a claim
  that means "this run won the atomic publish", made about a plain
  `rename` that excludes nobody. Two runs could both wipe, both be told
  they had created it, and both burn. There is now exactly one place in
  the crate that can report a created reservation, and it is the
  `hard_link` that fails when somebody else won; a test asserts that
  count.

- **`ackinacki-bridge withdraw --from-keys` accepts a `0x`-prefixed or
  short keys.json.** Preflight normalised the file's halves while the
  signing path only lowercased them, so such a file passed preflight and
  then failed against the very key preflight had just approved — on every
  attempt, permanently, with the same command and the same file. Note
  that key files are parsed as hex only: an all-decimal-digit key is a
  hex key, not a decimal number.

- **`ackinacki-bridge withdraw` reports exit 10 rather than inventing a
  success.** Two ways the burn stage could mis-read the network's answer:
  a transaction carrying neither `aborted` nor `compute.exit_code` was
  read as "fine", and an empty transaction id was left-padded into
  `0x000…0` and reported as the burn's hash. The first wrote a durable
  `burned` status after a possibly-reverted call, from which no re-run
  can proceed without hand-editing; the second produced a hash that
  matched nothing in the event query, forever. Both are now exit 10 —
  "broadcast, outcome unknown, reconcile" — which is what they are. A
  short id is also lowercased now; the event query is byte-exact, and an
  upper-case one used to time out five minutes after the money moved.

- **The ceremony refusal says why.** `--params-dir` holding an SRS that
  is loadable but *not* the Hermez Perpetual Powers of Tau produced "no
  usable ceremony at k=N" plus instructions to provision one — for a file
  that was already there. The sentence that matters, that its toxic waste
  is public and every proof produced with it is forgeable, was the error's
  source and was never printed. Both the preflight refusal and the
  prover-side panic now print the full chain and say to delete the named
  file, which provisioning does not replace.

- **The burn confirmation prompt refuses instead of reading an answer
  nobody saw.** Every line of the prompt was written with the result
  discarded, while the `y` it then reads authorises an irreversible burn.
  With stderr unwritable — a closed pager, a full disk — the terminal sat
  with no prompt on it and whatever was typed counted as consent. A
  redirect to a file is unaffected; only a write that actually fails is
  refused.

- **A malformed argument no longer crashes the CLI.** The truncation
  applied to untrusted arguments before echoing them back split multibyte
  characters, so e.g. a `--to` with non-ASCII at the wrong offset exited
  101 with an unparseable message instead of the `--json` error envelope.
  Control characters in an argument are now escaped rather than replayed
  into the terminal.

- **Resuming a withdrawal no longer leaves a record no later run can
  act on.** The resume path — a prior record already carrying an AN tx
  hash — reserved the identity directly and discarded the reservation's
  answer, on the grounds that a resume broadcasts nothing. Two things
  followed that are not about broadcasting.

  It took no withdrawal lock, so a resuming run was invisible to the
  liveness check the exit-3 refusal promises, and two concurrent resumes
  both reached the submit stage — where the loser's `withdrawByProof`
  reverts on the nullifier and writes `Failed` over the winner's
  `Confirmed`, leaving the record claiming a paid-out withdrawal is
  resumable.

  And it ignored the reservation, so a record deleted between the initial
  read and the reservation — the deletion the exit-3 message itself
  prescribes once an operator has reconciled — left the run carrying on
  with the hash it had read earlier and never writing it back. The
  capture stage then persisted `captured` with no `an_tx_hash`, a
  combination the record guard refuses permanently as "acting on it would
  broadcast a SECOND burn": a withdrawal that completed, and a record
  nothing can resume. That case now restores the hash into the record it
  just claimed instead.

  The lock is also held for the rest of the run rather than only through
  the burn, which is what excludes the concurrent-resume case above.

- **Two keygens can no longer run over each other in one `params_dir`.**
  That directory is documented as shared with the bundle daemon, and
  nothing prevented both from generating keys at once. Per-file writes are
  atomic, so a file is never torn — but the manifest is written last and
  hashes whatever is on disk at that moment, so two processes building
  DIFFERENT circuits could publish a manifest that is internally
  consistent and describes a mixed keyset. Every later check then passes,
  the verdict is "warm", and the proof fails at stage 5, after the burn.
  (Two processes building the same circuit produce identical keys, so that
  case only wasted ~3 GB and ~7 minutes against a preflight that had
  reserved for one.)

  Keygen now takes an exclusive `flock` on `<prefix>_keygen.lock` for the
  whole write, per circuit, so unrelated circuits still run in parallel.
  A second arrival waits up to 30 minutes with progress logged, then
  refuses rather than blocking a withdrawal forever — and when it does get
  in, it re-checks the cache first and adopts what the other process
  published instead of regenerating it. `flock` rather than a marker file
  deliberately: the kernel releases it when the holder exits, so a keygen
  killed mid-write leaves nothing to clean up, and a lock that IS held
  proves a live holder. Never delete the lock file to "clear" it.

- **The proving key is re-verified at stage 5, before it is used.** Its
  ~2.65 GB digest was streamed exactly once, in preflight — the runtime
  gate only asks whether the file exists — and between those two points
  sit the irreversible burn and up to ~91 minutes of anchor wait. A key
  replaced in that window (an rsync, a restored backup, another keygen)
  was not caught: a truncated one reports, but a same-length corruption
  past the embedded verifying key deserialises happily and leaves the
  verifying key's digest unchanged. The result was a proof rejected on
  submit, after the money had moved. Stage 5 now re-streams the key and
  compares it against the digest preflight recorded — not against the
  manifest read again, so replacing key and manifest together does not
  pass. A mismatch is exit 12 with instructions that resume rather than
  re-burn.

- **An interrupted keygen no longer leaves 2.65 GB nobody can see.** Key
  files are published through a temp file that removes itself on drop —
  but not when the process is killed, and a proving-key write is ~2.65 GB
  spread over minutes. Ctrl-C, an OOM kill or a container stop anywhere in
  that window left the whole partial file behind as a `.tmpXXXXXX`: a
  dotfile, so `ls` did not show it, and nothing removed it, including
  `probe_event_keys --repair`. The space it held is exactly the headroom
  the withdraw preflight reserves for the next keygen, so the run that
  tripped over it was the run that had been told there was room — and it
  tripped at stage 5, after the burn.

  `probe_event_keys` now reports them on every invocation (whatever the
  cache verdict) and removes them under `--repair`; matching is
  `.tmp` + exactly six alphanumerics and regular files only, so a
  directory or symlink of that name is listed to you and never touched.

  **The sweep takes every circuit's keygen lock first and refuses if any
  is held**, naming the circuit. That name shape is also what a keygen
  writes its ~2.65 GB proving key into while the write is in progress, so
  a sweep run against a live keygen would unlink the file being written —
  the write survives, but the rename that publishes it fails, and the
  keygen dies at the end having spent its full seven minutes. Under the
  withdraw CLI that is stage 5, after the burn. It refuses rather than
  waits: `--repair` is run to free space, and blocking it behind a keygen
  that is itself consuming that space helps nobody. As a second guard for
  any future writer that does not take the lock, a file observed still
  changing is skipped rather than deleted.
  The withdraw preflight warns when it sees them, and its
  out-of-space refusal now names how much of the shortfall they are
  holding and the command that reclaims it — "free 4 GB" is the wrong
  instruction when 2.65 GB of it is a file the operator cannot see.

- **The halo2 circuit crates are pinned by revision, not `branch = "main"`.**
  `EVENT_CIRCUIT_REVISION` is bumped by hand and is the only thing between
  a moved Circuit 4 and a "warm" verdict over keys built for the old one:
  the manifest format, the revision and the vk/config digests all still
  match after the circuit moves, because the *files* did not change. Under
  a branch, one `cargo update` did that silently and the mismatch surfaced
  as a rejected proof at stage 5, after the burn and the anchor wait. All
  five crates from `acki-nacki-to-eth-bridge-halo2-circuits` now name
  revision `5356b178cce8ab5a283096032c774533bcab8e28` — the commit
  `Cargo.lock` already resolved, so nothing that gets built changes.
  Moving the circuit is now an edit reviewers see next to the revision
  constant. Two `cargo run` invocations in the key-cache CI job also
  gained `--locked`, which the rest of the pipeline already used.

  Not covered: `crates/bridge-snark-utils` declares the same crates at
  `branch = "main"`, has no committed lockfile at all, and carries a
  `[patch]` pointing at a sibling checkout. It is excluded from the
  workspace and built by no CI job; making it reproducible is a separate
  piece of work.

- **The CI alternate-keyset guard can fire.** `git ls-tree` ran after a
  `cd` into `crates/bridge-prover-libraries` and asked for that path
  again, so it always answered empty: setting `BRIDGE_ALT_KEYS_REF` failed
  the job every run while blaming the operator's ref, and the three
  alternate-keyset regressions never ran.

- **`aggregate-proof` subprocess now receives an absolute
  `--inner-snark` path.** `SubprocessAggregator::aggregate` spawns the
  aggregator with `current_dir = aggregator_dir`; a relative snark path
  (the `ackinacki-bridge` default `--snark-dir=./shplonk-snark`
  hits this) resolved against the wrong CWD and the subprocess exited
  with `No such file or directory (os error 2)`. Canonicalized in
  `aggregator.rs::SubprocessAggregator::aggregate` before argv
  construction; the snark file always exists at that point (the inner
  prover just wrote it), so the canonicalize is safe. First observed
  running `ackinacki-bridge withdraw` against shellnet from the
  crate root without an explicit `--snark-dir` override.

- **`ackinacki-bridge` error output now walks the anyhow
  `#[source]` chain.** `output.rs::print_error` used to print only the
  top-level `Display`, so any wrapped `CliError::ProofFailed { source }`
  (and any nested `anyhow::Context`) was invisible — the user saw
  `Circuit4ShplonkPipeline::prove failed` with no hint that the real
  cause was, e.g., the aggregator subprocess stderr. Human-mode errors
  now emit indented `caused by [N]:` lines beneath the top-level
  message. `--json` mode is unchanged.

- **`ackinacki-bridge withdraw` preflight USDCBridge liveness
  check no longer misreports an Active bridge as `acc_type is Unknown`.**
  The GraphQL query at `preflight.rs::query_usdc_bridge_state` asked
  for `info.acc_type` (a numeric enum: `1 = Active`) and then tried to
  match it as a string, silently falling back to `"Unknown"` and
  failing the `== "Active"` check on every well-deployed shellnet
  bridge. Fixed by requesting the string-typed `info.acc_type_name`
  alias instead (`"Active"`/`"Uninit"`/…) and matching that. First
  observed on shellnet's canonical zerostate USDCBridge
  (`0000…::1a1a…`), which is Active per tvm-cli but was reported
  Unknown by the CLI.

- **`ackinacki-bridge withdraw` preflight `getCustodians` call
  no longer rejects the very `--from` form the CLI itself mandates.**
  Args validation requires `--from dapp_id::account_id` (the tvm-cli v3
  extended form) and rejects the legacy `0:<acc>` shape, but
  `preflight.rs` was threading that same extended string into
  `tvm_sdk::abi::encode_message`'s `address` field, which the ABI
  encoder does not accept and which failed with `Invalid address
  [Invalid argument: 0]`. The encoder path now uses `from.legacy()`
  (`0:<acc>`); `.extended()` is still used for the `get_account` BOC
  fetch (which does accept it) and for user-facing log lines. First
  observed running `scripts/local_smoke.sh --dry-run` on shellnet
  against a freshly-deployed single-custodian multisig; before the fix,
  every dry-run and every live withdraw would fail at preflight step 3
  regardless of on-chain state.

- **`ackinacki-bridge` no longer double-burns ECC[3] when
  retrying after a `withdrawByProof` revert.** The `Failed` arm of
  `idempotency::reserve` used to wipe the prior record with a fresh
  `Reserved`, dropping the recorded `an_tx_hash`. Because `Status::Failed`
  is only written by the post-burn `withdrawByProof` revert path
  (`orchestrator.rs::WithdrawSubmitOutcome::Reverted`), the burn had
  already broadcast — but the orchestrator's stage-3 resume branch
  reads `record.an_tx_hash.is_some()`, so wiping caused it to fall into
  the else branch and call `burn::fire()` a second time. Concrete
  failure mode: operator hits a `WithdrawTreasuryShortfall` (Case 3e /
  exit 13), tops up treasury, re-runs the same command → second
  `initiateWithdrawal` on the AN side, second ECC[3] debit for the
  same withdrawal. Post-fix, `Failed` with `an_tx_hash` present
  returns the prior record verbatim (resume path skips
  `burn::fire()`); only `Failed` without `an_tx_hash` (defensive
  branch for manual state edits) still wipes. Regression test:
  `idempotency::tests::reserve_over_failed_with_an_tx_hash_preserves_prior_record`.
- **`ackinacki-bridge` now refuses `--to
  0x0000000000000000000000000000000000000000` at preflight** (Sergey
  review R7). The USDCBridge ERC-20 leg could otherwise succeed against
  a token whose `transfer` treats the zero address as a burn sink,
  permanently consuming an ECC[3] draw against an unrecoverable
  recipient. Refusal path: `parse_to` in `args.rs`, `ArgInvalid { flag:
  "to", .. }`, exit 2. Test: `args::tests::to_rejects_zero_address`.
- **`ackinacki-bridge` now caps `--amount` at `u64::MAX`
  micro-USDC at preflight** (Sergey review R8). The multisig ECC[3]
  balance and the AN-side `initiateWithdrawal(amount)` argument are u64
  on the wire; letting a larger value through caused a silent downstream
  cast in `burn::fire` at broadcast time rather than a clean preflight
  refusal. Boundary: exactly `u64::MAX` micros (== `18446744073709.551615`
  USDC) is still accepted. Refusal path: `parse_amount` in `args.rs`,
  `ArgInvalid { flag: "amount", .. }`, exit 2. Tests:
  `args::tests::amount_rejects_above_u64_max_micro`,
  `args::tests::amount_accepts_u64_max_micro_exact`.
- **On-AN ABI artifacts realigned to shellnet (`acki-nacki@cf664666b`).**
  Dropped stale `anWorkchain int8` from `confirmDeposit` inputs and the
  `DepositFinalized` event in both runtime copies
  (`crates/bridge-prover-libraries/python/contracts/USDCBridge.abi.json` and
  `crates/ackinacki-bridge/abi/USDCBridge.abi.json`); rewrote
  `DepositVoucher.abi.json` constructor to the 5-arg
  `(depositId, contractAddr, dappId, amount, anAccount)` schema.
  Withdraw runtime paths (`initiateWithdrawal`, `mintAndSend`,
  `finalizeDeposit`) were already correct — no calldata change.
- `scripts/check_voucher_abi_consistency.py` default `--compiled` now
  points at `crates/bridge-prover-libraries/python/contracts/` (was a
  nonexistent path).


- GraphQL BK-update range queries now cap their open-ended upper bound at the
  schema's signed 64-bit `Int` maximum instead of serializing `u64::MAX`, which
  live GraphQL servers reject during integer coercion.
- L2 warm-resume startup now compares the immutable genesis anchor at
  `anchor_level - 1`; a valid level-2 state no longer fails drift validation
  after a clean restart.
- The live relayer runbook now provisions the required K=22 SRS, documents
  runtime `solc 0.8.19`, isolates the relayer cursor per mode, treats deploys
  as irreversible broadcasts, keeps production secrets outside Git and
  reflects the prover's sequential-stage but multi-core execution model. The
  deployment helper no longer writes a private key into tracked config and
  archives previous prover state instead of deleting it; an explicit
  `CONFIRM_NEW_BRIDGE_DEPLOY=DEPLOY_NEW_CONTRACTS` gate is now required before
  any broadcast.
- **`ackinacki-bridge withdraw --dry-run` no longer requires
  `BURNER_PRIVATE_KEY` or the prover directories.** `--eth-private-key`,
  `--aggregator-dir`, `--verifiers-dir`, `--params-dir` and `--work-dir`
  are now optional at parse time and resolved only for a real withdrawal.
  A real run missing any of them refuses at stage 1 naming all of them at
  once, instead of clap listing nine flags before any check runs.
- **The EVM side is checked before the AN burn — on every run, including
  `--dry-run`.** None of these need a signing key, so all of them run in
  stage 1 whether or not the run will submit:
  - an `RPC_URL` whose `eth_chainId` is not `--to-chain`;
  - a `--bridge-address` with no contract behind it;
  - a withdrawal verifier stack that is unset or incomplete — `adapter →
    shplonkVerifier → yulVerifier`, code required at every level, and the
    deployed Yul runtime byte-compared against the local
    `BridgeWithdrawalAggregatorVerifier.bin`. Same walk as
    `deploy/shellnet-l2/scripts/preflight.sh`. The bytecode comparison
    needs `--verifiers-dir`, which is submit-only; pass it to a dry run and
    the dry run performs it too, otherwise it is skipped with a warning;
  - a bridge pinned to different `(bridgeWithdrawalDappFr,
    bridgeWithdrawalAccFr)` values than this withdrawal will prove;
  - a `treasuryBalance` that already cannot cover the amount.

  Previously the first contract call happened in stage 4b — after the
  irreversible burn and up to ~101 min of anchor wait. The verifier and
  identity checks read `immutable` storage, so what the preflight sees is
  what `withdrawByProof` will see. The treasury check is a preflight, not a
  guarantee: the treasury is shared and can be drained again while a
  withdrawal waits for its anchor bundle.
- **The signing key and the prover artifacts are checked in stage 1 too,
  on real runs.** These need the submit-only flags, so they are gated on
  them and a `--dry-run` does not reach them: parsing
  `BURNER_PRIVATE_KEY`, the KZG ceremony at **both** degrees a withdrawal
  loads (k=20 and k=21 — checking only the larger missed a bad
  `kzg_bn254_20.srs`, which `load_srs` prefers by exact filename over any
  larger ceremony), the Circuit-4 key cache, a runnable `aggregate-proof`,
  and writable output directories with room for what will be written.
  Previously the burner key was parsed in stage 6 and nothing opened
  `--params-dir` before stage 5.

  So a clean `--dry-run` means "nothing about either chain is
  misconfigured" — not "a real run will succeed". `--dry-run`'s `--help`
  now says exactly that, and no longer claims to compose the messages,
  which it never did.
- **The Circuit-4 key cache is validated, not just counted.** Stage 1 now
  asks the key manager what it will do with `--params-dir` instead of
  testing that `event_pk.bin` exists. A proving key with no matching
  config or verifying key means keygen will run, so the ~3 GB headroom
  check applies; a truncated or empty proving key beside a valid verifying
  key is refused outright, because the manager would skip keygen and then
  fail loading it at proof time. Both previously surfaced in stage 5, after
  the burn. A warm cache is not asked to keep ~3 GB free permanently.

  Also refused in stage 1: a directory at one of the four key paths — a
  bind mount whose host path does not exist is the usual cause. Keygen
  replaces those names by rename and cannot write through a directory, so
  what used to be a stage-5 `EISDIR` after the burn is now an exit-2
  refusal naming the path. `probe_event_keys` reports it as `blocked` and
  `--repair` refuses without touching anything: remove the directory by
  hand. (Symlinks, FIFOs and sockets at those paths are *not* refused —
  rename replaces them.)

  A real run now makes two extra passes over the ~2.65 GB proving key
  during preflight — one to verify its digest, one to confirm this build
  can still deserialise it — on top of the pass stage 5 already made. Tens
  of seconds before the burn, on a cold page cache. `--dry-run` makes none
  of them: these checks need the submit-only flags and a dry run has none,
  so a clean dry run says nothing about the key cache.
- **Burn and signing failures no longer echo the key file.** The SDK's
  signing errors embed the public key verbatim and the first eight
  characters of the secret; both reached stderr and `--json` through the
  refusal's message and its `source`. Refusals on these paths now carry the
  SDK error code and nothing else. Reconciliation is unaffected — it was
  always the on-chain walk, not the message.
- **The Circuit-4 proving and verifying keys are checked as a pair.**
  Two keysets from different circuit revisions each load cleanly, and the
  proof is made with the proving key's embedded verifying key while
  self-verification uses `event_vk.bin`. Mismatched, the proof was produced
  and then failed its own verification in stage 5, after the burn. Stage 1
  now compares the two.
- **The idempotency record is durable before the burn goes out.** The
  reservation is now `fsync`ed — the record, the state directory, and the
  parent of every directory level the run creates — before
  `sendTransaction`, and a reservation that fails to write removes its own
  partial file. Previously it was written and left in the page cache, so a
  power loss between the reservation and the burn could lose the record
  while the burn landed, and the next run would burn again.
- **A withdrawal refused before broadcast leaves no idempotency record.**
  Declining the confirmation prompt (or running without a TTY) used to
  write a `reserved` state file that made the next identical invocation
  fail with exit 3 and "duplicate in-flight withdrawal … Prior AN tx:
  None". The confirmation and every fallible pre-send step (reading
  `--from-keys`, encoding the payload) now run *before* the reservation,
  which is taken immediately before the message goes on the wire.
  A `reserved` record is still a hard block without `--allow-retry`: it is
  also the state a burn that reached the wire and errored leaves behind, so
  it must not be cleared automatically.
- **`--from-keys` refusals name the actual problem.** A missing file and a
  directory were both reported as "not owner-only readable; run: chmod
  600 <path>". The key file's two halves are now verified to be an actual
  key pair during preflight, rather than the `secret` field first being
  read at burn time — a mismatched pair was previously reported as exit 10
  ("burn: … reconcile via GraphQL before retrying") even though the SDK had
  rejected it locally and nothing was ever broadcast.
- **`--json` covers usage and configuration errors.** A malformed
  invocation or an unloadable `$BRIDGE_CONFIG` bypassed the machine-refusal
  contract entirely: clap printed its own human usage block, and the config
  path used an undocumented exit 1, leaving a `--json` consumer with nothing
  on stdout. Both now emit the one-line `{"error":{…}}` envelope and exit 2
  like every other pre-send refusal. `--help` and `--version` still print
  normally and exit 0.
- **`--yes` and `--non-interactive` can be passed together.** clap
  rejected the combination outright, which is the normal shape for a CI
  wrapper.
- **`scripts/deploy_msig_and_mint.sh` works on Linux and emits a 0400 keys
  file.** The committed `python/bin/tvm-cli` was a macOS-arm64 binary that
  PATH injection made win over a working system install ("Exec format
  error"); it is no longer tracked, and tvm-cli is now discovered by
  trying candidates until one answers `version` (`CLI_NAME` still
  overrides). The emitted multisig keys file is now mode 0400, which the
  very next documented step requires.
- **Idempotency state files are created 0600**, and stay 0600 across
  updates — the atomic writer previously renamed a tmp file created at the
  ambient umask over the record. A state directory the CLI creates itself
  is 0700; one the operator supplies keeps the mode they gave it.
- **`--from-keys` refusals name the mode they found.** The old message
  said only "is not owner-only readable", so an operator could not tell a
  wrong mode from a wrong owner or a missing file.
- **A missing or non-Hermez KZG ceremony is refused before the burn.**
  Nothing before stage 5 opened `$BRIDGE_PARAMS_DIR`, so an unprovisioned
  `params/` cost an irreversible AN burn plus up to ~91 min of anchor wait
  before failing with exit 12. A real run now resolves the ceremony at
  stage 1 using the same code path the prover uses (so a truncated file is
  caught, not just an absent one), compares
  `BridgeWithdrawalAggregatorVerifier.bin` byte-for-byte against the
  verifier this build embeds, requires a prebuilt `aggregate-proof` that
  answers `--help` (the `cargo run` fallback is no longer accepted for a
  real withdrawal — a cold build cannot be verified inside a preflight),
  proves `--work-dir` / `--snark-dir` / `--pk-cache-dir` / `--params-dir`
  accept writes, and prints the exact provisioning commands on failure.
    - New flag `--allow-verifier-drift` for self-deploy operators whose
      `--verifiers-dir` legitimately holds a regenerated verifier.
    - `aggregate-proof` gains `--help` / `-h`, which it previously rejected
      as an unknown argument.
- **The missing-SRS message names a tool that can actually provision it.**
  It used to point at `scripts/bootstrap_hermez_srs.sh`, which writes K=20
  into `crates/bridge-snark-utils/params/` — wrong degree, wrong directory,
  so following it verbatim failed identically on the next attempt. Both the
  new preflight refusal and the last-resort panic in `load_srs` now name the
  `bootstrap_hermez_srs` bin and the `powersOfTau28_hez_final_21.ptau`
  download.

### Removed

- **`relayer daemon-prover` subcommand deleted.** The legacy file-based
  standalone verifyBlock daemon (reads `proof_<seqno>.json` bundles
  from `PROVER_PROOFS_DIR`, submits `verifyBlock`) is superseded by
  `daemon-bridge` (file-based, both legs on one EOA) and `daemon-live`
  (in-process, GraphQL-driven, bundle-only). No active systemd unit,
  runbook, CI job, or E2E test invoked `daemon-prover` — the last
  reference was an "example manual/one-off invocation" in
  `AGENTS.md` which is also removed. Operators who need the standalone
  verifyBlock leg can still use `submit-verify-block` (one-shot) or
  `daemon-bridge` (long-running).
- `scripts/ursus/USDCBridge.abi.json` and
  `crates/bridge-prover-libraries/python/contracts/README.md` — unreferenced
  ABI mirror and its documentation. Systemd/env templates under
  `scripts/ursus/` retained.
- Fossil `.tvc` files under `python/contracts/`
  (`USDCBridge.tvc`, `DepositVoucher.tvc`); nothing loaded them.
- `crates/bridge-prover-libraries/python/bin/tvm-cli` and
  `crates/ackinacki-bridge/tvm-cli.conf.json` are no longer tracked;
  both are now gitignored. Supply `tvm-cli` on `PATH` or via `CLI_NAME`.

## [0.1.0] – 2026-06-11

Tagged at `0f7c635`. Changes up to this tag predate this changelog and are not
recorded here; use `git log` for that history.
