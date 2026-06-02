// Configuration for the Acki Nacki Bridge frontend
//
// Update these values after deploying contracts to Sepolia

/// Bridge contract address on Sepolia
/// Update this after running: forge script script/DeployTestBridge.s.sol:DeployTestBridge
pub const BRIDGE_CONTRACT_ADDRESS: &str = "0xDE8180911Ab2EbC9A6c1F5526bCE4c8242C061d9";

/// USDT (ERC-20, 6 decimals) accepted for deposits.
/// Sepolia testnet: the Aave V3 faucet USDT underlying.
pub const USDT_CONTRACT_ADDRESS: &str = "0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0";

/// Aave V3 Sepolia permissionless faucet — mints test USDT for dev wallets.
pub const AAVE_FAUCET_ADDRESS: &str = "0xC959483DBa39aa9E78757139af0e9a2EDEb3f42D";

/// USDT decimals (Tether uses 6 on Ethereum).
pub const USDT_DECIMALS: u32 = 6;

/// One whole USDT expressed in base units (10^6).
pub const USDT_UNIT: u128 = 1_000_000;

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

/// Bridge + USDT contract ABI (minimal — just the functions the UI calls).
pub const BRIDGE_ABI: &str = r#"[
    {
        "inputs": [{"internalType": "uint256", "name": "amount", "type": "uint256"}],
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
            {"indexed": false, "internalType": "uint256", "name": "timestamp", "type": "uint256"}
        ],
        "name": "Deposit",
        "type": "event"
    }
]"#;

/// Acki Nacki receiver / endpoint (configurable).
///
/// The currently deployed Sepolia bridge contract's `deposit(uint256)` credits
/// funds to `msg.sender` on the Acki Nacki side, so this value is **not** sent
/// on-chain yet. It is exposed as a configurable parameter for the E2E test:
/// once the AN side provides a concrete receiving address / endpoint, set it
/// here (or via the build-time `AN_RECEIVER` env var). An empty value means
/// "use the connected wallet address (msg.sender) as the AN recipient".
pub const ACKI_NACKI_RECEIVER: &str = match option_env!("AN_RECEIVER") {
    Some(v) => v,
    None => "",
};
