# Acki Nacki Bridge Frontend

A beautiful, modern web interface for the Acki Nacki Bridge built with **Rust + Yew + WebAssembly**.

## Features

- 🦀 **100% Rust** - Type-safe, fast, and compiled to WebAssembly
- 🎨 **Modern UI** - Beautiful gradient design with smooth animations
- 🔐 **MetaMask Integration** - Connect your wallet seamlessly
- ⚡ **Real-time Updates** - Live transaction status and history
- 📱 **Responsive** - Works on desktop, tablet, and mobile
- 🌙 **Dark Theme** - Easy on the eyes

## Prerequisites

1. **Rust** (latest stable)
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Trunk** (Rust WASM bundler)
   ```bash
   cargo install trunk
   ```

3. **wasm32 target**
   ```bash
   rustup target add wasm32-unknown-unknown
   ```

## Development

### Run Development Server

```bash
cd frontend
export BRIDGE_CONTRACT_ADDRESS='0x<new-bridge-address>'
trunk serve
```

The app will be available at `http://localhost:8080` and will auto-reload on changes.

### Build for Production

```bash
trunk build --release
```

The optimized build will be in `dist/` directory.

## Project Structure

```
frontend/
├── src/
│   ├── lib.rs              # Main app component
│   ├── components/         # UI components
│   │   ├── header.rs       # Header with wallet connection
│   │   ├── deposit_form.rs # Deposit interface
│   │   ├── stats.rs        # Bridge statistics
│   │   └── transaction_history.rs
│   ├── hooks/              # Custom Yew hooks
│   ├── utils/              # Utility functions
│   ├── config.rs           # Network / contract configuration
│   └── web3.rs             # Wallet + contract calls
├── index.html              # HTML template
├── styles.css              # Global styles
├── Cargo.toml              # Rust dependencies
└── Trunk.toml              # Build configuration
```

## Features Overview

**This is a deposit-only interface.** There is no withdrawal UI, and that is deliberate: the
Ethereum contract exposes no user-callable withdrawal. Payouts go through `withdrawByProof`, which
requires a Circuit-4 ZK proof of a burn on Acki Nacki and is submitted by a relayer, not by the
person receiving the funds. A withdrawal form existed here once and was removed — it could make a
user believe funds had been returned when nothing had happened. Verified 2026-08-18: the string
`withdraw` does not occur anywhere under `frontend/src/`.

### Deposit Flow
1. Connect MetaMask wallet
2. Approve USDC for the bridge contract
3. Enter the amount and the 64-hex-character Acki Nacki account ID
4. Submit — the contract takes custody via `transferFrom` and emits a `Deposit` event

The proof of that event is produced off-chain and consumed on the Acki Nacki side, which mints to the
destination. Nothing further is required from the user in this UI.

### Transaction History
- View all your bridge transactions
- Real-time status updates
- Direct links to Etherscan

## Technology Stack

- **Yew** - Rust framework for building web apps
- **WebAssembly** - Compile Rust to run in the browser
- **Trunk** - WASM web application bundler
- **CSS3** - Modern styling with gradients and animations

## Deployment

Set `BRIDGE_CONTRACT_ADDRESS` as described under Configuration before building.

### Deploy to GitHub Pages

```bash
trunk build --release --public-url /acki-nacki-bridge/
# Upload dist/ to GitHub Pages
```

### Deploy to Vercel/Netlify

```bash
trunk build --release
# Deploy dist/ directory
```

### Deploy with Docker

```bash
docker build --build-arg BRIDGE_CONTRACT_ADDRESS -t acki-nacki-bridge-frontend .
docker run -p 8080:80 acki-nacki-bridge-frontend
```

## Configuration

Set `BRIDGE_CONTRACT_ADDRESS` when building with Trunk. It must be a new
Sepolia deployment exposing `deposit(uint256,bytes32)`, paired with an
`eccUSDCBridge` 1.11.0 or newer that embeds this build's deposit key. The old
workchain-bearing deployment addresses are incompatible.

There is no default bridge address. Without a valid non-zero 20-byte address,
the form disables deposits and the wallet helpers reject approval and deposit
requests before contacting MetaMask. Rebuild the frontend after changing the
address. Other network and token settings live in `src/config.rs`.

The same EVM deployment must be configured as `BRIDGE_ADDRESS` in the CLI and
deposit relayer; set their `BRIDGE_DEPLOY_BLOCK` / `FROM_BLOCK` to its deployment
block.

## Browser Support

- Chrome/Edge (latest)
- Firefox (latest)
- Safari (latest)
- Mobile browsers

## Contributing

1. Fork the repository
2. Create your feature branch
3. Make your changes
4. Test thoroughly
5. Submit a pull request

## License

MIT License - see LICENSE file for details

