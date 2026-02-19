/// Configuration for the Acki Nacki Bridge frontend
/// 
/// Update these values after deploying contracts to Sepolia

/// Bridge contract address on Sepolia
/// Update this after running: forge script script/DeployTestBridge.s.sol:DeployTestBridge
pub const BRIDGE_CONTRACT_ADDRESS: &str = "0xDE8180911Ab2EbC9A6c1F5526bCE4c8242C061d9";

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

/// Bridge contract ABI (minimal - just the functions we need)
pub const BRIDGE_ABI: &str = r#"[
    {
        "inputs": [],
        "name": "deposit",
        "outputs": [],
        "stateMutability": "payable",
        "type": "function"
    },
    {
        "inputs": [
            {"internalType": "bytes", "name": "proof", "type": "bytes"},
            {"internalType": "uint256[]", "name": "publicInputs", "type": "uint256[]"}
        ],
        "name": "withdraw",
        "outputs": [],
        "stateMutability": "nonpayable",
        "type": "function"
    },
    {
        "inputs": [],
        "name": "depositCount",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function"
    },
    {
        "inputs": [],
        "name": "totalDeposited",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function"
    },
    {
        "inputs": [],
        "name": "totalWithdrawn",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function"
    },
    {
        "inputs": [{"internalType": "bytes32", "name": "", "type": "bytes32"}],
        "name": "withdrawals",
        "outputs": [{"internalType": "bool", "name": "", "type": "bool"}],
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
    },
    {
        "anonymous": false,
        "inputs": [
            {"indexed": true, "internalType": "bytes32", "name": "depositId", "type": "bytes32"},
            {"indexed": true, "internalType": "address", "name": "recipient", "type": "address"},
            {"indexed": false, "internalType": "uint256", "name": "amount", "type": "uint256"}
        ],
        "name": "Withdrawal",
        "type": "event"
    }
]"#;

