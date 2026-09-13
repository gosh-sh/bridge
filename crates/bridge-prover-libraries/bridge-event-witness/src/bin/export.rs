//! Standalone exporter binary.
//!
//! Mode 1 (current): consume a base64-encoded event BOC + block-level
//!   context from CLI flags. Useful for hermetic testing and for the Python
//!   orchestration driver that already knows the block context.
//!
//! Mode 2 (future, when the daemon lands): fetch the event message by tx
//!   hash via GraphQL and self-discover the block context. Will be added
//!   alongside Track D.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;

use bridge_event_witness::{
    export_from_event_boc_base64, BlockContextInput,
};

#[derive(Parser, Debug)]
#[command(
    name = "bridge-event-private-witness-export",
    about = "Export Circuit-4 private witness JSON for a WithdrawalInitiated event"
)]
struct Args {
    /// Base64-encoded ExtOut `Message` BOC.
    #[arg(long)]
    event_boc_b64: String,

    /// Hex-encoded block id (64 chars, no leading 0x). Block containing the event.
    #[arg(long)]
    block_id: String,

    /// Block sequence number.
    #[arg(long)]
    block_seq_no: u64,

    /// Hex-encoded account dApp id (64 chars).
    #[arg(long)]
    account_dapp_id: String,

    /// Hex-encoded account id of the emitting account (64 chars).
    #[arg(long)]
    account_id: String,

    /// Hex-encoded envelope hash for the block (64 chars).
    #[arg(long)]
    envelope_hash: String,

    /// Output JSON path.
    #[arg(long)]
    out: PathBuf,
}

fn parse_hex32(label: &str, s: &str) -> Result<[u8; 32]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).with_context(|| format!("{label} is not valid hex"))?;
    if bytes.len() != 32 {
        bail!("{label} must decode to 32 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    let ctx = BlockContextInput {
        block_id: parse_hex32("--block-id", &args.block_id)?,
        block_seq_no: args.block_seq_no,
        account_dapp_id: parse_hex32("--account-dapp-id", &args.account_dapp_id)?,
        account_id: parse_hex32("--account-id", &args.account_id)?,
        envelope_hash: parse_hex32("--envelope-hash", &args.envelope_hash)?,
    };

    let witness = export_from_event_boc_base64(&args.event_boc_b64, &ctx)
        .context("export failed")?;

    let json = serde_json::to_string_pretty(&witness)
        .context("witness serialization failed")?;
    std::fs::write(&args.out, json)
        .with_context(|| format!("failed to write {}", args.out.display()))?;

    tracing::info!(
        "wrote private witness to {} (event_message_hash={}, token_id={})",
        args.out.display(),
        witness.event_message_hash_hex,
        witness.event.token_id,
    );
    Ok(())
}
