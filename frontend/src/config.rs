// Configuration for the Acki Nacki Bridge frontend
//
// Update these values after deploying contracts to Sepolia

/// Bridge contract address on Sepolia
/// Update this after running: forge script script/DeployTestBridge.s.sol:DeployTestBridge
pub const BRIDGE_CONTRACT_ADDRESS: &str = "0xDE8180911Ab2EbC9A6c1F5526bCE4c8242C061d9";

/// USDC (ERC-20, 6 decimals) accepted for deposits.
/// Sepolia testnet: the Aave V3 faucet USDC underlying.
pub const USDC_CONTRACT_ADDRESS: &str = "0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8";

/// Aave V3 Sepolia permissionless faucet — mints test USDC for dev wallets.
pub const AAVE_FAUCET_ADDRESS: &str = "0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D";

/// USDC decimals (USD Coin uses 6 on Ethereum).
pub const USDC_DECIMALS: u32 = 6;

/// One whole USDC expressed in base units (10^6).
pub const USDC_UNIT: u128 = 1_000_000;

/// Sepolia Chain ID
pub const SEPOLIA_CHAIN_ID: u32 = 11155111;

/// Sepolia Chain ID as hex string for MetaMask
pub const SEPOLIA_CHAIN_ID_HEX: &str = "0xaa36a7";

/// Sepolia RPC URL (public endpoint)
pub const SEPOLIA_RPC_URL: &str = "https://rpc.sepolia.org";

/// Network name
pub const NETWORK_NAME: &str = "Sepolia";

/// Block explorer URL
pub const BLOCK_EXPLORER_URL: &str = "https://sepolia.etherscan.io";

/// Bridge + USDC contract ABI (minimal — just the functions the UI calls).
pub const BRIDGE_ABI: &str = r#"[
    {
        "inputs": [
            {"internalType": "uint256", "name": "amount", "type": "uint256"},
            {"internalType": "int8", "name": "anWorkchain", "type": "int8"},
            {"internalType": "bytes32", "name": "anAccount", "type": "bytes32"}
        ],
        "name": "deposit",
        "outputs": [],
        "stateMutability": "nonpayable",
        "type": "function"
    },
    {
        "inputs": [],
        "name": "depositCounter",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function"
    },
    {
        "inputs": [],
        "name": "treasuryBalance",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function"
    },
    {
        "inputs": [
            {"internalType": "address", "name": "spender", "type": "address"},
            {"internalType": "uint256", "name": "amount", "type": "uint256"}
        ],
        "name": "approve",
        "outputs": [{"internalType": "bool", "name": "", "type": "bool"}],
        "stateMutability": "nonpayable",
        "type": "function"
    },
    {
        "inputs": [
            {"internalType": "address", "name": "owner", "type": "address"},
            {"internalType": "address", "name": "spender", "type": "address"}
        ],
        "name": "allowance",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function"
    },
    {
        "anonymous": false,
        "inputs": [
            {"indexed": true, "internalType": "uint256", "name": "depositId", "type": "uint256"},
            {"indexed": true, "internalType": "address", "name": "sender", "type": "address"},
            {"indexed": false, "internalType": "uint256", "name": "amount", "type": "uint256"},
            {"indexed": false, "internalType": "int8", "name": "anWorkchain", "type": "int8"},
            {"indexed": false, "internalType": "bytes32", "name": "anAccount", "type": "bytes32"},
            {"indexed": false, "internalType": "uint256", "name": "timestamp", "type": "uint256"}
        ],
        "name": "Deposit",
        "type": "event"
    }
]"#;

/// Optional default Acki Nacki receiver account (build-time `AN_RECEIVER`).
///
/// The bridge's `deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount)`
/// now takes the AN destination explicitly (an EVM address is not a valid AN
/// recipient), so the deposit form collects workchain + account per-deposit.
/// This constant is only a convenience default the UI may prefill; an empty
/// value means "no prefill — the user types their AN account".
pub const ACKI_NACKI_RECEIVER: &str = match option_env!("AN_RECEIVER") {
    Some(v) => v,
    None => "",
};
