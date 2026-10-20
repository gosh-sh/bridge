use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::{
    config::{
        bridge_contract_address, ACKI_NACKI_RECEIVER, MAX_TRANSFER_USDC_UNITS, SEPOLIA_CHAIN_ID_HEX,
    },
    utils::validate_an_account,
    web3::{
        approve_usdc, get_allowance, get_chain_id, get_current_account, get_deposit_counter,
        make_deposit, switch_to_sepolia, usdc_to_units, wait_for_receipt,
    },
};

#[derive(Properties, PartialEq)]
pub struct DepositFormProps {
    pub wallet_connected: bool,
}

#[function_component(DepositForm)]
pub fn deposit_form(props: &DepositFormProps) -> Html {
    let deployment_error = bridge_contract_address().err();
    let amount = use_state(String::new);
    let an_account = use_state(|| {
        if ACKI_NACKI_RECEIVER.is_empty() {
            String::new()
        } else {
            ACKI_NACKI_RECEIVER.to_string()
        }
    });
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

    let on_account_change = {
        let an_account = an_account.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            an_account.set(input.value());
        })
    };

    let on_submit = {
        let amount = amount.clone();
        let an_account = an_account.clone();
        let is_loading = is_loading.clone();
        let deposit_id = deposit_id.clone();
        let status_msg = status_msg.clone();
        let tx_hash = tx_hash.clone();
        let error_msg = error_msg.clone();
        let wallet_connected = props.wallet_connected;

        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();

            if let Err(err) = bridge_contract_address() {
                error_msg.set(Some(err));
                return;
            }

            if !wallet_connected {
                error_msg.set(Some("Please connect your wallet first".to_string()));
                return;
            }

            let amount_val = (*amount).clone();
            let units = match usdc_to_units(&amount_val) {
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
            if units > MAX_TRANSFER_USDC_UNITS {
                error_msg.set(Some(
                    "Amount exceeds the 50 USDC per-transfer limit".to_string(),
                ));
                return;
            }

            let an_recipient = match validate_an_account(&an_account) {
                Ok(a) => a,
                Err(err) => {
                    error_msg.set(Some(err));
                    return;
                },
            };

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

                let evm_account = match get_current_account() {
                    Some(a) => a,
                    None => {
                        error_msg.set(Some("No account connected".to_string()));
                        is_loading.set(false);
                        return;
                    },
                };

                // Step 1: approve the bridge for `units` USDC if the current
                // allowance is insufficient.
                let allowance = get_allowance(&evm_account).await.unwrap_or(0);
                if allowance < units {
                    status_msg.set(Some(
                        "Step 1/2 — approve USDC (confirm in wallet)…".to_string(),
                    ));
                    match approve_usdc(units).await {
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

                // Step 2: deposit to the Acki Nacki destination.
                status_msg.set(Some("Step 2/2 — deposit (confirm in wallet)…".to_string()));
                match make_deposit(units, &an_recipient).await {
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
                    {"Bridge your USDC from Ethereum to the Acki Nacki blockchain"}
                </p>
            </div>

            <form onsubmit={on_submit}>
                <div class="form-group">
                    <label class="form-label">
                        {"Amount (USDC)"}
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
                        <span class="input-suffix">{"USDC"}</span>
                    </div>
                    <div class="input-hint">
                        {"Maximum 50 USDC per transfer. Gas is paid in ETH."}
                    </div>
                </div>

                <div class="form-group">
                    <label class="form-label">
                        {"Acki Nacki recipient"}
                    </label>
                    <div class="input-wrapper">
                        <input
                            type="text"
                            class="form-input"
                            placeholder="0x… (64 hex chars, 256-bit AN account)"
                            value={(*an_account).clone()}
                            onchange={on_account_change}
                            disabled={!props.wallet_connected || *is_loading}
                        />
                        <span class="input-suffix">{"account"}</span>
                    </div>
                    <div class="input-hint">
                        {"Your destination on Acki Nacki: exactly 64 hex chars. An Ethereum \
                          address is 40 chars and is not a valid AN recipient. Check it before \
                          sending — a deposit to the wrong account is one-way, with no refund."}
                    </div>
                </div>

                {
                    if let Some(err) = (*error_msg).as_ref().or(deployment_error.as_ref()) {
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
                    disabled={!props.wallet_connected || *is_loading || deployment_error.is_some() || (*amount).is_empty() || (*an_account).is_empty()}
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

            </form>

            <div class="help-text">
                <p>
                    {"No test USDC? The shellnet bridge takes Circle Sepolia USDC from "}
                    <a href="https://faucet.circle.com/" target="_blank" rel="noopener">
                        {"faucet.circle.com"}
                    </a>
                    {". Approve and deposit after that. The relayer picks up your deposit automatically — you don't need to track the Deposit ID."}
                </p>
            </div>
        </div>
    }
}
