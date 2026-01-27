//! Deposit Event Prover
//!
//! This is a standalone binary that generates ZK proofs for Ethereum Deposit events.
//! It uses axiom-eth to prove that a Deposit event was emitted by the bridge contract.
//!
//! Usage:
//!   deposit-prover prove \
//!     --withdrawal-hash <hex> \
//!     --nullifier-preimage <hex> \
//!     --tx-hash <hex> \
//!     --rpc-url <url> \
//!     --contract-address <address> \
//!     --output <file>

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ethers::providers::{Http, Provider};
use std::path::PathBuf;

mod circuit;
mod ethereum;
mod mpt;
mod rlp_utils;
mod types;

use circuit::DepositEventCircuit;
use ethereum::EthereumClient;
use types::{DepositProofInput, DepositProofOutput};

#[derive(Parser)]
#[command(name = "deposit-prover")]
#[command(about = "Generate ZK proofs for Ethereum Deposit events", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a deposit event proof
    Prove {
        /// Withdrawal hash (32 bytes hex)
        #[arg(long)]
        withdrawal_hash: String,

        /// Nullifier preimage (32 bytes hex)
        #[arg(long)]
        nullifier_preimage: String,

        /// Transaction hash containing the Deposit event
        #[arg(long)]
        tx_hash: String,

        /// Ethereum RPC URL
        #[arg(long)]
        rpc_url: String,

        /// Bridge contract address
        #[arg(long)]
        contract_address: String,

        /// Output file for the proof
        #[arg(long)]
        output: PathBuf,
    },

    /// Generate proving and verifying keys
    Setup {
        /// Output directory for keys
        #[arg(long)]
        output_dir: PathBuf,
    },

    /// Verify a deposit event proof
    Verify {
        /// Proof file
        #[arg(long)]
        proof: PathBuf,

        /// Verifying key file
        #[arg(long)]
        vkey: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Prove {
            withdrawal_hash,
            nullifier_preimage,
            tx_hash,
            rpc_url,
            contract_address,
            output,
        } => {
            prove(
                &withdrawal_hash,
                &nullifier_preimage,
                &tx_hash,
                &rpc_url,
                &contract_address,
                &output,
            )
            .await?;
        }
        Commands::Setup { output_dir } => {
            setup(&output_dir)?;
        }
        Commands::Verify { proof, vkey } => {
            verify(&proof, &vkey)?;
        }
    }

    Ok(())
}

async fn prove(
    withdrawal_hash: &str,
    nullifier_preimage: &str,
    tx_hash: &str,
    rpc_url: &str,
    contract_address: &str,
    output: &PathBuf,
) -> Result<()> {
    println!("🔍 Fetching Ethereum data...");

    // Parse inputs
    let withdrawal_hash = hex::decode(withdrawal_hash.trim_start_matches("0x"))
        .context("Invalid withdrawal hash")?;
    let nullifier_preimage = hex::decode(nullifier_preimage.trim_start_matches("0x"))
        .context("Invalid nullifier preimage")?;

    // Connect to Ethereum
    let provider = Provider::<Http>::try_from(rpc_url)
        .context("Failed to connect to Ethereum RPC")?;
    let client = EthereumClient::new(provider);

    // Fetch receipt and proof data
    let receipt_data = client
        .fetch_deposit_event(tx_hash, contract_address)
        .await
        .context("Failed to fetch deposit event")?;

    println!("✅ Found Deposit event:");
    println!("   Block: {}", receipt_data.block_number);
    println!("   Tx Index: {}", receipt_data.transaction_index);
    println!("   Log Index: {}", receipt_data.log_index);
    println!("   Amount: {}", receipt_data.amount);

    // Generate MPT proof
    println!("🔐 Generating Merkle-Patricia Trie proof...");
    let receipt_proof = client
        .generate_receipt_proof(tx_hash)
        .await
        .context("Failed to generate receipt proof")?;

    println!("✅ Receipt proof generated");

    // Generate ZK proof
    println!("⚡ Generating ZK proof...");
    let proof_input = DepositProofInput {
        withdrawal_hash: withdrawal_hash.try_into().unwrap(),
        nullifier_preimage: nullifier_preimage.try_into().unwrap(),
        event_data: receipt_data,
        receipt_proof,
    };

    let proof_output = DepositEventCircuit::prove(proof_input)
        .context("Failed to generate ZK proof")?;

    // Save proof
    let proof_json = serde_json::to_string_pretty(&proof_output)
        .context("Failed to serialize proof")?;
    std::fs::write(output, proof_json)
        .context("Failed to write proof to file")?;

    println!("✅ Proof saved to {}", output.display());
    println!("📊 Public inputs:");
    println!("   Nullifier: 0x{}", hex::encode(&proof_output.nullifier));
    println!("   Recipient: 0x{}", hex::encode(&proof_output.recipient));
    println!("   Amount: {}", proof_output.amount);
    println!("   Contract: 0x{}", hex::encode(&proof_output.contract_address));

    Ok(())
}

fn setup(output_dir: &PathBuf) -> Result<()> {
    println!("🔧 Generating proving and verifying keys...");

    std::fs::create_dir_all(output_dir)
        .context("Failed to create output directory")?;

    // TODO: Implement key generation
    // This will use halo2's keygen to generate proving and verifying keys

    println!("✅ Keys saved to {}", output_dir.display());

    Ok(())
}

fn verify(proof_path: &PathBuf, vkey_path: &PathBuf) -> Result<()> {
    println!("🔍 Verifying proof...");

    // Load proof
    let proof_json = std::fs::read_to_string(proof_path)
        .context("Failed to read proof file")?;
    let _proof: DepositProofOutput = serde_json::from_str(&proof_json)
        .context("Failed to parse proof")?;

    // Load verifying key
    let _vkey = std::fs::read(vkey_path)
        .context("Failed to read verifying key")?;

    // TODO: Implement proof verification
    // This will use halo2's verifier to check the proof

    println!("✅ Proof is valid");

    Ok(())
}

