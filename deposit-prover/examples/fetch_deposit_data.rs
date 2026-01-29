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

use clap::Parser;
use deposit_prover::ethereum_fetcher::EthereumFetcher;
use ethers::types::{H160, H256};
use std::str::FromStr;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Get RPC URL from args or environment
    let rpc_url = args.rpc_url.or_else(|| std::env::var("ETH_RPC_URL").ok())
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
    let proof_input = fetcher
        .fetch_deposit_proof(tx_hash, contract, args.log_index)
        .await?;

    println!("\n✅ Deposit proof fetched successfully!\n");
    println!("Event Data:");
    println!("  Block Number: {}", proof_input.event_data.block_number);
    println!("  Transaction Index: {}", proof_input.event_data.transaction_index);
    println!("  Log Index: {}", proof_input.event_data.log_index);
    println!("  Deposit ID: {}", proof_input.event_data.deposit_id);
    println!("  Sender: 0x{}", hex::encode(proof_input.event_data.sender));
    println!("  Amount: {} wei", proof_input.event_data.amount);
    println!("  Timestamp: {}", proof_input.event_data.timestamp);
    println!(
        "  Contract: 0x{}",
        hex::encode(proof_input.event_data.contract_address)
    );
    println!();

    println!("Receipt Proof:");
    println!("  Receipt RLP: {} bytes", proof_input.receipt_proof.receipt_rlp.len());
    println!("  Proof Nodes: {}", proof_input.receipt_proof.proof_nodes.len());
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
    println!("   cargo run --example test_with_real_data -- --input {}", args.output);
    println!("3. Generate a proof:");
    println!("   cargo run --example generate_proof -- --input {}", args.output);

    Ok(())
}

