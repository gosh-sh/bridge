// serde traits used by serde_json::json! macro via serde_wasm_bindgen
use serde_wasm_bindgen::{from_value, to_value};
/// Web3 and MetaMask integration for the Acki Nacki Bridge
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::window;

use crate::config::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    pub fn log(s: &str);

    #[wasm_bindgen(js_namespace = console)]
    pub fn error(s: &str);
}

/// Ethereum provider interface (MetaMask)
#[wasm_bindgen]
extern "C" {
    pub type Ethereum;

    #[wasm_bindgen(method, catch)]
    pub async fn request(this: &Ethereum, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(method, getter)]
    pub fn selectedAddress(this: &Ethereum) -> Option<String>;

    #[wasm_bindgen(method, getter)]
    pub fn chainId(this: &Ethereum) -> Option<String>;
}

/// Check if MetaMask is installed
pub fn is_metamask_installed() -> bool {
    if let Some(window) = window() {
        // Check for window.ethereum
        let has_ethereum =
            js_sys::Reflect::has(&window, &JsValue::from_str("ethereum")).unwrap_or(false);
        web_sys::console::log_1(
            &format!(
                "MetaMask detection: window.ethereum exists = {}",
                has_ethereum
            )
            .into(),
        );

        if has_ethereum {
            if let Ok(ethereum) = js_sys::Reflect::get(&window, &JsValue::from_str("ethereum")) {
                let is_metamask = js_sys::Reflect::get(&ethereum, &JsValue::from_str("isMetaMask"))
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                web_sys::console::log_1(
                    &format!("window.ethereum.isMetaMask = {}", is_metamask).into(),
                );
                return true;
            }
        }

        // Fallback: check for window.web3 (older MetaMask versions)
        let has_web3 = js_sys::Reflect::has(&window, &JsValue::from_str("web3")).unwrap_or(false);
        if has_web3 {
            web_sys::console::log_1(&"Found window.web3 (legacy MetaMask)".into());
            return true;
        }

        web_sys::console::log_1(&"MetaMask not detected".into());
        false
    } else {
        web_sys::console::log_1(&"MetaMask detection: window is None".into());
        false
    }
}

/// Get the Ethereum provider (MetaMask)
pub fn get_ethereum() -> Option<Ethereum> {
    let window = window()?;
    let ethereum = js_sys::Reflect::get(&window, &JsValue::from_str("ethereum")).ok()?;

    // Log the type of ethereum object for debugging
    web_sys::console::log_1(&format!("ethereum object type: {:?}", ethereum.js_typeof()).into());

    // Try to cast to Ethereum type
    match ethereum.dyn_into::<Ethereum>() {
        Ok(eth) => {
            web_sys::console::log_1(&"Successfully cast to Ethereum type".into());
            Some(eth)
        },
        Err(e) => {
            web_sys::console::log_1(&format!("Failed to cast to Ethereum type: {:?}", e).into());
            // Return the error value as Ethereum anyway - it should still work
            e.dyn_into::<Ethereum>().ok()
        },
    }
}

/// Connect to MetaMask and request account access
pub async fn connect_wallet() -> Result<String, String> {
    // Get ethereum provider directly from window
    let window = window().ok_or("No window object")?;
    let ethereum_val = js_sys::Reflect::get(&window, &JsValue::from_str("ethereum"))
        .map_err(|_| "Failed to get window.ethereum")?;

    if ethereum_val.is_undefined() || ethereum_val.is_null() {
        return Err("MetaMask not installed. Please install MetaMask extension.".to_string());
    }

    web_sys::console::log_1(&"Got window.ethereum object".into());

    // Cast to Ethereum type
    let ethereum: Ethereum = ethereum_val.unchecked_into();

    // Build request object manually
    let request = js_sys::Object::new();
    js_sys::Reflect::set(&request, &"method".into(), &"eth_requestAccounts".into())
        .map_err(|_| "Failed to build request")?;

    let result = ethereum
        .request(request.into())
        .await
        .map_err(|e| format!("MetaMask error: {:?}", e))?;

    let accounts: Vec<String> =
        from_value(result).map_err(|e| format!("Failed to parse accounts: {:?}", e))?;

    accounts
        .first()
        .cloned()
        .ok_or_else(|| "No accounts found".to_string())
}

/// Get the current connected account
pub fn get_current_account() -> Option<String> {
    let ethereum = get_ethereum()?;
    ethereum.selectedAddress()
}

/// Get the current chain ID
pub fn get_chain_id() -> Option<String> {
    let ethereum = get_ethereum()?;
    ethereum.chainId()
}

/// Switch to Sepolia network
pub async fn switch_to_sepolia() -> Result<(), String> {
    let ethereum = get_ethereum().ok_or("MetaMask not installed")?;

    // Try to switch to Sepolia
    let chain_param = js_sys::Object::new();
    js_sys::Reflect::set(
        &chain_param,
        &"chainId".into(),
        &SEPOLIA_CHAIN_ID_HEX.into(),
    )
    .map_err(|_| "Failed to build chain param")?;

    let params = js_sys::Array::new();
    params.push(&chain_param);

    let switch_request = js_sys::Object::new();
    js_sys::Reflect::set(
        &switch_request,
        &"method".into(),
        &"wallet_switchEthereumChain".into(),
    )
    .map_err(|_| "Failed to build request")?;
    js_sys::Reflect::set(&switch_request, &"params".into(), &params)
        .map_err(|_| "Failed to build request")?;

    let switch_result = ethereum.request(switch_request.into()).await;

    // If switching failed, try to add the network
    if switch_result.is_err() {
        let network_param = to_value(&serde_json::json!({
            "chainId": SEPOLIA_CHAIN_ID_HEX,
            "chainName": "Sepolia Testnet",
            "nativeCurrency": {
                "name": "Sepolia ETH",
                "symbol": "ETH",
                "decimals": 18
            },
            "rpcUrls": [SEPOLIA_RPC_URL],
            "blockExplorerUrls": [BLOCK_EXPLORER_URL]
        }))
        .map_err(|e| format!("Serialization error: {:?}", e))?;

        let add_params = js_sys::Array::new();
        add_params.push(&network_param);

        let add_request = js_sys::Object::new();
        js_sys::Reflect::set(
            &add_request,
            &"method".into(),
            &"wallet_addEthereumChain".into(),
        )
        .map_err(|_| "Failed to build request")?;
        js_sys::Reflect::set(&add_request, &"params".into(), &add_params)
            .map_err(|_| "Failed to build request")?;

        ethereum
            .request(add_request.into())
            .await
            .map_err(|e| format!("Failed to add Sepolia network: {:?}", e))?;
    }

    Ok(())
}

/// Make a deposit to the bridge contract
pub async fn make_deposit(amount_wei: &str, _acki_nacki_address: &str) -> Result<String, String> {
    let ethereum = get_ethereum().ok_or("MetaMask not installed")?;
    let account = get_current_account().ok_or("No account connected")?;

    // Ensure we're on Sepolia
    switch_to_sepolia().await?;

    // Encode the function call: deposit()
    // Function selector: keccak256("deposit()")[0:4] = 0xd0e30db0
    let data = "0xd0e30db0";

    let tx_param = to_value(&serde_json::json!({
        "from": account,
        "to": BRIDGE_CONTRACT_ADDRESS,
        "value": amount_wei,
        "data": data,
    }))
    .map_err(|e| format!("Serialization error: {:?}", e))?;

    let params = js_sys::Array::new();
    params.push(&tx_param);

    let request = js_sys::Object::new();
    js_sys::Reflect::set(&request, &"method".into(), &"eth_sendTransaction".into())
        .map_err(|_| "Failed to build request")?;
    js_sys::Reflect::set(&request, &"params".into(), &params)
        .map_err(|_| "Failed to build request")?;

    let result = ethereum
        .request(request.into())
        .await
        .map_err(|e| format!("Transaction failed: {:?}", e))?;

    let tx_hash: String =
        from_value(result).map_err(|e| format!("Failed to parse transaction hash: {:?}", e))?;

    Ok(tx_hash)
}

/// Get bridge statistics
pub async fn get_bridge_stats() -> Result<BridgeStats, String> {
    // Call depositCount()
    let deposit_count_data = "0x2dfdf0b5"; // keccak256("depositCount()")[0:4]
    let deposit_count_result = call_contract(deposit_count_data).await?;

    // Call totalDeposited()
    let total_deposited_data = "0x4e71d92d"; // keccak256("totalDeposited()")[0:4]
    let total_deposited_result = call_contract(total_deposited_data).await?;

    // Call totalWithdrawn()
    let total_withdrawn_data = "0xc4e2b619"; // keccak256("totalWithdrawn()")[0:4]
    let total_withdrawn_result = call_contract(total_withdrawn_data).await?;

    Ok(BridgeStats {
        deposit_count: parse_uint256(&deposit_count_result)?,
        total_deposited: parse_uint256(&total_deposited_result)?,
        total_withdrawn: parse_uint256(&total_withdrawn_result)?,
    })
}

/// Call a contract view function
async fn call_contract(data: &str) -> Result<String, String> {
    let ethereum = get_ethereum().ok_or("MetaMask not installed")?;

    let call_param = to_value(&serde_json::json!({
        "to": BRIDGE_CONTRACT_ADDRESS,
        "data": data,
    }))
    .map_err(|e| format!("Serialization error: {:?}", e))?;

    let params = js_sys::Array::new();
    params.push(&call_param);
    params.push(&"latest".into());

    let request = js_sys::Object::new();
    js_sys::Reflect::set(&request, &"method".into(), &"eth_call".into())
        .map_err(|_| "Failed to build request")?;
    js_sys::Reflect::set(&request, &"params".into(), &params)
        .map_err(|_| "Failed to build request")?;

    let result = ethereum
        .request(request.into())
        .await
        .map_err(|e| format!("Contract call failed: {:?}", e))?;

    let result_str: String =
        from_value(result).map_err(|e| format!("Failed to parse result: {:?}", e))?;

    Ok(result_str)
}

/// Parse a uint256 from hex string
fn parse_uint256(hex: &str) -> Result<String, String> {
    let hex = hex.trim_start_matches("0x");
    let value =
        u128::from_str_radix(hex, 16).map_err(|e| format!("Failed to parse uint256: {:?}", e))?;
    Ok(value.to_string())
}

#[derive(Debug, Clone)]
pub struct BridgeStats {
    pub deposit_count: String,
    pub total_deposited: String,
    pub total_withdrawn: String,
}

/// Format Wei to ETH string
pub fn wei_to_eth(wei: &str) -> String {
    let wei_value: u128 = wei.parse().unwrap_or(0);
    let eth_value = wei_value as f64 / 1_000_000_000_000_000_000.0;
    format!("{:.4}", eth_value)
}

/// Format ETH to Wei string
pub fn eth_to_wei(eth: &str) -> Result<String, String> {
    let eth_value: f64 = eth.parse().map_err(|_| "Invalid ETH amount")?;
    let wei_value = (eth_value * 1_000_000_000_000_000_000.0) as u128;
    Ok(wei_value.to_string())
}
