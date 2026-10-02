//! Empirical parity probe: does this workspace's `tvm-sdk` pin still decode
//! the wire format your live Acki Nacki node emits?
//!
//! Motivation. `crates/bridge-prover-libraries` pins `tvm-sdk = v3.0.6.an`
//! while the live multi-thread node (`acki-nacki@state_v2`) pins the SDK to
//! the `state_v2` branch — see `acki-nacki/Cargo.toml:132-139`. If
//! `state_v2` changed cell/BOC/Message wire encoding, everything the bridge
//! decodes from GraphQL (ExtOut `boc` field → `Message` → cell tree →
//! Poseidon leaves → repr_hash) can silently drift. Shellnet is
//! single-thread and stays on `v3.0.6.an` too, so this drift never fires
//! there.
//!
//! Two modes, both hermetic:
//!
//! * `--boc-base64 <B64>`: pure offline parse. Paste any ExtOut BOC (e.g.
//!   from `curl` against the node's `/graphql`) and see if this workspace's
//!   `tvm_block::Message::construct_from_base64` accepts it.
//! * `--gql-url <URL> --account-id <HEX> --dapp-id <HEX> [--limit N]`:
//!   pull the last `N` ExtOut messages from the node's own GQL and parse
//!   each. Requires a real bridge account with at least one ExtOut event.
//!
//! Per-BOC checks:
//! 1. **Parse**   — `Message::construct_from_base64(boc)` succeeds.
//! 2. **Walk**    — `serialize_cells_tree_root_first(root)` traverses the
//!    DAG without erroring on unknown cell types.
//! 3. **Descriptor parity** — for every FlatCell, independently
//!    `SHA-256(cell_repr_data) == repr_hash`. This is the state_v2
//!    tripwire: `build_cell_repr_data` is a byte-exact local port of
//!    `tvm-sdk@v3.0.6.an`'s descriptor packing, and if `state_v2` renamed
//!    or reshaped any cell descriptor field the hash reconstruction
//!    diverges from tvm_types' own `cell.repr_hash()`.
//! 4. **GQL identity** (GQL mode only) — the `BridgeExtOutMessage.id`
//!    field returned by the node equals `hex(root.repr_hash)`.
//!
//! Exit codes: `0` all pass, `1` parse failure (wire format changed),
//! `2` descriptor parity failure (cell-layout drift), `3` usage / network
//! error. Non-zero and the answer to "should we move the bridge to
//! `state_v2`?" is yes.

use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use bridge_event_witness::boc_walk::{serialize_cells_tree_root_first, FlatCell};
use bridge_gql_fetcher::gql_client::{create_client, BridgeExtOutMessage};
use sha2::{Digest, Sha256};
use tvm_block::{Deserializable, Message, Serializable};

struct Args {
    mode: Mode,
    verbose: bool,
}

enum Mode {
    Local {
        boc_b64: String,
    },
    Gql {
        url: String,
        account_id: String,
        dapp_id: String,
        limit: u32,
    },
}

fn parse_args() -> Result<Args> {
    let mut boc_b64: Option<String> = None;
    let mut gql_url: Option<String> = None;
    let mut account_id: Option<String> = None;
    let mut dapp_id: Option<String> = None;
    let mut limit: u32 = 1;
    let mut verbose = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--boc-base64" => {
                boc_b64 = Some(it.next().context("--boc-base64 requires a value")?);
            },
            "--gql-url" => {
                gql_url = Some(it.next().context("--gql-url requires a value")?);
            },
            "--account-id" => {
                account_id = Some(it.next().context("--account-id requires a value")?);
            },
            "--dapp-id" => {
                dapp_id = Some(it.next().context("--dapp-id requires a value")?);
            },
            "--limit" => {
                limit = it
                    .next()
                    .context("--limit requires a value")?
                    .parse()
                    .context("--limit must be a positive integer")?;
            },
            "-v" | "--verbose" => verbose = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            },
            other => bail!("unknown flag: {other}"),
        }
    }

    let mode = match (boc_b64, gql_url) {
        (Some(b), None) => Mode::Local { boc_b64: b },
        (None, Some(u)) => {
            let account_id = account_id.context(
                "--account-id is required with --gql-url (64 lowercase hex chars)",
            )?;
            let dapp_id =
                dapp_id.context("--dapp-id is required with --gql-url (64 lowercase hex chars)")?;
            Mode::Gql {
                url: u,
                account_id,
                dapp_id,
                limit,
            }
        },
        (Some(_), Some(_)) => bail!("--boc-base64 and --gql-url are mutually exclusive"),
        (None, None) => bail!("one of --boc-base64 or --gql-url is required (see --help)"),
    };

    Ok(Args { mode, verbose })
}

fn print_help() {
    println!(
        "\
probe_tvm_decode — empirical tvm-sdk wire-format parity probe

USAGE:
    probe_tvm_decode --boc-base64 <B64> [-v]
    probe_tvm_decode --gql-url <URL> --account-id <HEX> --dapp-id <HEX> \
                     [--limit N] [-v]

MODES:
    Local (offline):  --boc-base64 <B64>
        Parses one base64-encoded ExtOut Message BOC. No network.
    GQL   (online):   --gql-url <URL> --account-id <HEX> --dapp-id <HEX>
        Pulls the last N ExtOut messages from the account via GraphQL and
        parses each. Default --limit=1.

OPTIONS:
    --boc-base64 B64         Base64-encoded ExtOut Message BOC (offline mode).
    --gql-url URL            GraphQL endpoint of the AN node (e.g.
                             http://127.0.0.1:8600/graphql).
    --account-id HEX         64-hex account id emitting ExtOut messages.
    --dapp-id HEX            64-hex dapp id owning the account.
    --limit N                Number of recent messages to probe (default 1).
    -v, --verbose            Print every cell's repr_hash + descriptor size.
    -h, --help               Show this help.

EXIT CODES:
    0  All checks passed.
    1  Parse failure — Message::construct_from_base64 or the tree walk
       rejected the BOC. Wire format has changed; the bridge must migrate
       tvm-sdk pins (likely to acki-nacki's branch = 'state_v2').
    2  Descriptor parity failure — SHA-256(cell_repr_data) != repr_hash for
       at least one cell. Cell descriptor layout has changed; the bridge's
       `boc_walk::build_cell_repr_data` needs an update alongside the pin
       bump. This is the harder migration.
    3  Usage or network error.

INTERPRETATION:
    Exit 0 means this workspace's tvm-sdk = v3.0.6.an pin still decodes
    the node's ExtOut BOCs correctly. Bridge on v3.0.6.an is safe against
    the state_v2 multi-thread node for the code paths this probe exercises
    (Message parsing + cell tree walk + descriptor SHA reconstruction).
    Empirical result, not a proof — deeper state_v2 changes (e.g. new Block
    fields) can still bite; this probe covers the surface the Circuit-4
    event witness pipeline touches.
"
    );
}

/// Run all parity checks on one BOC. Returns the root repr_hash on success.
fn probe_one(boc_b64: &str, verbose: bool) -> Result<[u8; 32], ProbeError> {
    let msg = Message::construct_from_base64(boc_b64).map_err(|e| {
        ProbeError::Parse(format!("Message::construct_from_base64 failed: {e}"))
    })?;

    let root = msg
        .serialize()
        .map_err(|e| ProbeError::Parse(format!("msg.serialize() failed: {e}")))?;

    let cells: Vec<FlatCell> = serialize_cells_tree_root_first(&root).map_err(|e| {
        ProbeError::Parse(format!("serialize_cells_tree_root_first failed: {e}"))
    })?;

    if cells.is_empty() {
        return Err(ProbeError::Parse("cell tree walk produced 0 cells".into()));
    }

    for (i, c) in cells.iter().enumerate() {
        let mut h = Sha256::new();
        h.update(&c.cell_repr_data);
        let got: [u8; 32] = h.finalize().into();
        if got != c.repr_hash {
            return Err(ProbeError::Descriptor {
                cell_index: i,
                claimed: hex::encode(c.repr_hash),
                recomputed: hex::encode(got),
                repr_len: c.cell_repr_data.len(),
                refs: c.refs_count,
            });
        }
        if verbose {
            println!(
                "  cell[{i:>2}] refs={} repr_len={:>4} repr_hash={}",
                c.refs_count,
                c.cell_repr_data.len(),
                hex::encode(c.repr_hash),
            );
        }
    }

    Ok(cells[0].repr_hash)
}

enum ProbeError {
    Parse(String),
    Descriptor {
        cell_index: usize,
        claimed: String,
        recomputed: String,
        repr_len: usize,
        refs: u8,
    },
}

impl ProbeError {
    fn exit_code(&self) -> u8 {
        match self {
            ProbeError::Parse(_) => 1,
            ProbeError::Descriptor { .. } => 2,
        }
    }
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Parse(m) => write!(f, "parse failure: {m}"),
            ProbeError::Descriptor {
                cell_index,
                claimed,
                recomputed,
                repr_len,
                refs,
            } => write!(
                f,
                "descriptor parity failure at cell[{cell_index}]:\n  \
                 repr_len={repr_len}, refs={refs}\n  \
                 claimed (cell.repr_hash)   = {claimed}\n  \
                 recomputed (SHA256 preimg) = {recomputed}"
            ),
        }
    }
}

async fn run_local(boc_b64: &str, verbose: bool) -> ExitCode {
    println!("[probe_tvm_decode] mode=local");
    match probe_one(boc_b64, verbose) {
        Ok(root_hash) => {
            println!(
                "[probe_tvm_decode] OK — root repr_hash = {}",
                hex::encode(root_hash)
            );
            ExitCode::SUCCESS
        },
        Err(e) => {
            eprintln!("[probe_tvm_decode] FAIL — {e}");
            ExitCode::from(e.exit_code())
        },
    }
}

async fn run_gql(
    url: &str,
    account_id: &str,
    dapp_id: &str,
    limit: u32,
    verbose: bool,
) -> ExitCode {
    println!("[probe_tvm_decode] mode=gql url={url} account={account_id} dapp={dapp_id} limit={limit}");

    let gql = match create_client(url) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[probe_tvm_decode] GQL client init failed: {e:#}");
            return ExitCode::from(3);
        },
    };

    let msgs: Vec<BridgeExtOutMessage> =
        match gql.query_bridge_extouts(account_id, dapp_id, limit).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[probe_tvm_decode] query_bridge_extouts failed: {e:#}");
                return ExitCode::from(3);
            },
        };

    if msgs.is_empty() {
        eprintln!(
            "[probe_tvm_decode] the node returned 0 ExtOut messages for account={account_id} \
             dapp={dapp_id}. Either the account has emitted no events yet, or the GQL server is \
             filtering them. Cannot probe. Retry after a real WithdrawalInitiated fires, or use \
             --boc-base64 with any BOC you can obtain by other means."
        );
        return ExitCode::from(3);
    }

    println!("[probe_tvm_decode] probing {} message(s)…", msgs.len());
    let mut fail: Option<u8> = None;

    for (i, m) in msgs.iter().enumerate() {
        println!(
            "\n[msg {}/{}] gql_id={} block_id={:?} created_at={:?}",
            i + 1,
            msgs.len(),
            m.id,
            m.block_id,
            m.created_at
        );
        match probe_one(&m.boc, verbose) {
            Ok(root_hash) => {
                let root_hex = hex::encode(root_hash);
                let gql_id_norm = m.id.trim_start_matches("0x").to_lowercase();
                if !gql_id_norm.is_empty() && gql_id_norm != root_hex {
                    eprintln!(
                        "  IDENTITY MISMATCH: gql.id={gql_id_norm} != recomputed root={root_hex}"
                    );
                    // Descriptor parity already passed for every cell above,
                    // so this is a schema-level drift (GQL `id` no longer
                    // means "root repr_hash") rather than a wire drift.
                    // Report loudly but keep going — the operator needs the
                    // full picture across all `limit` messages.
                    fail.get_or_insert(2);
                } else {
                    println!("  OK — root repr_hash = {root_hex}");
                }
            },
            Err(e) => {
                eprintln!("  FAIL — {e}");
                let code = e.exit_code();
                fail = Some(fail.map(|f| f.max(code)).unwrap_or(code));
            },
        }
    }

    match fail {
        None => {
            println!("\n[probe_tvm_decode] ALL {} message(s) OK.", msgs.len());
            ExitCode::SUCCESS
        },
        Some(c) => {
            eprintln!(
                "\n[probe_tvm_decode] {} message(s) probed, at least one failed (worst exit={c}).",
                msgs.len()
            );
            ExitCode::from(c)
        },
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[probe_tvm_decode] {e:#}\nrun with --help for usage.");
            return ExitCode::from(3);
        },
    };

    match args.mode {
        Mode::Local { boc_b64 } => run_local(&boc_b64, args.verbose).await,
        Mode::Gql {
            url,
            account_id,
            dapp_id,
            limit,
        } => run_gql(&url, &account_id, &dapp_id, limit, args.verbose).await,
    }
}
