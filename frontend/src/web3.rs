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

// ---------------------------------------------------------------------------
// ABI encoding helpers (minimal, for the few calls the UI makes)
// ---------------------------------------------------------------------------

/// Left-pad a uint to a 32-byte (64 hex char) ABI word.
fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

/// Encode an address as a 32-byte ABI word (right-aligned, 12-byte zero pad).
fn encode_address(addr: &str) -> String {
    let a = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", a)
}

/// Encode a Solidity `int8` as a 32-byte two's-complement ABI word.
fn encode_int8(value: i8) -> String {
    let fill = if value < 0 { "ff" } else { "00" };
    let mut s = String::with_capacity(64);
    for _ in 0..31 {
        s.push_str(fill);
    }
    s.push_str(&format!("{:02x}", value as u8));
    s
}

/// Encode a Solidity `bytes32` as a 32-byte ABI word. The Acki Nacki account
/// is a 256-bit number, so a short hex input is left-padded with zeros.
fn encode_bytes32(value: &str) -> String {
    let v = value.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", v)
}

/// Send a contract transaction via MetaMask. Returns the tx hash.
async fn send_tx(to: &str, data: &str) -> Result<String, String> {
    let ethereum = get_ethereum().ok_or("MetaMask not installed")?;
    let account = get_current_account().ok_or("No account connected")?;

    switch_to_sepolia().await?;

    let tx_param = to_value(&serde_json::json!({
        "from": account,
        "to": to,
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

    from_value(result).map_err(|e| format!("Failed to parse transaction hash: {:?}", e))
}

/// Approve the bridge to spend `amount` USDC base units on the caller's behalf.
pub async fn approve_usdc(amount: u128) -> Result<String, String> {
    // approve(address spender, uint256 amount) = 0x095ea7b3
    let data = format!(
        "0x095ea7b3{}{}",
        encode_address(BRIDGE_CONTRACT_ADDRESS),
        encode_uint256(amount)
    );
    send_tx(USDC_CONTRACT_ADDRESS, &data).await
}

/// Deposit `amount` USDC base units into the bridge (pulls via `transferFrom`),
/// bridging to the Acki Nacki destination `an_workchain:an_account`. The EVM
/// `msg.sender` is not a valid AN recipient, so the destination is supplied
/// explicitly and carried as a ZK public input.
pub async fn make_deposit(
    amount: u128,
    an_workchain: i8,
    an_account: &str,
) -> Result<String, String> {
    // deposit(uint256 amount, int8 anWorkchain, bytes32 anAccount) = 0xa41d0229
    let data = format!(
        "0xa41d0229{}{}{}",
        encode_uint256(amount),
        encode_int8(an_workchain),
        encode_bytes32(an_account)
    );
    send_tx(BRIDGE_CONTRACT_ADDRESS, &data).await
}

/// Mint `amount` test USDC base units to the connected account from the Aave faucet.
pub async fn mint_test_usdc(amount: u128) -> Result<String, String> {
    let account = get_current_account().ok_or("No account connected")?;
    // mint(address token, address to, uint256 amount) = 0xc6c3bbe6
    let data = format!(
        "0xc6c3bbe6{}{}{}",
        encode_address(USDC_CONTRACT_ADDRESS),
        encode_address(&account),
        encode_uint256(amount)
    );
    send_tx(AAVE_FAUCET_ADDRESS, &data).await
}

/// Current USDC allowance (base units) the owner has granted the bridge.
pub async fn get_allowance(owner: &str) -> Result<u128, String> {
    // allowance(address owner, address spender) = 0xdd62ed3e
    let data = format!(
        "0xdd62ed3e{}{}",
        encode_address(owner),
        encode_address(BRIDGE_CONTRACT_ADDRESS)
    );
    let raw = call_to(USDC_CONTRACT_ADDRESS, &data).await?;
    parse_uint128(&raw)
}

/// Connected account's USDC balance in base units.
pub async fn get_usdc_balance(owner: &str) -> Result<u128, String> {
    // balanceOf(address) = 0x70a08231
    let data = format!("0x70a08231{}", encode_address(owner));
    let raw = call_to(USDC_CONTRACT_ADDRESS, &data).await?;
    parse_uint128(&raw)
}

/// Poll for a transaction receipt until it is mined. Returns Ok on success
/// status, Err on revert or timeout. Used to sequence approve → deposit.
pub async fn wait_for_receipt(tx_hash: &str) -> Result<(), String> {
    let ethereum = get_ethereum().ok_or("MetaMask not installed")?;
    for _ in 0..40 {
        let params = js_sys::Array::new();
        params.push(&JsValue::from_str(tx_hash));

        let request = js_sys::Object::new();
        js_sys::Reflect::set(
            &request,
            &"method".into(),
            &"eth_getTransactionReceipt".into(),
        )
        .map_err(|_| "Failed to build request")?;
        js_sys::Reflect::set(&request, &"params".into(), &params)
            .map_err(|_| "Failed to build request")?;

        let result = ethereum
            .request(request.into())
            .await
            .map_err(|e| format!("Receipt poll failed: {:?}", e))?;

        if !result.is_null() && !result.is_undefined() {
            let status = js_sys::Reflect::get(&result, &"status".into())
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            if status == "0x1" {
                return Ok(());
            } else {
                return Err("Transaction reverted".to_string());
            }
        }
        sleep(3000).await;
    }
    Err("Timed out waiting for transaction to be mined".to_string())
}

/// Sleep for `ms` milliseconds (setTimeout wrapped in a Promise).
async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(win) = window() {
            let resolve_fn: &js_sys::Function = resolve.unchecked_ref();
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(resolve_fn, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Get bridge statistics (deposit count + treasury principal in USDC base units).
pub async fn get_bridge_stats() -> Result<BridgeStats, String> {
    // depositCounter() = 0xecb3dc88
    let deposit_count_result = call_to(BRIDGE_CONTRACT_ADDRESS, "0xecb3dc88").await?;
    // treasuryBalance() = 0x313dab20
    let treasury_result = call_to(BRIDGE_CONTRACT_ADDRESS, "0x313dab20").await?;

    Ok(BridgeStats {
        deposit_count: parse_uint128(&deposit_count_result)?.to_string(),
        treasury_balance: parse_uint128(&treasury_result)?.to_string(),
    })
}

/// Read `depositCounter()`. The id of the most recent deposit is this minus 1.
pub async fn get_deposit_counter() -> Result<u128, String> {
    let raw = call_to(BRIDGE_CONTRACT_ADDRESS, "0xecb3dc88").await?;
    parse_uint128(&raw)
}

/// Call a contract view function on an arbitrary `to` address.
async fn call_to(to: &str, data: &str) -> Result<String, String> {
    let ethereum = get_ethereum().ok_or("MetaMask not installed")?;

    let call_param = to_value(&serde_json::json!({
        "to": to,
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

    from_value(result).map_err(|e| format!("Failed to parse result: {:?}", e))
}

/// Parse a uint from a 0x-prefixed hex string (sufficient range for our values).
fn parse_uint128(hex: &str) -> Result<u128, String> {
    let hex = hex.trim_start_matches("0x");
    let hex = if hex.is_empty() { "0" } else { hex };
    // Take the low 32 hex chars (16 bytes) to fit u128 — our values are small.
    let start = hex.len().saturating_sub(32);
    u128::from_str_radix(&hex[start..], 16).map_err(|e| format!("Failed to parse uint: {:?}", e))
}

#[derive(Debug, Clone)]
pub struct BridgeStats {
    pub deposit_count: String,
    pub treasury_balance: String,
}

/// Format USDC base units (6 decimals) to a human string, e.g. 100000000 → "100.000000".
pub fn units_to_usdc(units: u128) -> String {
    let whole = units / USDC_UNIT;
    let frac = units % USDC_UNIT;
    format!("{}.{:06}", whole, frac)
}

/// Parse a human USDC amount (e.g. "12.5") into base units (6 decimals).
pub fn usdc_to_units(amount: &str) -> Result<u128, String> {
    let amount = amount.trim();
    if amount.is_empty() {
        return Err("empty amount".to_string());
    }
    let mut parts = amount.splitn(2, '.');
    let whole_str = parts.next().unwrap_or("0");
    let frac_str = parts.next().unwrap_or("");

    let whole: u128 = whole_str
        .parse()
        .map_err(|_| "Invalid USDC amount".to_string())?;
    if frac_str.len() > USDC_DECIMALS as usize {
        return Err("USDC supports at most 6 decimal places".to_string());
    }
    let frac_padded = format!("{:0<6}", frac_str);
    let frac: u128 = if frac_padded.is_empty() {
        0
    } else {
        frac_padded
            .parse()
            .map_err(|_| "Invalid USDC amount".to_string())?
    };
    Ok(whole * USDC_UNIT + frac)
}
