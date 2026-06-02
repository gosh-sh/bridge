//! Fetch Deposit Data from Ethereum
//!
//! This tool fetches real deposit event data from Ethereum for testing.
//!
//! Usage:
//!   cargo run --example fetch_deposit_data -- \
//!     --rpc-url https://eth-sepolia.g.alchemy.com/v2/YOUR_KEY \
//!     --tx-hash 0x... \
//!     --contract 0x... \
//!     --log-index 0

use std::str::FromStr;

use clap::Parser;
use deposit_prover::ethereum_fetcher::EthereumFetcher;
use ethers::types::{H160, H256};

#[derive(Parser, Debug)]
#[command(name = "fetch-deposit-data")]
#[command(about = "Fetch deposit event data from Ethereum", long_about = None)]
struct Args {
    /// Ethereum RPC URL (or set ETH_RPC_URL environment variable)
    #[arg(long)]
    rpc_url: Option<String>,

    /// Transaction hash containing the Deposit event
    #[arg(long)]
    tx_hash: String,

    /// Bridge contract address
    #[arg(long)]
    contract: String,

    /// Log index in the transaction (default: 0)
    #[arg(long, default_value = "0")]
    log_index: usize,

    /// Output file for the proof input (JSON)
    #[arg(long, default_value = "deposit_proof_input.json")]
    output: String,

    /// Acki Nacki destination dApp identifier (UInt256), hex (with or without
    /// 0x), big-endian. Config-supplied tag bound as the dappId public inputs
    /// (not part of the Ethereum event). Defaults to zero.
    #[arg(long, default_value = "0")]
    dapp_id: String,
}

/// Parse a UInt256 hex/decimal string into a 32-byte big-endian array.
fn parse_dapp_id(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.trim();
    let bytes = if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let padded = format!("{:0>64}", hex_str);
        hex::decode(&padded).map_err(|e| anyhow::anyhow!("invalid --dapp-id hex: {e}"))?
    } else if s == "0" {
        vec![0u8; 32]
    } else {
        // Treat as hex without 0x prefix.
        let padded = format!("{:0>64}", s);
        hex::decode(&padded).map_err(|e| anyhow::anyhow!("invalid --dapp-id hex: {e}"))?
    };
    let mut out = [0u8; 32];
    if bytes.len() != 32 {
        anyhow::bail!("--dapp-id must be 32 bytes (64 hex chars), got {}", bytes.len());
    }
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Get RPC URL from args or environment
    let rpc_url = args
        .rpc_url
        .or_else(|| std::env::var("ETH_RPC_URL").ok())
        .expect("RPC URL must be provided via --rpc-url or ETH_RPC_URL environment variable");

    println!("=== Deposit Data Fetcher ===\n");
    println!("Configuration:");
    println!("  RPC URL: {}", rpc_url);
    println!("  Transaction: {}", args.tx_hash);
    println!("  Contract: {}", args.contract);
    println!("  Log Index: {}", args.log_index);
    println!("  Output: {}", args.output);
    println!();

    // Parse addresses
    let tx_hash = H256::from_str(&args.tx_hash)?;
    let contract = H160::from_str(&args.contract)?;

    // Create fetcher
    println!("Connecting to Ethereum...");
    let fetcher = EthereumFetcher::new(&rpc_url)?;
    println!("✓ Connected\n");

    // Fetch deposit proof
    println!("Fetching deposit proof...");
    let mut proof_input = fetcher
        .fetch_deposit_proof(tx_hash, contract, args.log_index)
        .await?;

    // dappId is a config tag (not part of the event); set it from the CLI.
    proof_input.dapp_id = parse_dapp_id(&args.dapp_id)?;
    println!("  dappId: 0x{}", hex::encode(proof_input.dapp_id));

    println!("\n✅ Deposit proof fetched successfully!\n");
    println!("Event Data:");
    println!("  Block Number: {}", proof_input.event_data.block_number);
    println!(
        "  Transaction Index: {}",
        proof_input.event_data.transaction_index
    );
    println!("  Log Index: {}", proof_input.event_data.log_index);
    println!("  Deposit ID: {}", proof_input.event_data.deposit_id);
    println!("  Sender: 0x{}", hex::encode(proof_input.event_data.sender));
    // FIX BC-TYPES-001: amount is now [u8; 32]
    println!("  Amount: 0x{}", hex::encode(proof_input.event_data.amount));
    println!("  Timestamp: {}", proof_input.event_data.timestamp);
    println!(
        "  Contract: 0x{}",
        hex::encode(proof_input.event_data.contract_address)
    );
    println!();

    println!("Receipt Proof:");
    println!(
        "  Receipt RLP: {} bytes",
        proof_input.receipt_proof.receipt_rlp.len()
    );
    println!(
        "  Proof Nodes: {}",
        proof_input.receipt_proof.proof_nodes.len()
    );
    println!(
        "  Receipt Root: 0x{}",
        hex::encode(proof_input.receipt_proof.receipt_root)
    );
    println!(
        "  Block Header RLP: {} bytes",
        proof_input.receipt_proof.block_header_rlp.len()
    );
    println!();

    // Save to file
    println!("Saving to {}...", args.output);
    let json = serde_json::to_string_pretty(&proof_input)?;
    std::fs::write(&args.output, json)?;
    println!("✓ Saved\n");

    println!("Next steps:");
    println!("1. Review the saved data in {}", args.output);
    println!("2. Use this data to test the circuit:");
    println!(
        "   cargo run --example test_with_real_data -- --input {}",
        args.output
    );
    println!("3. Generate a proof:");
    println!(
        "   cargo run --example generate_proof -- --input {}",
        args.output
    );

    Ok(())
}
