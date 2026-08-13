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
│   │   ├── withdraw_form.rs# Withdrawal interface
│   │   ├── stats.rs        # Bridge statistics
│   │   └── transaction_history.rs
│   ├── hooks/              # Custom Yew hooks
│   └── utils/              # Utility functions
├── index.html              # HTML template
├── styles.css              # Global styles
├── Cargo.toml              # Rust dependencies
└── Trunk.toml              # Build configuration
```

## Features Overview

### Deposit Flow
1. Connect MetaMask wallet
2. Enter amount to bridge
3. Approve transaction
4. Receive Deposit ID for withdrawal

### Withdraw Flow
1. Enter Deposit ID from Ethereum deposit
2. Enter amount to withdraw
3. Generate ZK proof (automatic)
4. Submit withdrawal transaction

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
docker build -t acki-nacki-bridge-frontend .
docker run -p 8080:80 acki-nacki-bridge-frontend
```

## Configuration

Edit `src/lib.rs` to configure:
- Contract addresses
- RPC endpoints
- Network settings

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

