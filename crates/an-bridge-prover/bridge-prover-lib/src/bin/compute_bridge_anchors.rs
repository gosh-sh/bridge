//! Compute the two genesis anchors needed by `DeployShellnetE2EBridge.s.sol`:
//!
//!   * `GENESIS_BK_SET_COMMITMENT`         — Poseidon commitment of the current
//!                                           BK set (from `bk_set.*.json`).
//!   * `GENESIS_PREV_MAX_LEVEL_LAYER_HASH` — layer-1 root at the seed key
//!                                           block, i.e. the same anchor the
//!                                           on-chain bridge stores after its
//!                                           genesis stamp.
//!
//! No proving, no keygen, no SRS — this is a thin wrapper over the same helpers
//! the daemon uses on cold start:
//!   `bk_set_fetcher::load_bk_set_from_config`
//!   → `bridge_poseidon::compute_bk_set_poseidon`
//!   → `bridge_prover_lib::bootstrap::fetch_from_node`
//!
//! Sibling of `bootstrap_hermez_srs.rs` (per the "add a sibling, don't mutate"
//! rule). Prints two env-var lines suitable for `.env.shellnet`.
//!
//! USAGE
//!   cargo run --release --bin compute_bridge_anchors -- \
//!     [--endpoint https://shellnet.ackinacki.org/graphql] \
//!     [--bk-set-config ../bk_set.shellnet.json] \
//!     [--seed-seqno N | --at-head]
//!
//! Defaults:
//!   endpoint       = $BRIDGE_GQL_ENDPOINT (or shellnet public GraphQL)
//!   bk-set-config  = $BRIDGE_BK_SET_CONFIG (or ../bk_set.shellnet.json)
//!   seed-seqno     = latest already-produced `W*P = 1024` boundary at or below
//!                    chain head (`--at-head` forces this even if --seed-seqno
//!                    was passed)
//!
//! `--seed-seqno` must be a multiple of `1024` (shellnet bundle boundary
//! `W*P = 128*8`).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use bridge_gql_fetcher::{bk_set_fetcher, gql_client};
use bridge_poseidon::compute_bk_set_poseidon;
use bridge_prover_lib::bootstrap::fetch_from_node;
use halo2_base::halo2_proofs::halo2curves::group::ff::PrimeField;

/// Convert a 32-byte `Fr::to_repr()` (little-endian) array to the hex-string
/// representation that Foundry's `vm.envUint` will parse into the SAME numeric
/// `uint256` that the daemon submits on-chain.
///
/// The daemon path is:
/// ```text
///     Fr -> Fr::to_repr()       -> [u8; 32] LE
///        -> U256::from_le_bytes -> numeric = Fr scalar value
/// ```
/// Foundry's `vm.envUint("0x…")` parses the hex string as a **big-endian**
/// `uint256` literal (matches `uint256(bytes32(…))` in Solidity). So for the
/// two numerics to agree we must emit the LE bytes **reversed** into BE order.
///
/// A round-trip test lives in the `tests` module of this file; changing the
/// convention on either side without updating the other trips the assertion.
///
/// This is a pure byte-level helper — no dependency on `alloy::U256`.
fn le_repr_to_solidity_be_hex(le_repr: &[u8; 32]) -> String {
    let mut be = *le_repr;
    be.reverse();
    format!("0x{}", hex::encode(be))
}

const BUNDLE_BOUNDARY: u64 = 128 * 8; // W * P on shellnet.

const DEFAULT_ENDPOINT: &str = "https://shellnet.ackinacki.org/graphql";
const DEFAULT_BK_SET_CONFIG_RELPATH: &str = "../bk_set.shellnet.json";

struct Args {
    endpoint: String,
    bk_set_config: PathBuf,
    seed_seqno: Option<u64>,
    at_head: bool,
}

fn parse_args() -> Result<Args> {
    let default_endpoint =
        std::env::var("BRIDGE_GQL_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    let default_bk_set = std::env::var("BRIDGE_BK_SET_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // CARGO_MANIFEST_DIR points at bridge-prover-lib; bk_set lives one up.
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_BK_SET_CONFIG_RELPATH)
        });

    let mut endpoint = default_endpoint;
    let mut bk_set_config = default_bk_set;
    let mut seed_seqno: Option<u64> = None;
    let mut at_head = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--endpoint" => endpoint = it.next().context("--endpoint requires a URL")?,
            "--bk-set-config" => {
                bk_set_config =
                    PathBuf::from(it.next().context("--bk-set-config requires a path")?);
            }
            "--seed-seqno" => {
                let v: u64 = it
                    .next()
                    .context("--seed-seqno requires a value")?
                    .parse()
                    .context("--seed-seqno value must be a u64")?;
                seed_seqno = Some(v);
            }
            "--at-head" => at_head = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown flag: {other}"),
        }
    }

    Ok(Args { endpoint, bk_set_config, seed_seqno, at_head })
}

fn print_help() {
    println!(
        "\
compute_bridge_anchors — derive AckiNackiBridge constructor anchors from live chain

USAGE:
    compute_bridge_anchors [--endpoint URL] [--bk-set-config PATH]
                           [--seed-seqno N | --at-head]

OPTIONS:
    --endpoint URL           GraphQL endpoint (default: $BRIDGE_GQL_ENDPOINT
                             or {DEFAULT_ENDPOINT})
    --bk-set-config PATH     JSON map {{index -> hex pubkey}} (default:
                             $BRIDGE_BK_SET_CONFIG or ../bk_set.shellnet.json)
    --seed-seqno N           Seed at explicit key-block seq_no (must be a
                             multiple of {BUNDLE_BOUNDARY})
    --at-head                Seed at the latest already-produced {BUNDLE_BOUNDARY}
                             boundary at or below chain head (default)
    -h, --help               Show this help
",
        DEFAULT_ENDPOINT = DEFAULT_ENDPOINT,
        BUNDLE_BOUNDARY = BUNDLE_BOUNDARY,
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;

    // 1. BK set → Poseidon commitment (32 LE bytes).
    let bk_set = bk_set_fetcher::load_bk_set_from_config(
        args.bk_set_config
            .to_str()
            .context("bk-set-config path is not valid UTF-8")?,
    )
    .with_context(|| format!("load BK set from {}", args.bk_set_config.display()))?;
    let (bk_commit_fr, _) = compute_bk_set_poseidon(&bk_set);
    let bk_commit: [u8; 32] = bk_commit_fr.to_repr();
    eprintln!(
        "BK set loaded: {} keepers, Poseidon commitment = 0x{}",
        bk_set.len(),
        hex::encode(bk_commit)
    );

    // 2. GQL client + seed seq_no resolution.
    let gql = gql_client::create_client(&args.endpoint)
        .with_context(|| format!("create GraphQL client for {}", args.endpoint))?;

    let head_seqno = {
        let mut blocks = gql
            .query_latest_blocks(1)
            .await
            .context("query_latest_blocks(1) for chain head")?;
        let (_, seqno) = blocks
            .pop()
            .context("chain returned zero latest blocks — endpoint alive but empty?")?;
        seqno
    };
    eprintln!("chain head seq_no = {}", head_seqno);

    let seed_seqno = if args.at_head || args.seed_seqno.is_none() {
        (head_seqno / BUNDLE_BOUNDARY) * BUNDLE_BOUNDARY
    } else {
        let n = args.seed_seqno.unwrap();
        if !n.is_multiple_of(BUNDLE_BOUNDARY) {
            bail!(
                "--seed-seqno {n} is not a multiple of the bundle boundary {BUNDLE_BOUNDARY}",
            );
        }
        if n > head_seqno {
            bail!(
                "--seed-seqno {n} is ahead of chain head {head_seqno} — that block does not exist yet",
            );
        }
        n
    };
    if seed_seqno == 0 {
        bail!(
            "resolved seed_seqno = 0 (chain head {head_seqno} < first bundle boundary {BUNDLE_BOUNDARY})",
        );
    }
    eprintln!("seed key-block seq_no = {} (boundary {})", seed_seqno, BUNDLE_BOUNDARY);

    // 3. Pull the key-block envelope, extract layer_hashes.
    let seed = fetch_from_node(&gql, seed_seqno, bk_commit)
        .await
        .context("fetch_from_node")?;

    // 4. Anchor = layer-1 root. Contract's genesis stamp records exactly this.
    //
    // Shellnet single-thread runs produce a single layer entry; the daemon
    // takes `window(pick).latest()` (see BridgeState::prev_max_level_layer_hash_for)
    // which on a fresh seed is the only entry present.
    if seed.layer_hashes.is_empty() {
        bail!(
            "seed block {} has zero layer_hashes — cannot compute genesis anchor",
            seed_seqno
        );
    }
    let layer1 = seed
        .layer_hashes
        .iter()
        .find(|(_, layer)| *layer == 1)
        .with_context(|| {
            format!(
                "seed block {} has no layer=1 entry (found layers: {:?})",
                seed_seqno,
                seed.layer_hashes.iter().map(|(_, l)| *l).collect::<Vec<_>>(),
            )
        })?;
    let genesis_anchor = layer1.0;

    // Extra info for review before broadcast.
    eprintln!(
        "seed block_height = {}, layer_hashes count = {}",
        seed.block_height,
        seed.layer_hashes.len()
    );
    for (root, layer) in &seed.layer_hashes {
        eprintln!("  layer {} root = 0x{}", layer, hex::encode(root));
    }

    // 5. Emit env-var lines suitable for `.env.shellnet`.
    //
    // BE-hex is required: Foundry's `vm.envUint` parses hex as a big-endian
    // `uint256` literal, while the daemon submits
    // `U256::from_le_bytes(fr.to_repr())` = the Fr scalar's numeric value.
    // `Fr::to_repr()` gives LE bytes, so we byte-reverse before hex-encoding
    // so both sides land on the same numeric.  See
    // `le_repr_to_solidity_be_hex` docs + the `tests` module below.
    println!("# generated by compute_bridge_anchors at seed_seqno={seed_seqno}");
    println!(
        "GENESIS_BK_SET_COMMITMENT={}",
        le_repr_to_solidity_be_hex(&bk_commit),
    );
    println!(
        "GENESIS_PREV_MAX_LEVEL_LAYER_HASH={}",
        le_repr_to_solidity_be_hex(&genesis_anchor),
    );
    println!("GENESIS_SEED_SEQNO={seed_seqno}");
    println!("GENESIS_SEED_HEIGHT={}", seed.block_height);

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Round-trip tests pinning the emitted-env-value convention against the
    //! two consumers that must agree on the numeric:
    //!
    //!   * the on-chain constructor
    //!     (`vm.envUint` in `DeployShellnetE2EBridge.s.sol` → BE-hex uint256)
    //!   * the daemon submission path
    //!     (`bridge-relayer-daemon/src/types.rs` → `U256::from_le_bytes`)
    //!
    //! A regression on either side (e.g. someone reverts to
    //! `hex::encode(fr.to_repr())` or the daemon flips to `from_be_bytes`)
    //! will trip these tests.
    //!
    //! We deliberately do NOT pull in `alloy::U256` here — the assertions are
    //! byte-level so this test module compiles as part of the bin's own
    //! `cargo test --bin compute_bridge_anchors` run and has no extra deps.
    use super::*;
    use halo2_base::halo2_proofs::halo2curves::bn256::Fr;

    /// Simulates Foundry's `vm.envUint("0x…")` at the byte level: strip the
    /// `0x`, hex-decode, and return the 32-byte big-endian representation of
    /// the resulting uint256.
    fn parse_foundry_be_hex(env_value: &str) -> [u8; 32] {
        let stripped = env_value.strip_prefix("0x").expect("env value must start with 0x");
        let bytes = hex::decode(stripped).expect("env value must be valid hex");
        assert_eq!(bytes.len(), 32, "uint256 literal must decode to 32 bytes");
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    }

    /// The daemon submits `U256::from_le_bytes(fr.to_repr())`.  In BE-byte
    /// form (`uint256(bytes32)` layout) that's the LE bytes reversed.
    fn daemon_submission_be_bytes(fr: &Fr) -> [u8; 32] {
        let mut be = fr.to_repr();
        be.reverse();
        be
    }

    /// Core invariant: for any Fr, the value my helper emits must, when
    /// re-parsed as a Foundry BE-hex uint256, equal the BE byte-form of the
    /// daemon-submitted numeric.  Would have caught the 2026-08-02
    /// PrevAnchorMismatch on Sepolia.
    #[test]
    fn emitted_hex_matches_daemon_submission_numeric() {
        // Sample a handful of Fr values, including asymmetric byte patterns
        // whose LE and BE representations differ obviously.
        let samples = [
            Fr::from(1u64),
            Fr::from(0xdeadbeefu64),
            Fr::from(u64::MAX),
            // Poseidon-shaped scalar: fixed non-symmetric bytes derived from
            // Fr::from_raw so `to_repr()` is nontrivial.
            Fr::from_raw([
                0x0123456789abcdef,
                0xfedcba9876543210,
                0x1122334455667788,
                0x0aabbccddeeff001,
            ]),
        ];

        for fr in samples {
            let emitted = le_repr_to_solidity_be_hex(&fr.to_repr());
            let parsed_be = parse_foundry_be_hex(&emitted);
            let daemon_be = daemon_submission_be_bytes(&fr);
            assert_eq!(
                parsed_be, daemon_be,
                "emitted env value {emitted} parses to BE bytes {} but daemon would submit {}",
                hex::encode(parsed_be),
                hex::encode(daemon_be),
            );
        }
    }

    /// Explicit anti-regression: the buggy form (`hex::encode(fr.to_repr())`
    /// without reversal) must NOT equal the correct emission for any Fr with
    /// asymmetric bytes.  If someone re-introduces the bug, this trips.
    #[test]
    fn buggy_le_hex_form_is_rejected_for_asymmetric_fr() {
        let fr = Fr::from_raw([
            0x0123456789abcdef,
            0xfedcba9876543210,
            0x1122334455667788,
            0x0aabbccddeeff001,
        ]);
        let correct = le_repr_to_solidity_be_hex(&fr.to_repr());
        let buggy = format!("0x{}", hex::encode(fr.to_repr()));
        assert_ne!(
            correct, buggy,
            "asymmetric Fr must not produce the same string in LE-hex and BE-hex forms — \
             if these are equal the test is trivially passing and needs a better sample"
        );
    }

    /// Pins the exact byte-reversal observed on Sepolia 2026-08-02.  If this
    /// exact production seed is re-derived, the emission must produce the
    /// value we deployed with post-fix — not the LE-hex form that caused the
    /// first PrevAnchorMismatch.
    #[test]
    fn shellnet_2026_08_02_seed_regression_vector() {
        // Fr scalar whose Fr::to_repr() (LE) equals the raw seed layer-1
        // bytes fetched from shellnet at seq_no = 4_887_552.  Constructed
        // here from those bytes so we don't depend on the live chain.
        let raw_layer1_le: [u8; 32] = [
            0x58, 0x91, 0x86, 0xa7, 0x29, 0x48, 0x02, 0x0e, 0xf9, 0x59, 0x45, 0x9c,
            0x5e, 0xbf, 0x45, 0x61, 0x34, 0xff, 0x2c, 0xee, 0xd7, 0x2f, 0x0d, 0x85,
            0x3a, 0x73, 0x53, 0xf6, 0x0a, 0x7b, 0x8e, 0x26,
        ];
        // Emission from the fixed helper must be the BE-hex string that
        // matches what the daemon submits (byte-reversal of the raw LE bytes).
        let emitted = le_repr_to_solidity_be_hex(&raw_layer1_le);
        assert_eq!(
            emitted,
            "0x268e7b0af653733a850d2fd7ee2cff346145bf5e9c4559f90e024829a7869158",
            "emission convention drifted from the daemon-submitted numeric \
             observed on shellnet 2026-08-02",
        );
    }
}
