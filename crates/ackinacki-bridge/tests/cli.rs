//! End-to-end tests that run the real `ackinacki-bridge` binary.
//!
//! Everything else in this crate tests functions. This file tests the
//! program: argv in, exit code and bytes out. That matters because the
//! exit code IS the interface — the whole point of the 0/2/3/10/11/12/13
//! discipline is that a script can tell "nothing was broadcast" from
//! "broadcast, outcome unknown" without reading prose — and because
//! `main.rs` is otherwise reachable by no test at all: its three
//! pre-parse escapes (`--json` argv scan, `BRIDGE_CONFIG` loading, the
//! help/version passthrough), its clap-prefix strip, and the
//! `--non-interactive` policy all live above every unit test's entry
//! point.
//!
//! **Every test here stops before the network.** Stage 1 makes GraphQL and
//! JSON-RPC calls, so anything that reaches it would need a live shellnet;
//! these all refuse during argv parsing, profile loading or the dispatch
//! policy, which is exactly the region `main.rs` owns.

use std::process::{Command, Output};

/// The binary cargo just built for this test target.
fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ackinacki-bridge"));
    // A profile left in the environment by the developer's shell would
    // silently supply flags these tests assume are missing, and
    // `BRIDGE_*` env vars feed clap's `env =` attributes directly.
    c.env_clear();
    // Keep the loader working; `env_clear` also drops PATH.
    if let Some(p) = std::env::var_os("PATH") {
        c.env("PATH", p);
    }
    c
}

fn run(args: &[&str]) -> Output {
    bin()
        .args(args)
        .output()
        .expect("the binary must be runnable")
}

fn code(out: &Output) -> i32 {
    out.status
        .code()
        .expect("the process must not be signalled")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// The `{"error":{stage,exit_code,message}}` envelope, parsed. Asserts the
/// envelope is on stdout, is one line, and that its `exit_code` agrees with
/// the process's — a machine consumer reads one of the two and must not be
/// able to pick the wrong one.
fn error_envelope(out: &Output) -> serde_json::Value {
    let s = stdout(out);
    let line = s.trim();
    assert!(
        !line.contains('\n'),
        "the envelope must be one line for a line-oriented consumer, got: {line}"
    );
    let v: serde_json::Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {line:?}"));
    assert_eq!(
        v["error"]["exit_code"].as_i64(),
        Some(code(out) as i64),
        "the envelope's exit_code must equal the process exit code: {line}"
    );
    // Always present, `[]` when there is no source. The field carries the
    // `#[source]` chain that `--json` used to drop entirely — a stage-5
    // failure said "withdraw-e2e pipeline failed" and nothing about why,
    // on the one output mode a script reads. Asserting it here means
    // every integration case pins its presence, not just the one test
    // that cares about a deep chain.
    assert!(
        v["error"]["causes"].is_array(),
        "the envelope must always carry a causes list: {line}"
    );
    v
}

/// The minimum argv that parses. Every test that needs to get PAST clap
/// starts here and overrides what it is testing. The addresses are
/// syntactically valid and point nowhere: no test in this file reaches the
/// network.
const BASE: &[&str] = &[
    "withdraw",
    // `dapp_id::account_id`, both bare 64-hex: no `0x`, no workchain.
    "--from",
    "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a::3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c",
    "--from-keys",
    "/nonexistent/keys.json",
    "--to",
    "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
    "--to-chain",
    "11155111",
    "--amount",
    "1.000000",
    "--gql-endpoint",
    "http://127.0.0.1:1/graphql",
    "--usdc-bridge-account",
    "2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b",
    "--rpc-url",
    "http://127.0.0.1:1/rpc",
    "--bridge-address",
    "0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7",
];

fn with(extra: &[&str]) -> Vec<String> {
    BASE.iter()
        .chain(extra.iter())
        .map(|s| s.to_string())
        .collect()
}

/// [`BASE`] with some flags' values REPLACED rather than appended. clap
/// refuses a repeated `--to` outright ("cannot be used multiple times"), so
/// appending an override produces a usage error instead of exercising the
/// validation the test is about — which still exits 2 and still looks like
/// a pass if the assertion only checks the code.
fn base_overriding(overrides: &[(&str, &str)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(BASE.len());
    let mut i = 0;
    while i < BASE.len() {
        let tok = BASE[i];
        match overrides.iter().find(|(flag, _)| *flag == tok) {
            // Flag plus its value: keep the flag, swap the value, skip both.
            Some((flag, value)) => {
                out.push((*flag).to_string());
                out.push((*value).to_string());
                i += 2;
            },
            None => {
                out.push(tok.to_string());
                i += 1;
            },
        }
    }
    for (flag, _) in overrides {
        assert!(
            out.iter().any(|t| t == flag),
            "{flag} is not in BASE — the override would have been silently dropped"
        );
    }
    out
}

fn run_args(args: &[String]) -> Output {
    bin()
        .args(args.iter().map(String::as_str))
        .output()
        .expect("the binary must be runnable")
}

fn run_with(extra: &[&str]) -> Output {
    run_args(&with(extra))
}

// -- The three pre-parse escapes in main.rs ------------------------------

#[test]
fn help_and_version_are_not_errors() {
    // They arrive from clap as `Err`, and treating them as refusals would
    // hand `--json` an error envelope for `--help` and exit 2.
    for flag in ["--help", "--version"] {
        let out = run(&[flag]);
        assert_eq!(code(&out), 0, "{flag} must exit 0");
        assert!(
            !stdout(&out).is_empty(),
            "{flag} must print something to stdout"
        );
    }
    assert!(
        stdout(&run(&["withdraw", "--help"])).contains("--from-keys"),
        "subcommand help must document the subcommand's flags"
    );
}

#[test]
fn a_usage_error_is_exit_2_and_is_not_prefixed_twice() {
    // clap's rendering already begins "error: ", and `print_error`'s human
    // branch adds one. Both together printed `error: error: the following
    // required arguments…`.
    let out = run(&["withdraw"]);
    assert_eq!(code(&out), 2, "a usage error is a pre-send refusal");
    let e = stderr(&out);
    assert!(e.starts_with("error: "), "human errors are prefixed: {e:?}");
    assert!(
        !e.contains("error: error:"),
        "the prefix must appear exactly once: {e:?}"
    );
    assert!(e.contains("--from"), "must name what is missing: {e:?}");
}

#[test]
fn a_missing_subcommand_is_a_refusal_not_a_help_page() {
    // `DisplayHelpOnMissingArgumentOrSubcommand` is deliberately NOT in the
    // help/version passthrough: it is a usage error, and routing it there
    // would exit 0 with a help page for what is a refusal.
    let out = run(&[]);
    assert_eq!(code(&out), 2);
    let out = run(&["--json"]);
    assert_eq!(code(&out), 2);
    let v = error_envelope(&out);
    assert_eq!(v["error"]["stage"], "preflight");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("requires a subcommand"),
        "got: {v}"
    );
}

#[test]
fn the_json_flag_is_honoured_before_clap_has_parsed_it() {
    // `--json` is a clap flag, but a parse failure happens before there is
    // a parsed `Cli` to read it from — so main scans argv literally. Two
    // things to pin: the envelope goes to STDOUT (stderr is where the
    // human line goes, and a consumer reading the wrong stream sees
    // nothing), and stderr carries no human line at all under --json.
    let out = run(&["--json", "withdraw"]);
    assert_eq!(code(&out), 2);
    error_envelope(&out);
    assert!(
        stderr(&out).is_empty(),
        "--json must not also print the human line: {:?}",
        stderr(&out)
    );

    // And the scan must find the flag wherever it sits, including after
    // the subcommand.
    let out = run(&["withdraw", "--json"]);
    assert_eq!(code(&out), 2);
    error_envelope(&out);
}

#[test]
fn an_unloadable_bridge_config_is_a_usage_refusal() {
    // The profile is sourced before clap reads any `env =` attribute, so
    // this failure has no parsed `Cli` behind it either.
    let out = bin()
        .env("BRIDGE_CONFIG", "/nonexistent/profile.env")
        .args(["--json", "withdraw"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2);
    let v = error_envelope(&out);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("/nonexistent/profile.env"), "got: {msg}");
    assert!(msg.contains("BRIDGE_CONFIG"), "got: {msg}");
}

#[test]
#[cfg(unix)]
fn a_bridge_config_that_is_not_utf8_is_refused_rather_than_ignored() {
    // `std::env::var` has three answers and the profile loader read the
    // third as the first: `if let Ok(path)` treated a non-UTF-8
    // `BRIDGE_CONFIG` exactly like an unset one and carried on with
    // compiled defaults — before `init_tracing`, so with no line anywhere
    // saying the profile had not been sourced.
    //
    // The profile is where `BRIDGE_WITHDRAW_STATE_DIR` comes from, and
    // the idempotency key does not include the directory. An ignored
    // profile therefore reserves in a directory that holds no record of a
    // withdrawal that has one — `peek` says none, `reserve` says created,
    // `decide_burn` says send — and the multisig has no replay guard.
    //
    // Driven through the binary because that is the only place this code
    // runs: `main` is not a library target, and everything here happens
    // before there is a parsed `Cli` to test against.
    use std::os::unix::ffi::OsStrExt;

    let out = bin()
        .env(
            "BRIDGE_CONFIG",
            std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfeprofile"),
        )
        .args(["--json", "withdraw"])
        .output()
        .unwrap();
    assert_eq!(
        code(&out),
        2,
        "nothing is broadcast and nothing is written this early"
    );
    let v = error_envelope(&out);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("not valid UTF-8"), "got: {msg}");
    assert!(
        msg.contains("second burn"),
        "the refusal has to say what being ignored would have cost, or the next reader files it \
         as pedantry about encodings: {msg}",
    );
    // And the raw value never reaches the terminal unescaped: it comes
    // from the environment, it is malformed by definition, and a newline
    // in it forges a line the CLI never wrote.
    assert!(
        !msg.contains('\n') || !msg.lines().any(|l| l.starts_with("/tmp/")),
        "got: {msg}",
    );
}

#[test]
fn a_profile_supplies_flags_the_command_line_omits() {
    // The other half: a profile that loads must actually reach clap's
    // `env =` attributes. Without this, the test above passes just as well
    // against a build that ignores BRIDGE_CONFIG entirely.
    let dir = tempfile::TempDir::new().unwrap();
    let profile = dir.path().join("profile.env");
    std::fs::write(&profile, "BRIDGE_PARAMS_DIR=/tmp/params-from-profile\n").unwrap();

    // Without the profile: params_dir is unset, and a real run names it.
    //
    // `--state-dir` is supplied because the state directory is now
    // resolved BEFORE the plumbing, so that the plumbing refusal can be
    // told whether a record exists. With a cleared environment and no
    // flag, the run would complain about HOME first — a different
    // refusal from the one this test is about.
    let state = tempfile::TempDir::new().unwrap();
    let state_flag = state.path().to_str().unwrap();
    let bare = run_with(&["--json", "--state-dir", state_flag]);
    assert_eq!(code(&bare), 2);
    let msg = error_envelope(&bare)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("--params-dir"),
        "a real run without plumbing must name it: {msg}"
    );

    // With it: the same command no longer complains about that one flag.
    let args = with(&["--json", "--state-dir", state_flag]);
    let out = bin()
        .env("BRIDGE_CONFIG", &profile)
        .args(args.iter().map(String::as_str))
        .output()
        .unwrap();
    assert_eq!(code(&out), 2);
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !msg.contains("--params-dir"),
        "the profile supplied it: {msg}"
    );
}

// -- The dispatch policy -------------------------------------------------

#[test]
fn non_interactive_without_yes_is_refused_before_anything_runs() {
    let out = run_with(&["--non-interactive", "--json"]);
    assert_eq!(code(&out), 2);
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("--non-interactive"), "got: {msg}");
    assert!(msg.contains("--yes"), "must name the way out: {msg}");
    assert!(msg.contains("--dry-run"), "and the other one: {msg}");
}

#[test]
fn non_interactive_with_dry_run_gets_past_the_policy() {
    // `--dry-run` never prompts, so there is nothing to block on. It fails
    // later (there is no node at 127.0.0.1:1) — the assertion is that it
    // fails for a DIFFERENT reason than the policy.
    let out = run_with(&["--non-interactive", "--dry-run", "--json"]);
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !msg.contains("--non-interactive requires"),
        "the policy must not fire: {msg}"
    );
}

// -- Argument validation, through the real binary ------------------------

#[test]
fn a_dry_run_never_reports_a_duplicate() {
    // README: `--dry-run` can produce 0, 2 or 10. It never reserves and
    // never writes, so exit 3 is unreachable under it — and a state
    // directory full of records must not change that. It does read: a
    // dry run that finds a record for THIS identity refuses with 10
    // rather than claiming there is none, which is what the planted
    // record below is not (its name is not this run's key).
    // The state dir has to contain a record for this to test anything —
    // and BASE's `--from-keys /nonexistent/keys.json` refuses at the
    // perms check, which is step 1 of preflight, so the run never reaches
    // idempotency at all. This asserted `code == 0 || code == 2` against
    // a run that exited 2 on the key file: true, and about nothing.
    let dir = tempfile::TempDir::new().unwrap();
    let keys = dir.path().join("owner.keys.json");
    std::fs::write(
        &keys,
        format!(
            r#"{{"public":"{}","secret":"{}"}}"#,
            "1a".repeat(32),
            "2b".repeat(32)
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o400)).unwrap();

    // A record for THIS run's identity, in the state dir it is given. A
    // real run would refuse it with exit 3; a dry run must not look.
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let planted = state.join(format!("{}.json", "7f".repeat(32)));
    std::fs::write(&planted, "{}").unwrap();

    let mut args = base_overriding(&[("--from-keys", keys.to_str().unwrap())]);
    args.extend(
        ["--dry-run", "--yes", "--json", "--state-dir"]
            .iter()
            .map(|s| s.to_string()),
    );
    args.push(state.to_str().unwrap().to_string());
    let out = run_args(&args);
    assert_ne!(code(&out), 3, "a dry run cannot refuse as a duplicate");
    // It got past the key file — otherwise this is the old vacuous test
    // wearing a longer body.
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !msg.contains("mode is") && !msg.contains("chmod 400"),
        "the run must reach past the key-perms check to say anything about idempotency: {msg}",
    );
    assert!(
        code(&out) == 0 || code(&out) == 2 || code(&out) == 10,
        "a dry run exits 0, 2 or 10, got {}",
        code(&out)
    );
    assert_eq!(
        std::fs::read_dir(&state).unwrap().count(),
        1,
        "a dry run must neither read nor write the store — the planted record is untouched and no \
         new one appeared",
    );
    assert_eq!(std::fs::read_to_string(&planted).unwrap(), "{}");
}

#[test]
fn a_dry_run_does_not_need_a_state_directory_at_all() {
    // The regression the test above could not see, because it passes
    // `--state-dir`. `bin()` clears the environment — which is what
    // systemd, cron and most Docker images hand a process — and the
    // unset-HOME refusal was being raised BEFORE the `if dry_run` guard.
    // So the one command whose whole purpose is to be safe to run
    // anywhere started failing with a double-burn refusal about a
    // directory it never touches.
    //
    // The previous round documented this instead of noticing it: the
    // key-perms test grew a comment explaining that a dry run now needs
    // a state dir. It does not.
    let out = run_with(&["--dry-run", "--yes", "--json"]);
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !msg.contains("HOME is not set"),
        "a dry run reads and writes no state, so it must not be refused for not being able to \
         name that directory: {msg}"
    );
    // It still fails — there is no node at 127.0.0.1:1 — and that is the
    // point: it fails at the network, where a dry run is supposed to.
    assert_eq!(code(&out), 2);
}

#[test]
fn a_full_output_stream_does_not_replace_the_exit_code_with_101() {
    // `println!` and `eprintln!` PANIC when the write fails, so a full
    // disk, a closed pipe or a `> /dev/full` turned every refusal into
    // exit 101 — discarding the exit-code contract this whole branch is
    // about. The worst case is not reachable from a test: `print_success`
    // runs only after the burn landed and `withdrawByProof` was mined, so
    // the process that told a wrapper "unknown failure" had already moved
    // value on both chains. Same `emit`, so pinning the error path pins
    // that one too.
    if !std::path::Path::new("/dev/full").exists() {
        // Linux-only device. Skipping is right: inventing a pass on a
        // platform where the case cannot arise would be worse.
        return;
    }
    let devfull = || {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap()
    };

    // The machine path: the envelope's own stream is full.
    let out = bin()
        .args(["--json", "withdraw"])
        .stdout(devfull())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "a usage error is exit 2 whether or not stdout accepted the envelope",
    );
    // And it is not silently dropped: one hop to the other stream, marked
    // so nobody parses a rescued line as the machine contract.
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("NOT the machine output"), "got: {e}");
    assert!(
        e.contains("\"exit_code\":2"),
        "the envelope itself is repeated: {e}"
    );

    // The human path: stderr is full instead.
    let out = bin()
        .args(["withdraw"])
        .stdout(std::process::Stdio::piped())
        .stderr(devfull())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("--from"),
        "the human error is rescued to the other stream",
    );

    // Both gone. Nothing can be said, and the exit code still says it.
    let out = bin()
        .args(["--json", "withdraw"])
        .stdout(devfull())
        .stderr(devfull())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "with nowhere to write, the exit code is the whole answer and must survive",
    );
}

#[test]
fn the_zero_recipient_is_refused() {
    let mut args = base_overriding(&[("--to", "0x0000000000000000000000000000000000000000")]);
    args.push("--json".to_string());
    let out = run_args(&args);
    assert_eq!(code(&out), 2);
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("0x0") || msg.to_lowercase().contains("zero"),
        "got: {msg}"
    );
}

#[test]
fn a_group_readable_key_file_is_refused_with_the_remedy() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::TempDir::new().unwrap();
    let keys = dir.path().join("owner.keys.json");
    std::fs::write(&keys, r#"{"public":"00","secret":"00"}"#).unwrap();
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o440)).unwrap();

    // `--dry-run` because `require_submit_plumbing` refuses first otherwise,
    // and that refusal is not what this test is about. The perms check is
    // step 1 of preflight — ahead of the GraphQL client — so a dry run
    // still reaches it without touching the network.
    let mut args = base_overriding(&[("--from-keys", keys.to_str().unwrap())]);
    args.push("--dry-run".to_string());
    args.push("--json".to_string());
    // A state dir so this exercises the key-perms refusal and not the
    // network. (It used to be here because an unset HOME refused a dry
    // run outright — that was a bug, and it is fixed.)
    args.push("--state-dir".to_string());
    args.push(dir.path().to_str().unwrap().to_string());
    let out = run_args(&args);
    assert_eq!(code(&out), 2, "a readable-by-others key file is a refusal");
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("0440"), "must name the mode found: {msg}");
    assert!(msg.contains("chmod 400"), "must give the remedy: {msg}");
}

#[test]
fn an_unset_home_is_refused_rather_than_silently_using_the_cwd() {
    // Not a synthetic case: `bin()` clears the environment, which is what
    // systemd, cron and `sudo` without `-H` hand a process. The fallback
    // used to be ".", which makes the double-burn guard relative to
    // wherever the operator was standing — the same command from two
    // directories finds no prior record either time and burns twice.
    //
    // A real run, not a dry run: a dry run neither reads nor writes the
    // store, so it has no state directory to be wrong about.
    // A real run refuses for missing submit plumbing first, and that is
    // not the refusal under test — supply all five.
    let plumbing = tempfile::TempDir::new().unwrap();
    let p = plumbing.path().to_str().unwrap();
    let args = with(&[
        "--json",
        "--yes",
        "--eth-private-key",
        "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a",
        "--aggregator-dir",
        p,
        "--verifiers-dir",
        p,
        "--params-dir",
        p,
        "--work-dir",
        p,
    ]);
    let out = run_args(&args);
    assert_eq!(
        code(&out),
        10,
        "nothing may be broadcast without a state dir — and exit 2 would have said there is no \
         record for this identity on disk, about a directory this run could not even name",
    );
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("HOME is not set"),
        "must name the cause: {msg}"
    );
    assert!(msg.contains("--state-dir"), "must name the way out: {msg}");
    assert!(
        msg.contains("could not LOOK"),
        "and say why this is not exit 2: an earlier run with HOME set may have recorded a burn, \
         and this one has no directory to read it from: {msg}",
    );
    assert!(
        msg.contains("burned twice") || msg.contains("twice"),
        "must say what is at stake: {msg}"
    );

    // And with HOME set it is no longer the complaint. (This one gets as
    // far as the network and fails there — 127.0.0.1:1 refuses instantly —
    // which is the point: a different refusal.)
    let home = tempfile::TempDir::new().unwrap();
    let out = bin()
        .env("HOME", home.path())
        .args(args.iter().map(String::as_str))
        .output()
        .unwrap();
    let msg = error_envelope(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!msg.contains("HOME is not set"), "got: {msg}");
}

#[test]
fn a_malformed_argument_is_a_refusal_and_not_a_panic() {
    // `redact` used to slice at byte 24 and panic on a multibyte character
    // there, which exits 101 with a message no consumer can parse — the
    // one outcome the --json envelope exists to prevent.
    for bad in [
        "0x742d35Cc6634C0532приветик",
        "0x742d35Cc6634C0532🙂🙂🙂🙂",
        "0x742d35Cc6634C0532日本語日本語",
    ] {
        let mut args = base_overriding(&[("--to", bad)]);
        args.push("--json".to_string());
        let out = run_args(&args);
        assert_eq!(code(&out), 2, "{bad:?} must be refused, not crash");
        error_envelope(&out);
    }
}

#[test]
fn a_control_character_in_an_argument_does_not_reach_the_terminal() {
    // A newline in argv would otherwise forge a line inside a multi-line
    // refusal, and an ANSI escape would repaint the terminal the refusal is
    // read on.
    let out = run_args(&base_overriding(&[(
        "--to",
        "0x742d35Cc6634C0\n\u{1b}[2Jinjected",
    )]));
    assert_eq!(code(&out), 2);
    let e = stderr(&out);
    assert!(!e.contains('\u{1b}'), "escape sequence survived: {e:?}");
    assert!(
        e.contains("\\n") || !e.contains("\ninjected"),
        "the newline was replayed verbatim: {e:?}"
    );
}
