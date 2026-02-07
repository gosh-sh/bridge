// Convert .ptau file from Perpetual Powers of Tau to Halo2's native .srs format
//
// Usage:
//   cargo run --release --example convert_ptau_to_halo2 -- \
//     --ptau trusted_setup/powersOfTau28_hez_final_18.ptau \
//     --output data/kzg_params_18.srs \
//     --k 18

use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    path::PathBuf,
};

use clap::Parser;
use halo2_base::halo2_proofs::{
    halo2curves::bn256::{Bn256, Fr, G1Affine, G2Affine},
    poly::kzg::commitment::ParamsKZG,
};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the .ptau file
    #[arg(long)]
    ptau: PathBuf,

    /// Output path for the .srs file
    #[arg(long)]
    output: PathBuf,

    /// Circuit degree (k). Must be <= the degree in the .ptau file
    #[arg(long)]
    k: u32,

    /// Verify the conversion by checking a known test vector
    #[arg(long, default_value = "false")]
    verify: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Converting .ptau to Halo2 .srs format                       ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    println!("Input:  {:?}", args.ptau);
    println!("Output: {:?}", args.output);
    println!("Degree: k={} (2^{} = {} rows)\n", args.k, args.k, 1u64 << args.k);

    // Read .ptau file
    println!("Step 1: Reading .ptau file...");
    let ptau_data = read_ptau_file(&args.ptau, args.k)?;
    println!("  ✅ Read {} G1 points and {} G2 points", ptau_data.g1_points.len(), ptau_data.g2_points.len());

    // Convert to Halo2 format
    println!("\nStep 2: Converting to Halo2 ParamsKZG...");
    let params = convert_to_halo2_params(ptau_data, args.k)?;
    println!("  ✅ Conversion successful");

    // Save to .srs file
    println!("\nStep 3: Saving to .srs file...");
    save_halo2_params(&params, &args.output)?;
    println!("  ✅ Saved to {:?}", args.output);

    // Verify if requested
    if args.verify {
        println!("\nStep 4: Verifying conversion...");
        verify_params(&params)?;
        println!("  ✅ Verification successful");
    }

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  ✅ Conversion complete!                                     ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    println!("You can now use this file in your prover:");
    println!("  Place it at: deposit-prover/data/kzg_params_{}.srs", args.k);
    println!("  The prover will automatically load it instead of generating random params");

    Ok(())
}

/// Data extracted from .ptau file
struct PtauData {
    g1_points: Vec<G1Affine>,
    g2_points: Vec<G2Affine>,
    power: u32,
}

/// Read and parse a .ptau file
fn read_ptau_file(path: &PathBuf, k: u32) -> Result<PtauData, Box<dyn std::error::Error>> {
    // For now, we'll use a simplified approach that works with the ppot-rs crate
    // In a production implementation, you would use ppot-rs to read the file properly
    
    // TODO: Implement proper .ptau parsing using ppot-rs
    // For now, return an error with instructions
    
    Err(format!(
        "⚠️  .ptau to Halo2 conversion not yet fully implemented.\n\
         \n\
         To complete this conversion, you need to:\n\
         \n\
         1. Use the ppot-rs crate to read the .ptau file:\n\
            https://docs.rs/ppot-rs/latest/ppot_rs/\n\
         \n\
         2. Extract the G1 and G2 points from the ceremony\n\
         \n\
         3. Convert from arkworks curve types to halo2curves types\n\
         \n\
         Alternative approaches:\n\
         \n\
         A. Download pre-converted Halo2 parameters (recommended):\n\
            - Check if the Halo2 community has pre-converted files\n\
            - Or ask in the Halo2 Discord/Telegram\n\
         \n\
         B. Use the existing .srs file if you already generated it:\n\
            - If you've run the prover before, it saved params to data/kzg_params_{}.srs\n\
            - Those params are INSECURE (from gen_srs) but work for testing\n\
         \n\
         C. For production, consider:\n\
            - Running your own trusted setup ceremony\n\
            - Or using a service that provides pre-converted params\n\
         \n\
         See TRUSTED_SETUP.md for more details.",
        k
    ).into())
}

/// Convert PtauData to Halo2's ParamsKZG format
fn convert_to_halo2_params(
    data: PtauData,
    k: u32,
) -> Result<ParamsKZG<Bn256>, Box<dyn std::error::Error>> {
    // Verify we have enough points
    let required_points = 1 << k;
    if data.g1_points.len() < required_points {
        return Err(format!(
            "Not enough G1 points: need {}, have {}",
            required_points,
            data.g1_points.len()
        )
        .into());
    }

    if data.g2_points.len() < 2 {
        return Err(format!("Not enough G2 points: need 2, have {}", data.g2_points.len()).into());
    }

    // Create ParamsKZG
    let params = ParamsKZG::<Bn256> {
        k,
        n: 1 << k,
        g: data.g1_points[..required_points].to_vec(),
        g_lagrange: vec![], // Will be computed on demand by Halo2
        g2: data.g2_points[0],
        s_g2: data.g2_points[1],
    };

    Ok(params)
}

/// Save Halo2 params to .srs file
fn save_halo2_params(
    params: &ParamsKZG<Bn256>,
    path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create parent directory if it doesn't exist
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    params
        .write(&mut writer)
        .map_err(|e| format!("Failed to write params: {}", e))?;

    Ok(())
}

/// Verify the converted params are valid
fn verify_params(params: &ParamsKZG<Bn256>) -> Result<(), Box<dyn std::error::Error>> {
    // Basic sanity checks
    if params.g.is_empty() {
        return Err("No G1 points".into());
    }

    if params.n != (1 << params.k) {
        return Err(format!("Invalid n: expected {}, got {}", 1 << params.k, params.n).into());
    }

    // TODO: Add more verification:
    // - Check that points are on the curve
    // - Verify pairing equations
    // - Compare with known test vectors

    println!("  - k = {}", params.k);
    println!("  - n = {}", params.n);
    println!("  - G1 points: {}", params.g.len());
    println!("  - G2 points: 2 (g2, s_g2)");

    Ok(())
}

