// Verify the integrity of trusted setup files
//
// This utility checks:
// 1. File exists and has correct size
// 2. SHA256 checksum matches expected value
// 3. File can be read and parsed
//
// Usage:
//   cargo run --example verify_trusted_setup

use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Trusted Setup Verification                                  ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Verify .ptau file
    verify_ptau_file()?;

    // Verify .srs file if it exists
    verify_srs_file()?;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  ✅ All verifications passed!                                ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    Ok(())
}

fn verify_ptau_file() -> Result<(), Box<dyn std::error::Error>> {
    println!("Verifying .ptau file...\n");

    let path = Path::new("trusted_setup/powersOfTau28_hez_final_18.ptau");
    let expected_size = 302_072_984; // ~288 MB (actual size from Hermez ceremony)
    let expected_checksum = "e970efa7774da80101e0ac336d083ef3339855c98112539338d706b2b89ac694";

    // Check file exists
    if !path.exists() {
        println!("  ❌ File not found: {:?}", path);
        println!("\n  To download:");
        println!("    cd deposit-prover");
        println!(
            "    wget https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_18.ptau \\"
        );
        println!("      -O trusted_setup/powersOfTau28_hez_final_18.ptau");
        return Err("File not found".into());
    }
    println!("  ✅ File exists: {:?}", path);

    // Check file size
    let metadata = std::fs::metadata(path)?;
    let actual_size = metadata.len();
    if actual_size != expected_size {
        println!("  ❌ File size mismatch!");
        println!("     Expected: {} bytes", expected_size);
        println!("     Actual:   {} bytes", actual_size);
        return Err("File size mismatch".into());
    }
    println!("  ✅ File size: {} bytes (288 MB)", actual_size);

    // Calculate SHA256 checksum
    println!("  ⏳ Calculating SHA256 checksum (this may take a minute)...");
    let actual_checksum = calculate_sha256(path)?;

    if actual_checksum != expected_checksum {
        println!("  ❌ Checksum mismatch!");
        println!("     Expected: {}", expected_checksum);
        println!("     Actual:   {}", actual_checksum);
        println!("\n  ⚠️  WARNING: File may be corrupted or tampered with!");
        println!("     Re-download from official source:");
        println!("     https://storage.googleapis.com/zkevm/ptau/powersOfTau28_hez_final_18.ptau");
        return Err("Checksum mismatch".into());
    }
    println!("  ✅ SHA256 checksum: {}", actual_checksum);

    println!("\n  ✅ .ptau file verification successful!");

    Ok(())
}

fn verify_srs_file() -> Result<(), Box<dyn std::error::Error>> {
    println!("\nVerifying .srs file...\n");

    let path = Path::new("data/kzg_params_18.srs");

    // Check if file exists
    if !path.exists() {
        println!("  ℹ️  File not found: {:?}", path);
        println!("     This is expected if you haven't generated/converted params yet.");
        println!("     See TRUSTED_SETUP.md for instructions.");
        return Ok(());
    }
    println!("  ✅ File exists: {:?}", path);

    // Check file size (should be ~2.6 GB for k=18)
    let metadata = std::fs::metadata(path)?;
    let actual_size = metadata.len();
    let expected_min_size = 2_000_000_000; // ~2 GB minimum
    let expected_max_size = 3_000_000_000; // ~3 GB maximum

    if actual_size < expected_min_size || actual_size > expected_max_size {
        println!("  ⚠️  File size unexpected!");
        println!("     Expected: ~2.6 GB (between 2-3 GB)");
        println!(
            "     Actual:   {} bytes ({:.2} GB)",
            actual_size,
            actual_size as f64 / 1e9
        );
        println!("     This may indicate the file is corrupted.");
    } else {
        println!(
            "  ✅ File size: {} bytes ({:.2} GB)",
            actual_size,
            actual_size as f64 / 1e9
        );
    }

    // Try to load the file to verify it's valid
    println!("  ⏳ Attempting to load params (this may take a minute)...");
    match load_and_verify_srs(path) {
        Ok(()) => println!("  ✅ .srs file is valid and can be loaded"),
        Err(e) => {
            println!("  ❌ Failed to load .srs file: {}", e);
            println!("     The file may be corrupted or in the wrong format.");
            return Err(e);
        },
    }

    println!("\n  ✅ .srs file verification successful!");

    Ok(())
}

fn calculate_sha256(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

fn load_and_verify_srs(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use halo2_base::halo2_proofs::{
        halo2curves::bn256::Bn256,
        poly::{commitment::Params, kzg::commitment::ParamsKZG},
    };

    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let params = ParamsKZG::<Bn256>::read(&mut reader)
        .map_err(|e| format!("Failed to read params: {}", e))?;

    // Verify basic properties
    let k = params.k();
    let n = params.n();

    if k == 0 {
        return Err("Invalid k=0".into());
    }

    if n != (1 << k) {
        return Err(format!("Invalid n: expected {}, got {}", 1 << k, n).into());
    }

    // Note: We can't directly access params.g as it's private
    // But if the file loaded successfully, it should be valid

    println!("     - k = {}", k);
    println!("     - n = {} (2^{})", n, k);

    Ok(())
}
