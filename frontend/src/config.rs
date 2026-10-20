// Configuration for the Acki Nacki Bridge frontend
//
// Update these values after deploying contracts to Sepolia

/// Bridge contract address on Sepolia — the shellnet deploy Acki Nacki
/// currently credits. From `DeployShellnetE2EBridge.s.sol` (2026-10-15).
pub const BRIDGE_CONTRACT_ADDRESS: &str = "0x5384f162aa1a80b7a03687e1a3188f67b1fffad5";

/// USDC the shellnet bridge pulls (`usdc()`). Circle's Sepolia USDC, not
/// the Aave V3 faucet token `DeployTestBridge` used.
pub const USDC_CONTRACT_ADDRESS: &str = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238";

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
/// The deposit form prefills the recipient field with this value when it is
/// non-empty. An empty value (the default, when `AN_RECEIVER` is unset)
/// leaves the field blank for the user to type. `anWorkchain` is pinned to
/// 0 in the ABI call; Acki Nacki ignores it.
pub const ACKI_NACKI_RECEIVER: &str = match option_env!("AN_RECEIVER") {
    Some(v) => v,
    None => "",
};
