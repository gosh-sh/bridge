# Quick Start Guide - Acki Nacki Bridge

## For New Developers

### 1. Clone the Repository

```bash
git clone <repository-url>
cd acki-nacki-bridge
```

### 2. Download Trusted Setup (Required)

⚠️ **Important**: The trusted setup files are NOT in git due to their size (~288 MB).

```bash
cd deposit-prover
./download_trusted_setup.sh
```

This will download and verify the KZG parameters from the Perpetual Powers of Tau ceremony.

### 3. Build the Project

```bash
# Build Rust components
cd deposit-prover
cargo build --release

# Build Solidity contracts
cd ../contracts/ethereum
forge build
```

### 4. Run Tests

```bash
# Rust tests
cd deposit-prover
cargo test --lib

# Solidity tests
cd ../contracts/ethereum
forge test -vv

# E2E test (requires Sepolia RPC URL)
cd ../..
./test_e2e.sh
```

## Project Structure

```
acki-nacki-bridge/
├── deposit-prover/              # ZK proof generator (Rust)
│   ├── download_trusted_setup.sh  # Download script for trusted setup
│   ├── trusted_setup/             # Trusted setup files (NOT in git)
│   │   ├── README.md              # Documentation
│   │   └── .gitkeep               # Preserve directory
│   ├── examples/                  # Example utilities
│   └── src/                       # Circuit implementation
├── contracts/ethereum/          # Solidity contracts
│   └── src/                     # Bridge and verifier contracts
└── test_e2e.sh                  # End-to-end test script
```

## Common Tasks

### Generate a Proof

```bash
cd deposit-prover
cargo run --release --example test_with_real_data \
  --input <deposit_data.json> \
  --generate-proof
```

### Verify Trusted Setup

```bash
cd deposit-prover
cargo run --release --example verify_trusted_setup
```

### Deploy Contracts

```bash
cd contracts/ethereum
forge script script/Deploy.s.sol --rpc-url <RPC_URL> --broadcast
```

## Important Notes

### Security

✅ **Production-safe by default**

- The prover **requires** trusted setup parameters
- Random parameter generation has been **permanently disabled**
- The prover will fail with clear instructions if parameters are not found
- No feature flags needed - secure by default

See [TRUSTED_SETUP.md](deposit-prover/TRUSTED_SETUP.md) for details.

### Git and Large Files

The following files are **NOT** tracked in git:

- `deposit-prover/trusted_setup/*.ptau` (288 MB)
- `deposit-prover/data/*.pk` (proving keys)
- `deposit-prover/data/*.srs` (KZG parameters)
- `e2e_test_data/` (test artifacts)

Always run `./download_trusted_setup.sh` after cloning!

## Troubleshooting

### "File not found: trusted_setup/powersOfTau28_hez_final_18.ptau"

Run the download script:

```bash
cd deposit-prover
./download_trusted_setup.sh
```

### "Checksum mismatch"

The download may be corrupted. Delete and re-download:

```bash
cd deposit-prover
rm trusted_setup/powersOfTau28_hez_final_18.ptau
./download_trusted_setup.sh
```

### "Proving key corrupted"

Delete the old proving key and regenerate:

```bash
cd deposit-prover
rm data/deposit_prover_k18.pk
cargo run --release --example generate_verifier
```

## Documentation

- [TRUSTED_SETUP.md](deposit-prover/TRUSTED_SETUP.md) - Trusted setup guide
- [SECURITY_AUDIT_TRUSTED_SETUP.md](SECURITY_AUDIT_TRUSTED_SETUP.md) - Security audit
- [TRUSTED_SETUP_IMPLEMENTATION_SUMMARY.md](TRUSTED_SETUP_IMPLEMENTATION_SUMMARY.md) - Implementation details
- [deposit-prover/README.md](deposit-prover/README.md) - Prover documentation
- [contracts/ethereum/README.md](contracts/ethereum/README.md) - Contract documentation

## Getting Help

If you encounter issues:

1. Check the documentation above
2. Verify trusted setup is downloaded: `ls -lh deposit-prover/trusted_setup/`
3. Run verification: `cargo run --release --example verify_trusted_setup`
4. Check git status: `git status --ignored`

## Next Steps

1. Read [TRUSTED_SETUP.md](deposit-prover/TRUSTED_SETUP.md) to understand the security model
2. Run the E2E test to verify everything works
3. Review the circuit implementation in `deposit-prover/src/circuit_v2.rs`
4. Explore the Solidity contracts in `contracts/ethereum/src/`

Happy hacking! 🚀
