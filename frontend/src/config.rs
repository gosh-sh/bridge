// Configuration for the Acki Nacki Bridge frontend
//
// Update these values after deploying contracts to Sepolia

/// Address of a new bridge deployment, supplied when building the frontend.
pub const BRIDGE_CONTRACT_ADDRESS: &str = match option_env!("BRIDGE_CONTRACT_ADDRESS") {
    Some(value) => value,
    None => "",
};

pub fn bridge_contract_address() -> Result<&'static str, String> {
    validate_bridge_address(BRIDGE_CONTRACT_ADDRESS.trim())
}

fn validate_bridge_address(value: &str) -> Result<&str, String> {
    let valid = value.strip_prefix("0x").is_some_and(|hex| {
        hex.len() == 40
            && hex.chars().all(|c| c.is_ascii_hexdigit())
            && hex.chars().any(|c| c != '0')
    });
    if !valid {
        return Err("Bridge deployment is not configured. Deposits are unavailable.".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::validate_bridge_address;

    #[test]
    fn a_deployment_address_must_be_explicit_and_nonzero() {
        for value in [
            "",
            "0x",
            "0x1234",
            "0x0000000000000000000000000000000000000000",
        ] {
            assert!(validate_bridge_address(value).is_err(), "{value}");
        }
    }

    #[test]
    fn a_configured_deployment_address_is_preserved() {
        let address = "0x1234567890123456789012345678901234567890";
        assert_eq!(validate_bridge_address(address).unwrap(), address);
    }
}

/// USDC the shellnet bridge pulls (`usdc()`). Circle's Sepolia USDC, not
/// the Aave V3 faucet token `DeployTestBridge` used.
pub const USDC_CONTRACT_ADDRESS: &str = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238";

/// USDC decimals (USD Coin uses 6 on Ethereum).
pub const USDC_DECIMALS: u32 = 6;
pub const MAX_TRANSFER_USDC_UNITS: u128 = 50_000_000;

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
            {"indexed": false, "internalType": "bytes32", "name": "anAccount", "type": "bytes32"},
            {"indexed": false, "internalType": "uint256", "name": "timestamp", "type": "uint256"}
        ],
        "name": "Deposit",
        "type": "event"
    }
]"#;

/// Optional default Acki Nacki receiver account (build-time `AN_RECEIVER`).
///
/// The bridge's `deposit(uint256 amount, bytes32 anAccount)`
/// takes the AN account explicitly.
/// This constant is only a convenience default the UI may prefill; an empty
/// value means "no prefill — the user types their AN account".
pub const ACKI_NACKI_RECEIVER: &str = match option_env!("AN_RECEIVER") {
    Some(v) => v,
    None => "",
};
