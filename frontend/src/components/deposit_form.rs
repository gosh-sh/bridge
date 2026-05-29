use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::config::{ACKI_NACKI_RECEIVER, SEPOLIA_CHAIN_ID_HEX};
use crate::web3::{eth_to_wei, get_chain_id, make_deposit, switch_to_sepolia};

#[derive(Properties, PartialEq)]
pub struct DepositFormProps {
    pub wallet_connected: bool,
}

#[function_component(DepositForm)]
pub fn deposit_form(props: &DepositFormProps) -> Html {
    let amount = use_state(String::new);
    let deposit_id = use_state(|| None::<u64>);
    let is_loading = use_state(|| false);
    let tx_hash = use_state(|| None::<String>);
    let error_msg = use_state(|| None::<String>);

    let on_amount_change = {
        let amount = amount.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            amount.set(input.value());
        })
    };

    let on_submit = {
        let amount = amount.clone();
        let is_loading = is_loading.clone();
        let deposit_id = deposit_id.clone();
        let tx_hash = tx_hash.clone();
        let error_msg = error_msg.clone();
        let wallet_connected = props.wallet_connected;

        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();

            if !wallet_connected {
                error_msg.set(Some("Please connect your wallet first".to_string()));
                return;
            }

            let amount_val = (*amount).clone();
            if amount_val.is_empty() {
                error_msg.set(Some("Please enter an amount".to_string()));
                return;
            }

            // Clear previous error
            error_msg.set(None);
            is_loading.set(true);

            // Make real deposit transaction
            let is_loading = is_loading.clone();
            let deposit_id = deposit_id.clone();
            let tx_hash = tx_hash.clone();
            let error_msg = error_msg.clone();

            spawn_local(async move {
                // Check if we're on Sepolia
                if let Some(chain_id) = get_chain_id() {
                    if chain_id != SEPOLIA_CHAIN_ID_HEX {
                        // Try to switch to Sepolia
                        if let Err(e) = switch_to_sepolia().await {
                            error_msg.set(Some(format!("Please switch to Sepolia network: {}", e)));
                            is_loading.set(false);
                            return;
                        }
                    }
                }

                // Convert ETH to Wei
                let wei = match eth_to_wei(&amount_val) {
                    Ok(w) => w,
                    Err(e) => {
                        error_msg.set(Some(format!("Invalid amount: {}", e)));
                        is_loading.set(false);
                        return;
                    },
                };

                // Format Wei as hex
                let wei_hex = format!("0x{:x}", wei.parse::<u128>().unwrap_or(0));

                // Make deposit (recipient on the Acki Nacki side is msg.sender)
                match make_deposit(&wei_hex, ACKI_NACKI_RECEIVER).await {
                    Ok(hash) => {
                        tx_hash.set(Some(hash.clone()));
                        // Note: In a real implementation, we'd wait for the transaction
                        // to be mined and extract the deposit ID from the event logs
                        deposit_id.set(Some(0)); // Placeholder
                        is_loading.set(false);

                        web_sys::console::log_1(
                            &format!("Deposit successful! TX: {}", hash).into(),
                        );
                    },
                    Err(e) => {
                        error_msg.set(Some(format!("Transaction failed: {}", e)));
                        is_loading.set(false);
                        web_sys::console::error_1(&format!("Deposit error: {}", e).into());
                    },
                }
            });
        })
    };

    html! {
        <div class="form-container">
            <div class="form-header">
                <h2>{"Deposit to Acki Nacki"}</h2>
                <p class="form-description">
                    {"Bridge your ETH from Ethereum to Acki Nacki blockchain"}
                </p>
            </div>

            <form onsubmit={on_submit}>
                <div class="form-group">
                    <label class="form-label">
                        {"Amount (ETH)"}
                    </label>
                    <div class="input-wrapper">
                        <input
                            type="text"
                            class="form-input"
                            placeholder="0.0"
                            value={(*amount).clone()}
                            onchange={on_amount_change}
                            disabled={!props.wallet_connected || *is_loading}
                        />
                        <span class="input-suffix">{"ETH"}</span>
                    </div>
                    <div class="input-hint">
                        {"Funds will be bridged to Acki Nacki for your connected wallet address"}
                    </div>
                </div>

                {
                    if let Some(err) = (*error_msg).as_ref() {
                        html! {
                            <div class="error-box">
                                <div class="error-icon">{"⚠"}</div>
                                <div class="error-content">
                                    <p>{err}</p>
                                </div>
                            </div>
                        }
                    } else {
                        html! {}
                    }
                }

                <div class="info-box">
                    <div class="info-row">
                        <span>{"Network"}</span>
                        <span class="info-value">{"Sepolia Testnet"}</span>
                    </div>
                    <div class="info-row">
                        <span>{"Estimated Time"}</span>
                        <span class="info-value">{"~2 minutes"}</span>
                    </div>
                </div>

                {
                    if let Some(id) = *deposit_id {
                        html! {
                            <div class="success-box">
                                <div class="success-icon">{"✓"}</div>
                                <div class="success-content">
                                    <h3>{"Deposit Successful!"}</h3>
                                    <p>{"Deposit reference: "}{id}</p>
                                    <p class="tx-hash">
                                        {"Transaction: "}
                                        <a href={format!("https://sepolia.etherscan.io/tx/{}", tx_hash.as_ref().unwrap())} target="_blank">
                                            {tx_hash.as_ref().unwrap()}
                                        </a>
                                    </p>
                                </div>
                            </div>
                        }
                    } else {
                        html! {}
                    }
                }

                <button
                    type="submit"
                    class="submit-button"
                    disabled={!props.wallet_connected || *is_loading || (*amount).is_empty()}
                >
                    {
                        if *is_loading {
                            html! {
                                <>
                                    <span class="spinner"></span>
                                    {"Processing..."}
                                </>
                            }
                        } else if !props.wallet_connected {
                            html! { "Connect Wallet to Deposit" }
                        } else {
                            html! { "Deposit" }
                        }
                    }
                </button>
            </form>

            <div class="help-text">
                <p>{"💡 Your deposit will be assigned a unique Deposit ID as a reference for tracking it on Acki Nacki."}</p>
            </div>
        </div>
    }
}
