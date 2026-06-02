use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::config::{SEPOLIA_CHAIN_ID_HEX, USDT_UNIT};
use crate::web3::{
    approve_usdt, get_allowance, get_chain_id, get_current_account, get_deposit_counter,
    make_deposit, mint_test_usdt, switch_to_sepolia, usdt_to_units, wait_for_receipt,
};

/// Maximum deposit (100 USDT in base units), mirrors `MAX_DEPOSIT_AMOUNT`.
const MAX_USDT_UNITS: u128 = 100 * USDT_UNIT;

/// Default faucet mint amount (100 USDT).
const FAUCET_USDT_UNITS: u128 = 100 * USDT_UNIT;

#[derive(Properties, PartialEq)]
pub struct DepositFormProps {
    pub wallet_connected: bool,
}

#[function_component(DepositForm)]
pub fn deposit_form(props: &DepositFormProps) -> Html {
    let amount = use_state(String::new);
    let deposit_id = use_state(|| None::<u128>);
    let is_loading = use_state(|| false);
    let status_msg = use_state(|| None::<String>);
    let tx_hash = use_state(|| None::<String>);
    let error_msg = use_state(|| None::<String>);

    let on_amount_change = {
        let amount = amount.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            amount.set(input.value());
        })
    };

    // "Get test USDT" — mint from the Aave Sepolia faucet to the connected wallet.
    let on_faucet = {
        let is_loading = is_loading.clone();
        let status_msg = status_msg.clone();
        let error_msg = error_msg.clone();
        let wallet_connected = props.wallet_connected;

        Callback::from(move |_: MouseEvent| {
            if !wallet_connected {
                error_msg.set(Some("Please connect your wallet first".to_string()));
                return;
            }
            error_msg.set(None);
            is_loading.set(true);

            let is_loading = is_loading.clone();
            let status_msg = status_msg.clone();
            let error_msg = error_msg.clone();
            spawn_local(async move {
                status_msg.set(Some("Minting 100 test USDT from the faucet…".to_string()));
                match mint_test_usdt(FAUCET_USDT_UNITS).await {
                    Ok(hash) => match wait_for_receipt(&hash).await {
                        Ok(()) => status_msg.set(Some("Minted 100 test USDT ✓".to_string())),
                        Err(e) => {
                            error_msg.set(Some(format!("Faucet mint not confirmed: {}", e)))
                        },
                    },
                    Err(e) => error_msg.set(Some(format!("Faucet mint failed: {}", e))),
                }
                is_loading.set(false);
            });
        })
    };

    let on_submit = {
        let amount = amount.clone();
        let is_loading = is_loading.clone();
        let deposit_id = deposit_id.clone();
        let status_msg = status_msg.clone();
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
            let units = match usdt_to_units(&amount_val) {
                Ok(u) => u,
                Err(err) => {
                    error_msg.set(Some(format!("Invalid amount: {}", err)));
                    return;
                },
            };
            if units == 0 {
                error_msg.set(Some("Amount must be greater than 0".to_string()));
                return;
            }
            if units > MAX_USDT_UNITS {
                error_msg.set(Some("Amount exceeds the 100 USDT per-deposit limit".to_string()));
                return;
            }

            error_msg.set(None);
            deposit_id.set(None);
            tx_hash.set(None);
            is_loading.set(true);

            let is_loading = is_loading.clone();
            let deposit_id = deposit_id.clone();
            let status_msg = status_msg.clone();
            let tx_hash = tx_hash.clone();
            let error_msg = error_msg.clone();

            spawn_local(async move {
                // Ensure we're on Sepolia.
                if let Some(chain_id) = get_chain_id() {
                    if chain_id != SEPOLIA_CHAIN_ID_HEX {
                        if let Err(e) = switch_to_sepolia().await {
                            error_msg.set(Some(format!("Please switch to Sepolia network: {}", e)));
                            is_loading.set(false);
                            return;
                        }
                    }
                }

                let account = match get_current_account() {
                    Some(a) => a,
                    None => {
                        error_msg.set(Some("No account connected".to_string()));
                        is_loading.set(false);
                        return;
                    },
                };

                // Step 1: approve the bridge for `units` USDT if the current
                // allowance is insufficient.
                let allowance = get_allowance(&account).await.unwrap_or(0);
                if allowance < units {
                    status_msg.set(Some("Step 1/2 — approve USDT (confirm in wallet)…".to_string()));
                    match approve_usdt(units).await {
                        Ok(hash) => {
                            status_msg.set(Some("Waiting for approval to confirm…".to_string()));
                            if let Err(e) = wait_for_receipt(&hash).await {
                                error_msg.set(Some(format!("Approval failed: {}", e)));
                                is_loading.set(false);
                                return;
                            }
                        },
                        Err(e) => {
                            error_msg.set(Some(format!("Approval failed: {}", e)));
                            is_loading.set(false);
                            return;
                        },
                    }
                }

                // Step 2: deposit.
                status_msg.set(Some("Step 2/2 — deposit (confirm in wallet)…".to_string()));
                match make_deposit(units).await {
                    Ok(hash) => {
                        tx_hash.set(Some(hash.clone()));
                        status_msg.set(Some("Waiting for deposit to confirm…".to_string()));
                        match wait_for_receipt(&hash).await {
                            Ok(()) => {
                                // Most recent deposit id = depositCounter - 1.
                                if let Ok(counter) = get_deposit_counter().await {
                                    deposit_id.set(Some(counter.saturating_sub(1)));
                                }
                                status_msg.set(None);
                                web_sys::console::log_1(
                                    &format!("Deposit successful! TX: {}", hash).into(),
                                );
                            },
                            Err(e) => error_msg.set(Some(format!("Deposit not confirmed: {}", e))),
                        }
                        is_loading.set(false);
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
                    {"Bridge your USDT from Ethereum to the Acki Nacki blockchain"}
                </p>
            </div>

            <form onsubmit={on_submit}>
                <div class="form-group">
                    <label class="form-label">
                        {"Amount (USDT)"}
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
                        <span class="input-suffix">{"USDT"}</span>
                    </div>
                    <div class="input-hint">
                        {"Max 100 USDT per deposit. Gas is paid in ETH. Credited to your connected wallet on Acki Nacki."}
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

                {
                    if let Some(status) = (*status_msg).as_ref() {
                        html! {
                            <div class="info-box">
                                <div class="info-row">
                                    <span>{"Status"}</span>
                                    <span class="info-value">{status}</span>
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
                        <span>{"Flow"}</span>
                        <span class="info-value">{"approve → deposit (2 txns)"}</span>
                    </div>
                </div>

                {
                    if let Some(id) = *deposit_id {
                        html! {
                            <div class="success-box">
                                <div class="success-icon">{"✓"}</div>
                                <div class="success-content">
                                    <h3>{"Deposit Successful!"}</h3>
                                    <p>{"Deposit ID: "}{id}</p>
                                    {
                                        if let Some(hash) = tx_hash.as_ref() {
                                            html! {
                                                <p class="tx-hash">
                                                    {"Transaction: "}
                                                    <a href={format!("https://sepolia.etherscan.io/tx/{}", hash)} target="_blank">
                                                        {hash}
                                                    </a>
                                                </p>
                                            }
                                        } else {
                                            html! {}
                                        }
                                    }
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
                                    {"Processing…"}
                                </>
                            }
                        } else if !props.wallet_connected {
                            html! { "Connect Wallet to Deposit" }
                        } else {
                            html! { "Approve & Deposit" }
                        }
                    }
                </button>

                <button
                    type="button"
                    class="submit-button secondary"
                    onclick={on_faucet}
                    disabled={!props.wallet_connected || *is_loading}
                >
                    { "Get 100 test USDT (faucet)" }
                </button>
            </form>

            <div class="help-text">
                <p>{"💡 No test USDT? Use the faucet button above, then approve and deposit. The relayer picks up your deposit automatically — you don't need to track the Deposit ID."}</p>
            </div>
        </div>
    }
}
