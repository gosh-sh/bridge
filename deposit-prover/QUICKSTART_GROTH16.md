# Quick Start: Groth16 Wrapper Setup

## What You Need to Do Now

I've created the installation scripts and documentation. Here's what you need to run:

### Step 1: Install Go and Dependencies

```bash
cd deposit-prover
sudo ./install_dependencies.sh
```

This will:
- Install Go compiler (1.21+)
- Install build-essential (for CGO)
- Verify installations

**Expected output:**
```
✓ Go installed successfully: go version go1.21.x linux/amd64
✓ All dependencies installed successfully!
```

### Step 2: Setup gnark Library

```bash
./setup_gnark.sh
```

This will:
- Create `gnark-wrapper/` directory
- Initialize Go module
- Install gnark and gnark-crypto libraries

**Expected output:**
```
✓ Go version: 1.21.x
✓ Go module initialized
✓ gnark setup completed successfully!
```

### Step 3: Verify Installation

```bash
cd gnark-wrapper
go version
go list -m github.com/consensys/gnark
```

**Expected output:**
```
go version go1.21.x linux/amd64
github.com/consensys/gnark v0.11.x
```

## What Happens Next

After you run these scripts, I will:

1. **Create the Groth16 verifier circuit** in Go (gnark-wrapper/circuit.go)
2. **Implement Halo2 proof parser** in Rust (src/groth16_wrapper/proof_parser.rs)
3. **Build Rust-Go FFI bridge** for data passing
4. **Generate Groth16 proofs** from your existing Halo2 proofs
5. **Export tiny Solidity verifier** (~1-2KB) for mainnet deployment

## Files Created

```
deposit-prover/
├── install_dependencies.sh     ✓ Created (run with sudo)
├── setup_gnark.sh              ✓ Created (run as user)
├── GROTH16_WRAPPER.md          ✓ Created (full documentation)
├── QUICKSTART_GROTH16.md       ✓ Created (this file)
└── README.md                   ✓ Updated (added Groth16 section)
```

## Troubleshooting

### Go Installation Fails

If `install_dependencies.sh` fails to install Go:

**Option 1: Manual installation (recommended for latest version)**
```bash
# Download latest Go
wget https://go.dev/dl/go1.23.6.linux-amd64.tar.gz

# Extract to /usr/local
sudo rm -rf /usr/local/go
sudo tar -C /usr/local -xzf go1.23.6.linux-amd64.tar.gz

# Add to PATH (add to ~/.bashrc for persistence)
export PATH=$PATH:/usr/local/go/bin
```

**Option 2: Use system package manager**
```bash
# Ubuntu/Debian
sudo apt update && sudo apt install -y golang-go

# Fedora
sudo dnf install -y golang

# Arch
sudo pacman -S go
```

### gnark Installation Fails

If `setup_gnark.sh` fails:

```bash
# Ensure Go is in PATH
which go

# Manually install gnark
cd gnark-wrapper
go get github.com/consensys/gnark@latest
go get github.com/consensys/gnark-crypto@latest
```

### Build Tools Missing

If you get CGO errors:

```bash
# Ubuntu/Debian
sudo apt install -y build-essential

# Fedora
sudo dnf groupinstall -y "Development Tools"

# Arch
sudo pacman -S base-devel
```

## Next Steps After Installation

Once you've successfully run both scripts, let me know and I'll proceed with:

1. **Phase 2**: Implement Halo2 proof parser in Rust
2. **Phase 3**: Implement Groth16 verifier circuit in Go
3. **Phase 4**: Build Rust-Go FFI bridge
4. **Phase 5**: Integration testing
5. **Phase 6**: Deploy tiny verifier on Ethereum mainnet

## Questions?

- **Why Go?** gnark is the industry-standard library for Groth16, written in Go
- **Why not use SP1's gnark-ffi?** It's tightly coupled to SP1's internal proof format
- **How long will this take?** 2-4 weeks of development + testing
- **Is this production-ready?** Yes, after auditing. This is how Succinct, Linea, and Worldcoin deploy on mainnet

## References

- [gnark Documentation](https://docs.gnark.consensys.net/)
- [Go Installation Guide](https://go.dev/doc/install)
- [GROTH16_WRAPPER.md](GROTH16_WRAPPER.md) - Full technical documentation

