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
    # NOTE: everything from here to the `BRIDGE_PARAMS_DIR ->` echo is
    # repeated verbatim in the two other upgrade blocks in this release.
    # Each block is pasted on its own, so it carries its own copy — but a
    # correction to one of them belongs in all three. The last one was a
    # source citation that was wrong in each.
    #
    # Resolve BRIDGE_PARAMS_DIR the way the CLI does — `shell env > profile`.
    # `dotenvy::from_path` does NOT overwrite what the shell already set
    # (`dotenvy::from_path` in `ackinacki-bridge/src/main.rs`; the overriding variant is
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
    # directory: `params_dir: Option<PathBuf>` with `env =` (`args.rs`)
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
      # `printenv` here too, and for the same reason: `main.rs` reads
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
          echo "CLI reads the process environment — so it would load no profile" >&2
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
    # NOTE: everything from here to the `BRIDGE_PARAMS_DIR ->` echo is
    # repeated verbatim in the two other upgrade blocks in this release.
    # Each block is pasted on its own, so it carries its own copy — but a
    # correction to one of them belongs in all three. The last one was a
    # source citation that was wrong in each.
    #
    # Resolve BRIDGE_PARAMS_DIR the way the CLI does — `shell env > profile`.
    # `dotenvy::from_path` does NOT overwrite what the shell already set
    # (`dotenvy::from_path` in `ackinacki-bridge/src/main.rs`; the overriding variant is
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
    # directory: `params_dir: Option<PathBuf>` with `env =` (`args.rs`)
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
      # `printenv` here too, and for the same reason: `main.rs` reads
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
          echo "CLI reads the process environment — so it would load no profile" >&2
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
    # NOTE: everything from here to the `BRIDGE_PARAMS_DIR ->` echo is
    # repeated verbatim in the two other upgrade blocks in this release.
    # Each block is pasted on its own, so it carries its own copy — but a
    # correction to one of them belongs in all three. The last one was a
    # source citation that was wrong in each.
    #
    # Resolve BRIDGE_PARAMS_DIR the way the CLI does — `shell env > profile`.
    # `dotenvy::from_path` does NOT overwrite what the shell already set
    # (`dotenvy::from_path` in `ackinacki-bridge/src/main.rs`; the overriding variant is
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
    # directory: `params_dir: Option<PathBuf>` with `env =` (`args.rs`)
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
      # `printenv` here too, and for the same reason: `main.rs` reads
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
          echo "CLI reads the process environment — so it would load no profile" >&2
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

### Changed

- **The shellnet profile documents how to tell a live deploy from a dead
  one, because an address alone does not.** The values are unchanged —
  `BRIDGE_ADDRESS=0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7`,
  `USDC_BRIDGE_ACCOUNT_ID=1a1a…1a1a` — but NODE-4011 moved the relayer
  to a second deploy
  (`0x8545129b215B248944A3aE40f711F34CAb458644`, over a new AN-side
  eccUSDCBridge) and rolled it back within the day, and the retired
  deploy answers every getter exactly like the live one throughout.

  What that costs if you get it wrong is the whole point: a bridge
  nobody advances accepts the burn on the AN side and then never
  produces a covering bundle, so the run waits out `COVERAGE_WAIT` and
  exits 11 or 12 with the USDC gone. The profile now carries the check —
  `storedLastSeenBlockSeqNo` plus the `BlockVerified` cadence, which is
  ~437 Sepolia blocks (~87 min) in L2 mode — and says plainly that a
  last event older than that means this is not the live deploy.

  Two invariants are written down beside it, both learned the expensive
  way during the switch:

  - `BRIDGE_ADDRESS` and `USDC_BRIDGE_ACCOUNT_ID` move **together**. A
    deploy is pinned at construction to one AN-side account, and
    `withdrawByProof` compares the `(dappFr, accFr)` a proof carries
    against that pinning *before* verifying the proof — a half-updated
    profile is a refusal after the burn, which is why preflight checks
    the pair up front. The id never has to be looked up: `cast call
    $BRIDGE_ADDRESS 'bridgeWithdrawalAccFr()(uint256)'` printed as
    `064x` **is** this line.
  - A deploy carries its **own** treasury, and switching does not bring
    the balance along. `treasuryBalance` is a counter on the bridge
    (`AckiNackiBridge.sol:98`), not an address, and only `deposit` moves
    it — a plain USDC `transfer` funds nothing, leaves the tokens as
    skimmable liquid surplus, and the withdrawal still reverts with
    `WithdrawTreasuryShortfall`.

  Nothing has to be re-provisioned on the prover side for either deploy:
  both verifier stacks end in a Yul runtime matching this build's
  embedded `BridgeWithdrawalAggregatorVerifier.bin` byte for byte
  (20 958 bytes, sha256 `406d4054…`), at different addresses.

### Fixed

- **The pinned-identity refusal printed `(dappFr, accFr)` in decimal and
  then asked you to compare it with a hex value.** The last line of the
  refusal sends you to `USDC_BRIDGE_ACCOUNT_ID`, which every profile
  writes as 64 lowercase hex characters — but the pair above it arrived
  as `U256` decimal, so the check it prescribed could not be done by eye:

  ```
  on chain (dappFr, accFr) = (0, 11806252235961651298089590628336806290921645495320214372650577192963691649562)
  ```

  Both pairs are now `{:064x}`, in the profile's own shape, so the
  comparison is a character-for-character one:

  ```
  on chain (dappFr, accFr) = (0000…0000, 1a1a1a1a…1a1a)
  ```

  Found on a live shellnet run of `withdraw --dry-run
  --usdc-bridge-account 2b2b…2b2b`, where the same run's
  `preflight ok usdc_bridge=…` line had already printed the pair as hex.
  Nothing else changes: the refusal's wording, its exit code (2) and the
  `--json` envelope's `stage`/`exit_code` are untouched — only the
  rendering of the four numbers inside `message`.

- **Every `cast logs` command in the withdraw docs was unrunnable, and
  each failed silently.** Found by running them: an empty result reads
  as "nothing happened on chain", which is the opposite of what the
  operator needs during a recovery.

  - `--from-block latest-2000` (README health check, runbook Case 2 and
    the diagnostics dump) — `cast` takes a height or one of
    `earliest|finalized|safe|latest|pending`, never arithmetic:
    `invalid digit found in string`. `--from-block -1000` (runbook
    Case 2) is parsed as a flag: `unexpected argument '-1' found`. Both
    now compute the height from `cast block-number`.
  - `grep -c BlockVerified` — `cast logs` prints `address`, `blockHash`,
    `blockNumber`, `data`, `logIndex`, `topics`, `transactionHash` and
    **no event name**, so this counted a string that is never there and
    answered `0` for a healthy bundle daemon. Counts `blockNumber` lines
    now.
  - **`WithdrawalExecuted(uint256,address,uint256,uint256)` is not an
    event this bridge emits.** `AckiNackiBridge.sol` declares
    `WithdrawalByProofExecuted(uint256 indexed nullifier, address indexed
    recipient, uint256 amount, uint256 indexed tokenId, address
    submitter)` — a different name and five parameters. The query
    returned nothing after a *successful* payout, in the two runbook
    places an operator reaches while reconciling one.

  The bundle-daemon health check also gets a criterion that matches what
  it guards: freshness rather than a count. Bundles land ~437 Sepolia
  blocks apart (~87 min) in L2 mode and `COVERAGE_WAIT` is 120 min, so a
  daemon one cadence behind the tip consumes the whole stage-4b budget
  *after* the burn. The check now reads the last event's age and pairs it
  with a GraphQL query for the chain's own `seq_no`, which is what
  separates a stalled relayer from an idle chain.

- **The withdraw log never prints `layer_idx=`, which both documents told
  operators to grep for** to confirm L2 anchoring. The enricher logs
  `resolved anchor: L2` and `anchor_layer=L2` (1-indexed, with an `L`),
  the orchestrator logs `anchor_stride=16384`; `layer_idx` is a field of
  the witness JSON and is 0-indexed, so the same fact reads as `1` there.
  The docs conflated the two, and the grep they published matches nothing
  on any run.

- **README's dedup-key formula did not describe the key.** It promised
  "SHA-256 of `{from}|{to}|{to_chain}|{amount}` (all ASCII)". Only
  `from` is ASCII: `idempotency.rs::key` hashes the extended `from`,
  then the recipient as **20 raw bytes**, the chain id as a big-endian
  `u64` and the amount as a big-endian `u128`. The record's filename is
  that digest, and Case 3a asks the operator to compute it — so the
  published formula sent them to the wrong file. Two golden vectors are
  now in the README beside it.

- **`ackinacki-bridge withdraw --dry-run` reads the idempotency store
  before it reports a refusal.** It still reserves nothing and writes
  nothing — exit 3 remains unreachable under it — but it used not to
  LOOK, so all eight refusals it can raise came out as exit 2, whose
  published contract is "nothing broadcast, **and** no record for this
  identity on disk". A dry run over a `burned` record with a hash on
  file said exactly that about a withdrawal whose burn is on the wire.

  A dry run that finds a record now reports its refusals as **exit 10**,
  names the record, and forbids deleting it. It skips the ECC[3]
  balance check over a recorded burn for the same reason a real run
  does: its job is to say what a real run would do.

  That job is now done to the end. A dry run over a record that is
  **terminal** (`confirmed`, `submitted`), one whose burn is in flight
  without `--allow-retry`, or one carrying **no AN tx hash** is refused
  with **exit 3** and the real run's own remedy — where it used to
  report `DryRunOk` and exit 0, because the whole reserve/resume
  machinery sat behind `if !dry_run`. `--dry-run` therefore produces 0,
  2, 3 or 10 rather than 0 or 2. It still reserves nothing and writes
  nothing.

  Both runs now raise that refusal before preflight rather than after
  it, so a withdrawal that has already paid out is refused without
  needing the node, the burner key or the proving ceremony.

  With `HOME` unset and no `--state-dir` a dry run still runs — it is
  meant to be safe to run anywhere — but a refusal it raises there is
  **exit 10**, not 2: it never opened a state directory, so it cannot
  say the withdrawal is untouched. A real run is refused outright in
  that state, as before.

- **`ackinacki-bridge withdraw`: a missing `--eth-private-key`,
  `--work-dir`, `--params-dir`, `--aggregator-dir` or `--verifiers-dir`
  is exit 10, not exit 2, when a record for the withdrawal is on disk.**
  The submit-only plumbing was resolved as the very first fallible thing
  the run did — above the read of the state record — so its refusal was
  a bare exit 2, whose published contract is "nothing broadcast, **and**
  no record for this identity on disk". The run had not opened the state
  directory when it said that.

  The state that makes it cost money is the ordinary one: a record with
  `an_tx_hash: null` — a burn broadcast whose outcome was never observed
  — and a re-run that drops one environment variable. `BURNER_PRIVATE_KEY`
  unset, `BRIDGE_WORK_DIR` unset, a `$BRIDGE_CONFIG` that did not get
  sourced. Supply the five and the same run answers **exit 3**: it
  reaches the reservation, where a record with no hash is a duplicate in
  flight. Without them it answered **2** and never mentioned the record,
  and a wrapper keying on the exit code read the withdrawal as
  untouched. The check now runs after the record is read and answers
  **10**, naming the record and forbidding the deletion, while still
  naming the flag that is missing — so every route through that state
  now tells you the withdrawal is not untouched.

  One ordering change comes with it, visible if you are missing both:
  `HOME`/`--state-dir` is now reported before the plumbing, because the
  record has to be read before the plumbing refusal can describe it.

- **`ackinacki-bridge withdraw`: an unset `HOME` with no `--state-dir` is
  exit 10, not exit 2.** Same class, and it cannot be fixed by
  reordering: with no state directory there is nowhere to read a record
  from, so the run cannot say the withdrawal is untouched — an earlier
  run with `HOME` set, or with `--state-dir`, may have recorded a burn
  for the same identity, which is exactly what a wrapper that drops the
  variable on a retry produces. Nothing is broadcast either way and the
  remedy is unchanged (pass `--state-dir` or set
  `BRIDGE_WITHDRAW_STATE_DIR`); the refusal now says why it is not exit
  2. **Exit 10 therefore covers five situations, not four** — the fifth
  is "this run had nowhere to look" — and the README table and runbook
  §3d-ii list it.

- **`--anchor-layer` above 2 is refused instead of accepted and then
  read three different ways.** `--anchor-layer 3` parsed, and the three
  places that consumed it disagreed: the coverage wait used the **L1**
  stride, the resurrected `BridgeState` was stamped with anchor level
  **3**, and the confirmation prompt printed "unbounded (no shipped
  relayer for L≥3)". The run therefore burned, then waited against a
  stride that does not correspond to the level it recorded — and no
  relayer advances an anchor above layer 2, so that wait never ends.
  `auto`, `1` and `2` are the accepted values, and the refusal comes
  before the prompt and before anything is broadcast. Its exit code
  depends on what is on disk, like every other stage-1 refusal: **2**
  with no record for this identity, **10** with one — the value is
  parsed after the record is read for exactly that reason.

- **A broken invariant after the burn is a refusal, not a panic.** The
  submit plumbing was unwrapped with `expect` three hundred lines below
  where it is established, and past `burn::send`. Exit 101 is not one of
  this CLI's exit codes — no consumer maps it and `--json` emits no
  envelope for it — so a future edit breaking that invariant would have
  aborted with nothing parseable, about a withdrawal whose USDC had
  already left the multisig. It is exit 12 now, the code the stage
  already uses for internal invariants downstream of the send.

- **`peek` stops reporting an unreadable state directory as an empty
  one.** It asked `Path::exists()`, which answers `bool` and folds every
  `stat` failure into `false`. A state directory this process cannot
  traverse — wrong mode, wrong owner, a half-restored backup — therefore
  read as "no record for this withdrawal", and the run reserved, found
  nothing, and broadcast a second `initiateWithdrawal` for an identity
  whose record was in that directory. Only `NotFound` means there is no
  record now; anything else is a refusal that says the CHECK failed.

- **Four refusals that happen with a record on disk stop reporting exit
  2.** Exit 2's published contract has two halves — nothing was
  broadcast, and there is no record for this identity — and these
  satisfied only the first:

  - the read of a competing record after `hard_link` answered EEXIST,
    where the file exists by construction;
  - the reservation whose record was published and whose directory entry
    could not be made durable, in a message that says "the record IS on
    disk and complete" itself;
  - `BurnPermit::issue`, which runs after `reserve` has published and
    whose message says "A reservation for this identity IS on disk";
  - the read taken behind a contended lock, in the previous entry.

  All four are exit 10 now. Nothing was broadcast by the run in any of
  them, and exit 10 does not claim otherwise — since the re-badge gate
  became record existence, it means "this run must not act as if the
  withdrawal were untouched". Scripts keying on 2 to mean "clean slate,
  safe to retry" would have retried into a live reservation.

- **A `BRIDGE_CONFIG` that is not valid UTF-8 is refused instead of
  silently ignored.** `std::env::var` answers three ways and the profile
  loader read the third as the first: a variable whose bytes are not
  UTF-8 was treated exactly like an unset one, and the profile was never
  sourced. Nothing said so, because this runs before tracing is
  initialised.

  On the withdraw path that is a route to a second `initiateWithdrawal`,
  not a configuration annoyance. `BRIDGE_WITHDRAW_STATE_DIR` comes from
  the profile; unsourced, the run falls back to the default state
  directory. The idempotency key is a hash of
  `(from, to, to_chain, amount)` and does not include the directory, so
  the record, the reservation and the lock file for the withdrawal in
  flight are all in the directory nobody is reading: the run finds no
  prior record and burns again. The multisig has no replay guard.

  It now exits 2 — nothing has been broadcast and no state directory has
  been resolved at that point — and the message says what being ignored
  would have cost. The value is rendered lossily and escaped before it
  reaches a terminal.

- **`bridge-prover-lib`'s test suite stops reporting a moving number.**
  Four tests in `paths::tests` mutate the process-wide environment
  (`BRIDGE_CONFIG_DIR` / `BRIDGE_STATE_DIR` / `BRIDGE_PROOFS_DIR`) behind
  a guard that saved and restored but did not serialise them, while the
  harness ran them in parallel threads of one process. Measured on
  `cargo test -p bridge-prover-lib --lib paths::tests`: 7 failures in 40
  runs before, 0 in 40 after.

  What to expect when you run it: **`108 passed; 2 failed; 16 ignored`**,
  and the two are always
  `keys::common::tests::load_srs_downsizes_{from_parent_ceremony_when_exact_missing,misnamed_larger_file}`,
  which need a `params/kzg_bn254_17.srs` this repository does not ship.
  Any other number is a real regression — before this change the line
  moved between 106/4, 107/3 and 108/2 on its own, which is how one gets
  waved through as "the usual two".

  Serialising the four writers was not the whole race: a fifth test,
  `ipc::tests::bkupd_paths_match_pattern`, asserts the literal
  `"proofs/bkupd_000042.json"` and resolves it through the same
  `BRIDGE_PROOFS_DIR` the others set — a reader of what they write, in
  the same process, and it failed with
  `left: "/tmp/override_proofs/bkupd_000042.json"`. It takes the same
  lock now.

  Neither the flake nor the two fixture failures were ever visible in CI:
  every `-p bridge-prover-lib` invocation in `.gitlab-ci.yml` is filtered
  to `keys::`, and no fmt or clippy job covers the crate at all. That gap
  is tracked in
  [NODE-4015](https://linear.app/acki-nacki/issue/NODE-4015/bridge-prover-lib-krejt-ne-pokryt-ni-odnim-gejtom-ci)
  and is not addressed here. Until its fmt step lands, do not run
  `cargo fmt` against this crate — it is 347 hunks from rustfmt's output
  at HEAD, and a sweep would bury unrelated diffs.

- **CLI README and advanced runbook: exit 10 no longer tells you to
  deploy a fresh multisig.** Its remediation for a key mismatch ended
  "re-run `scripts/deploy_msig_and_mint.sh` … and start over". That
  script deploys a NEW multisig — a different `--from`, so a different
  dedup identity — which orphans the record for the burn already on the
  wire: nothing will ever resume it. Both documents now say to correct
  the key file and re-run the same withdrawal command, which resumes
  from the recorded burn, and say plainly that the deploy script is for
  standing up a new test withdrawal rather than recovering one.

  The exit-code table also still described exit 10 as "AN burn WAS
  broadcast", which is one of its three populations. It now names what
  the three share, which is what a script should key on: do not treat
  this identity as untouched. Exit 2's row gains the half that actually
  distinguishes it — **no record on disk** — since the same preflight
  failure on a withdrawal that has one is exit 10, with the same checks
  and the same message.

- **`ackinacki-bridge withdraw`: a preflight refusal on a withdrawal
  whose burn is already recorded now exits 10, not 2.** Six checks in
  stage 1 — balance, destination chain, bridge deploy, signer key, prover
  artifacts, client context — reported exit 2 whether or not a prior run
  had already broadcast for this identity. Exit 2's published contract is
  "nothing broadcast, no state file written". The first half survives a
  stage-1 refusal; the second does not, and it is the half an operator
  acts on: the runbook attaches "do not delete the state file" to exit 10
  while telling an exit-2 reader no state file exists.

  The refusal now names the AN transaction already on the wire, says
  nothing new was sent or written, and says not to delete the record.
  Fix what preflight named and re-run with `--allow-retry`: the flag is
  what lets the reservation hand the recorded burn back, and without it
  that record is refused with exit 3. A first run's preflight refusal is
  unchanged — with no prior
  burn, exit 2 is exactly right.

  **A record with no AN tx hash moves too, and that is the case this
  branch's own failure mode produces.** If the burn is broadcast and the
  CLI cannot observe the outcome, it returns before a receipt, so the
  hash is never written. The next run sees `an_tx_hash: null`, treats
  the burn as not-sent, requires the ECC[3] balance — which is genuinely
  spent — and refuses. The missing hash hid the burn, caused the
  refusal, and used to suppress the reclassification, all three: exit 2,
  with a record on disk and a burn possibly on chain.

  The gate is now the existence of a record, not the hash on it. What
  the refusal says differs: with a hash, re-running resumes from the
  recorded burn; without one it does not — resume is gated on the hash —
  and it does not reach exit 3 by being re-run either, because stage 1
  goes first and a record showing no burn puts the ECC[3] balance check
  back in force. Reconciling on chain is what decides: a burn that
  landed gets its hash written into the record and the next run resumes
  with `--allow-retry`, and if none landed the reservation refuses with
  exit 3 once preflight passes, whose text is then the procedure. Both
  documents said "it resumes" unconditionally, which was the wrong half
  of that pair; the refusal message itself now promises no exit code at
  all, because which one a re-run reaches is decided on chain and not by
  this run.

  Four more sentences said it, and are corrected: **a re-run resumes a
  recorded burn only with `--allow-retry`**, and without the flag the
  same record is refused with exit 3. A test now holds both documents to
  that — any sentence promising a reader that their re-run will resume
  has to name the flag it needs.

  **A record that cannot be READ moves too.** Stage 1 reads the prior
  record before anything else, and that read is the call that finds out
  whether a burn is recorded — so it cannot be told. It does not need to
  be: an absent record is not an error, so every failure of that read is
  about a file that demonstrably exists. A torn or unreadable record
  exits 10, not 2. Its message already said "do NOT delete it on the
  strength of that"; the exit code said no state file was written, about
  the very file whose existence made the refusal fire.

  **So does the read taken behind a contended lock.** When the
  withdrawal lock is held by another live process, the run reads the
  record to name what that process is doing. That read propagated its
  failure bare — exit 2, "nothing broadcast, no state file written" —
  at the one moment a burn is most likely to be in flight, and it did so
  only for runs that reached it through the burn branch; the resume path
  reclassified it and hid the asymmetry. It is exit 10 now, with the
  same "reconcile before touching that file" text as the stage-1 read.

  Scripts that pattern-match exit codes: this is the last of the exit-2
  reclassifications in this branch. Preflight refusals on a fresh
  withdrawal stay 2; on a withdrawal with a recorded burn, or one whose
  record cannot be read — including the read behind a contended lock —
  they become 10. Exit 3 is untouched — a run refused because another
  process holds the lock still gets the wait remedy and the liveness
  verdict with it.

- **CLI README and advanced runbook: `proof_event_<seq>.json` is not in
  `work_dir/`.** Both directory listings placed it there and the
  runbook's diagnostic looked for it there. It is written only under
  `--prover-out-dir`, which has no default, so the listing described a
  file that never appears and the diagnostic could not fire. The listings
  say where it actually goes, and the diagnostic asks for the directory
  rather than reading an environment variable nothing sets.

- **`ackinacki-bridge withdraw`: the refusal that says "do not delete"
  can no longer be mistaken for the one that says "delete".** When the
  pre-send ownership check refuses, its remedy depends on what the kernel
  reported. In the one case where another process holds the withdrawal —
  and may be inside its send — the record must not be touched. That
  remedy was free to be replaced with either of the other two, which
  correctly authorise a deletion, and nothing objected. No operator-facing
  behaviour changes; what changes is that the wrong version of this
  message can no longer ship.

- **Advanced runbook: the CLI-lane snapshot looked for the proof JSON in
  the wrong directory.** `ls "$WORK_DIR"/proof_event_*.json` printed
  nothing for every run, because that file is written only under
  `--prover-out-dir`, which has no default. A diagnostic that cannot find
  anything reads as one that found nothing wrong. It now looks where the
  file is actually written, and says the flag is required for it to
  exist. The witness file listed beside it does live under `--work-dir`
  and is unchanged.

- **`ackinacki-bridge withdraw`: two more refusals stop claiming nothing
  was broadcast.** Both fire after the AN burn is on the wire and the
  record is written, and both reported exit 2 — whose published contract
  is that nothing was broadcast and no state file was written.

    - Stage 6 re-parses `--eth-private-key`, which stage 1 already
      parsed. Its failure was `exit 2`; it is now **exit 13**, naming the
      stage that could not complete. Nothing is submitted to the EVM
      chain and re-running with `--allow-retry` resumes from the recorded
      burn.
    - The corrupt-record refusal used to end "delete it manually if you
      know it's stale". A resuming run reaches that refusal too, where
      deleting the record destroys the only local trace of a live
      withdrawal. It now says the opposite and points at the runbook's
      Case 3a.

  Scripts that pattern-match exit codes: a stage-6 signer failure changes
  from 2 to 13. Nothing that previously exited 0, 3, 10 or 11 is
  affected.

- **Advanced runbook: a fourth copy of the deletion gate, in the "safe to
  prune between demos" list.** It told an operator to read the exit-3
  refusal because "it reports whether any process still holds the
  withdrawal" — which on a filesystem without `flock` it explicitly does
  not — and gave two conditions where the procedure 400 lines above gives
  three verdicts. A fifth, milder copy was in the CLI README's equivalent
  list. Both now point at the canonical procedure instead of summarising
  it, since every summary so far has dropped the third verdict.

  A test now checks this rather than a reader: any block in the shipped
  documents that sends someone to the exit-3 refusal for the liveness
  answer has to say the answer can be missing.

- **`ackinacki-bridge withdraw`: the burn is refused if this run no
  longer owns the withdrawal it reserved.** The withdrawal lock is taken
  before the reservation and is meant to be held across the multi-second
  `burn::send`, so a second run's liveness probe can see it. Nothing
  enforced that. A run that reached the send holding nothing was
  invisible to that probe, and the concurrent run asking about it was
  told the record had been left by a run that already exited — which the
  recovery procedure turns into permission to delete it, mid-send.

  The last step before the broadcast now asks the kernel whether this run
  still holds the lock, and refuses with **exit 10** if it does not. Exit
  10 and not exit 2, which is what this entry said until the check was
  re-badged: the refusal is raised after `reserve` has published, so a
  reservation for this identity IS on disk, and exit 2's contract is that
  there is none. Nothing was broadcast either way, and exit 10 does not
  claim otherwise — it means this run must not act as if the withdrawal
  were untouched. This is an internal invariant, so in normal operation
  you will never see it. If you do, nothing was broadcast — but **a plain
  re-run is not the remedy**, and the first version of this entry said it
  was. The reservation is already on disk, so the next run refuses it
  with exit 3 and a message written for a burn that may be in flight. The
  refusal therefore names the record and splits by case: another process
  holds the lock (wait, delete nothing); this run held one the kernel no
  longer knows about, which is what a cleanup sweeping `*.lock` produces
  (stop that, and only then delete the record and re-run); or this run
  never took one — one of three cases, so delete the record, re-run, and
  please report it.

  On a filesystem that cannot `flock` at all — a supported deployment —
  there is no lock to check and the run proceeds on the record's own
  guards, as before. A new `stage 3/6: broadcasting the burn` log line
  reports `locked=true|false` so which case you are in is on the record.

  **The lock is now held for the whole run, not just the send.** The
  first version of this guard protected the nine lines between the check
  and the broadcast; everything after — the write that records the AN tx
  hash, then capture, prove and submit, up to ~101 minutes — was back to
  convention, and one line anywhere in there released the lock with the
  build green.

  That was still narrower than this entry first claimed. The check runs
  in one arm of one branch, so the resume path, the "another run
  broadcast while this one preflighted" path, and the tail of the burn
  branch were all uncovered — the second of those being exactly when two
  runs are working the same identity. The lock is now held from the
  moment it is taken, on every path, and releasing it anywhere in
  between fails to compile.

- **`ackinacki-bridge withdraw`: the "could not be determined" liveness
  verdict no longer blames the filesystem for every cause.** The exit-3
  refusal's third verdict said `flock` was unavailable — "a network
  mount, typically" — whenever the lock could not be tested. That is one
  of the causes; `EACCES` on the state directory and `EMFILE` when the
  process is out of file descriptors reach the same arm, and the errno
  that would tell them apart was discarded. An operator out of file
  handles was sent to check their mount.

  The verdict now names no cause, and the reason is emitted as a `warn`
  line immediately above the refusal, carrying the underlying error —
  **for both causes.** The first version of this fix logged only the
  failed attempt (`EACCES`, `EMFILE`), so a filesystem that genuinely
  cannot lock — NFS, overlay mounts, the case the verdict exists for —
  got a sentence pointing at a log line that was never written. Both arms
  now log, each carrying its errno. The runbook's "could not be
  determined" section splits the two situations and says the second one
  is fixable — resolve the error, re-run, and you get a real verdict.

- **Advanced runbook: the delete-the-record procedure now covers all
  three liveness verdicts, not two.** The exit-3 refusal reports one of
  three things about whether another run holds the withdrawal — it holds
  it RIGHT NOW, nobody holds it, or the question could not be answered
  (`flock` unavailable, which is every run on an NFS or overlay mount).
  The procedure listed the first two and said only in the second case
  delete the record, so an operator on a lockless mount reading by
  elimination — not the first case, reconciliation clean — deleted a
  record while another run may have been inside `burn::send`. That is the
  second burn the whole refusal exists to prevent.

  The three verdicts are now a table, the third routed to "do not delete"
  with the two ways out: move `BRIDGE_WITHDRAW_STATE_DIR` to a filesystem
  that implements `flock` and re-run, or establish by other means — `ps`
  on every host sharing the mount — that no run is executing this
  withdrawal. No CLI behaviour changed; the message always said this and
  the procedure did not.

  **The same correction now covers the other two places that stated the
  gate.** The first pass fixed the advanced runbook only. The CLI
  README's cleanup rule ran the identical two-verdict elimination and
  ended by authorising the deletion once "both" were satisfied, without
  saying that one of the two can come back unanswered; and step 3
  of the refusal the CLI itself prints — the copy an operator is looking
  at when they decide — gated the deletion on a description ("the line
  above says no other run holds this withdrawal") that a lockless mount
  satisfies by elimination. The README carries the three-row table, and
  the refusal rules the third verdict out by name.

- **`ackinacki-bridge withdraw`: a failed reservation while resuming a
  recorded burn now exits 10, not 2.** The resume path is entered only
  when the record read at stage 1 carries an AN tx hash, so a burn is on
  the wire for every line inside it, and the run has already written the
  record back before it re-asks the reservation. Those failures reported
  themselves as exit 2, whose documented contract is "nothing broadcast,
  no state file written" — both false there. A wrapper that retries on 2,
  which the exit-code table invites, would have broadcast a second
  `initiateWithdrawal` against a multisig with no replay guard.

  Every pre-send refusal the resume path can raise is now exit 10 and
  names the AN transaction to reconcile instead of the dedup digest —
  including the ones behind the path's very first statement, which
  reaches four of them: preparing the state directory, opening the
  withdrawal lock, reading the record behind a contended one, and the
  reservation itself. The exit-3 duplicate refusals raised by the same
  call are unchanged and still reach the operator as themselves, with
  their per-status remedy — only the pre-send half moves.

  The exit-10 message no longer contradicts itself either: the refusal it
  wraps was written for a caller that had not sent anything, and its
  "nothing was sent" is removed rather than quoted. The AN hash prints
  plainly instead of as `Some("0x…")`, since it is the string an operator
  pastes into an explorer.

  Scripts that pattern-match exit codes: a resumed withdrawal that fails
  to re-reserve changes from 2 to 10. Separately, an internal
  inconsistency detected at the start of the capture stage changes from 2
  to 12 — it is downstream of the burn, so exit 2 was the same lie there.
  Nothing that previously exited 0, 3, 11 or 13 is affected.

- **`ackinacki-bridge withdraw`: the exit-10 documentation no longer
  promises the AN tx hash is on the record.** It said the idempotency
  record is "persisted with the AN tx hash", which is the reverse of the
  dominant case: every failure inside the send propagates before the
  block that writes it, so what an exit 10 usually leaves is a
  `Reserved` record with `an_tx_hash: null` — the CLI cannot write a
  hash it never learned. The hash is there only for the two exit 10s
  raised after the send returned. Reconciliation is the remedy either
  way; where the hash is not on the record it is not in the CLI either,
  and the advanced runbook's Case 3a is the procedure. (The runbook has
  had this right all along; only the exit-code table was wrong.)

- **`ackinacki-bridge withdraw`: the state record drops its unused
  `proof_json_path` field.** It was written as `null` on every record and
  read by nothing; the comment beside the `withdrawByProof` revert path
  claimed the aggregator populated it when `--prover-out-dir` was given,
  which was never true. `--prover-out-dir` itself is unaffected and still
  writes `<dir>/proof_event_<seq>.json`; what it never did was put that
  path on the record. A resumed run re-proves, as it already did, which
  is safe because the proof is deterministic per (event, prover_state).

  Records are compatible in both directions, so a rollback mid-withdrawal
  is not affected: a build without the field ignores it in an older
  record, and a build with it reads a newer record that omits it as
  `null`.

- **`ackinacki-bridge withdraw`: deleting the state record no longer
  substitutes for `--allow-retry`.** When a record is removed between the
  run's first read of it and its reservation — the window the exit-3
  message itself sends a reconciled operator into — the run restores what
  it saw and carries on. It used to ask only whether the restored status
  was terminal, so a `burned` record deleted in that window resumed with
  no `--allow-retry` at all, while the identical record still on disk
  exits 3. The restored file is now put back to `reserve`, which is what
  asks the whole question: the terminal refusal, and the flag as consent
  to act on a withdrawal that already has a record. **A resume after such
  a deletion now needs `--allow-retry`, as it does without one.** The
  record is still restored first, so the flag has something to act on.

- **`ackinacki-bridge withdraw`: a withdrawal lock this run could not take
  is now a refusal, not a downgrade.** Every failure to take the lock was
  read as "this filesystem does not implement `flock`" — a supported
  deployment — and the run continued holding nothing. A run holding
  nothing is invisible to the liveness probe, so the next run is told it
  "has already exited", which the runbook gives as the condition for
  deleting the state record; deleting it while that run is inside
  `burn::send` is a second burn. The failures that reach this are the
  asymmetric ones, where one process fails and another does not: `EMFILE`
  is per-process, and a state directory left half-prepared is finished
  for every run but the one that hit it.

  Only `ENOLCK`, `EOPNOTSUPP` and `ENOSYS` now mean "this filesystem
  cannot lock" and let the run proceed with the liveness evidence
  declared missing. Everything else — including errnos this build has
  never seen — refuses with exit 2 and nothing sent. **Runs that
  previously continued past a lock failure will now stop.** Preparing the
  state directory is also no longer able to answer a question about
  locking: it happens before the lock is attempted, and its failures say
  so in those words.

  Two smaller consequences of the same split: the state directory's own
  entry is now fsynced on every invocation rather than only when this run
  created it — a run that created it and died before syncing left nothing
  behind for a later run to notice — and a state directory reachable by
  anyone but its owner is reported. It is not corrected: 0755 there may be
  a directory a run failed to restrict or one an operator chose, and
  nothing on disk tells them apart.

- **The fixture's `tvm-cli` resolver fails where it decides, not where it
  is used.** When every candidate failed its `version` probe it returned
  the first one anyway — a binary it had just proven does not run — so
  the deploy started, and the failure arrived later as "Exec format
  error" from a command the operator never chose. With nothing on `PATH`
  at all it returned `./contracts/compiler/tvm-cli`, a path that does not
  exist in this repository. It now raises, listing every candidate it
  tried and pointing at
  `crates/ackinacki-bridge/scripts/check_fixture_prereqs.sh`, which
  reports the same thing without starting a deploy.

- **`scripts/check_fixture_prereqs.sh` now picks the `tvm-cli` the fixture
  will pick.** It used `command -v tvm-cli`, which answers with the first
  match on `PATH` and nothing else, and failed if that one could not run
  `version`. The fixture's resolver deliberately tries every match,
  because `deploy_msig_and_mint.py` prepends `python/bin` and the binary
  committed there may be built for another OS/arch — an early entry that
  cannot run, with a working system install behind it. The check
  therefore refused the exact arrangement the fixture supports. It now
  walks `PATH` the same way, probes each candidate with `version`
  (bounded, and without handing it stdin), names the one it selected, and
  reports every candidate it skipped. `CLI_NAME` still wins outright, and
  is probed rather than assumed, because the fixture will use it whether
  or not it runs.

- **Key-cache probe: a cache entry that is not a regular file is refused
  instead of hanging preflight.** The probe guarded the four key paths
  against being a *directory*, on the grounds that keygen replaces them
  by `rename` and cannot write through one. That is the right answer to
  the question of whether keygen can replace the entry, and the wrong one
  for what happens next: the probe then OPENS those paths —
  `read_to_string` for `event_config_params.json`, a streaming digest for
  `event_pk.bin`. `open(2)` on a FIFO blocks until a writer appears, and
  a device node hands the digest a stream with no end. Either one made
  the check hang with no timeout anywhere to end it, on every run, before
  the burn. Such an entry is now `Corrupt` — refuse the run, and
  `probe_event_keys --repair` clears it, since `remove_file` unlinks a
  FIFO exactly as it unlinks a regular file.

- **`ackinacki-bridge withdraw`: the first withdrawal on a host now holds
  its withdrawal lock.** The lock is taken before the reservation, and
  the reservation is what creates the state directory — so on a first
  invocation the lock's own `open` returned ENOENT. That is reported as
  "flock could not be attempted", which is deliberately not a refusal (a
  state directory on a filesystem without working flock is supported), so
  the run proceeded holding nothing. A concurrent retry then probed a
  lock nobody held, was told the first run "has already exited", and the
  documented recovery invites deleting the record on exactly that
  verdict — while the first run may be inside `burn::send`. The lock now
  creates the state directory before opening.

- **`ackinacki-bridge withdraw`: the exit-3 refusal for a record with no
  AN tx hash no longer tells you to pass `--allow-retry`.** A run whose
  reservation found a hash-less record left behind by an earlier run —
  and that ran without `--allow-retry` — was told to "re-run with
  --allow-retry to resume from the recorded burn". Doing that reaches a
  different exit 3 whose own text is "`--allow-retry` does NOT override
  this", leaving deletion of the state record as the only escape an
  operator could find, which is exactly what permits a second burn. A
  record with no hash now gets the refusal that carries the withdrawal
  lock's verdict — whether another process is executing this withdrawal
  right now — and the record's path, in every case rather than only some
  of them. The `--allow-retry` advice remains where it works: on a record
  that carries a hash and therefore resumes at capture.

- **`ackinacki-bridge withdraw`: a `tvm_client` context that cannot be
  built is exit 2, not exit 10.** The withdraw pipeline kept its own copy
  of preflight's context constructor, differing from it in one respect:
  a construction failure was reported as `BurnOutcomeUnknown`, which the
  exit-code table defines as "the USDC has left the source multisig
  regardless". `ClientContext::new` only builds configuration — it does
  not connect — so every failure of it is local and happens before any
  message is composed. In practice preflight builds a context from the
  same `--gql-endpoint` first and refuses there, so this was reachable
  only when the same construction succeeded once and then stopped; the
  copy is gone regardless, and both call sites now share one constructor
  and one exit code.

- **`ackinacki-bridge withdraw`: a withdrawal that has already paid out
  can no longer be resumed as an unpaid one.** The resume branch is
  entered whenever the prior record carries an AN tx hash — it does not
  look at the status — and the refusal that stops a `confirmed` or
  `submitted` record only fires while that record's file is still on
  disk. Deleting the file during a run's preflight, which takes minutes
  because it hashes a ~2.65 GB proving key, therefore produced a fresh
  reservation that the resume path rewrote as `burned`: the run then
  carried a completed withdrawal through capture, prove and a **second**
  `withdrawByProof`. The record is now restored exactly as the run read
  it — status, AN tx hash and `eth_tx_hash` — and a restored `confirmed`
  or `submitted` record is refused with exit 3 and the same message it
  would have earned had the file never been deleted.

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

- **The documented exit-3 recovery now reaches the message it describes.**
  The procedure is "re-run the identical command and read the refusal" —
  that refusal reports whether another process still holds the
  withdrawal, which is the condition for deleting the record. The
  identical command carries no `--allow-retry`, and that path produced a
  different exit 3: no liveness verdict, no record path, and advice to
  pass `--allow-retry`, which lands on the refusal it was supposed to be.
  Its "Prior AN tx" was also structurally empty there, printing
  `None` — which reads as "nothing was sent" — about a record that may be
  a burn in flight.

  Both paths now produce the same refusal, and `--allow-retry` no longer
  changes it: with the flag the run used to warn, compose, and be refused
  anyway, so the flag only bought the operator a prompt and some work
  before the same answer. The recovery it does not block is the intended
  one — an operator who reconciled and deleted the record is not refused,
  because there is no record left to find.

  `duplicate in-flight` refusals carry their own remedy for the same
  reason. The sentence used to end "re-run with `--allow-retry` to
  override" for every status, including `confirmed` and `submitted`,
  which refuse that flag outright — telling half its readers to try the
  one thing that cannot work for them.

- **Case 3a's first diagnostic matches the log again.** It grepped for
  `capture: matched`, `capture: polling` and `enrich_witness: filling`,
  none of which any binary has ever written, so an operator could not
  classify their incident before reaching any of the advice below it. It
  now greps `captured WithdrawalInitiated event` — the one line the
  capture stage writes on success, whose absence is the whole diagnosis —
  and reads the event's seq_no out of that line instead of a `seq_no=`
  field that does not exist.

- **A missing ceremony file is no longer reported as an unreadable one.**
  A regression from this branch's own error-message work: the code that
  started carrying the halo2 reader's reason also warned "the
  exact-degree ceremony file is unreadable" when the file was simply
  absent — which is the normal case, since the shipped layout provisions
  `kzg_bn254_21.srs` and derives lower degrees from it. Absent and
  unreadable are now distinct, and only the second warns.

- **A full or closed output stream no longer replaces the exit code with
  101.** `println!`/`eprintln!` panic when the write fails, so
  `> /dev/full`, a full disk or a closed pipe turned every refusal into
  the panic exit — discarding the 0/2/3/10/11/12/13 contract that exists
  precisely so a script can tell "nothing was broadcast" from "broadcast,
  outcome unknown". The worst case was the success summary, which is
  reached only after the burn landed **and** `withdrawByProof` was mined:
  value moved on both chains and the wrapper was told the process died of
  something unknown.

  Every terminal write is now a single checked `write_all` with one
  fallback hop to the other stream, prefixed so a rescued line is not
  mistaken for the machine output. With both streams gone the process
  stays silent and still exits with the code that describes what happened
  to the money.

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
