use yew::prelude::*;
use web_sys::HtmlInputElement;

#[derive(Properties, PartialEq)]
pub struct WithdrawFormProps {
    pub wallet_connected: bool,
}

#[function_component(WithdrawForm)]
pub fn withdraw_form(props: &WithdrawFormProps) -> Html {
    let deposit_id = use_state(|| String::new());
    let amount = use_state(|| String::new());
    let is_loading = use_state(|| false);
    let is_generating_proof = use_state(|| false);
    let tx_hash = use_state(|| None::<String>);

    let on_deposit_id_change = {
        let deposit_id = deposit_id.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            deposit_id.set(input.value());
        })
    };

    let on_amount_change = {
        let amount = amount.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            amount.set(input.value());
        })
    };

    let on_submit = {
        let deposit_id = deposit_id.clone();
        let amount = amount.clone();
        let is_loading = is_loading.clone();
        let is_generating_proof = is_generating_proof.clone();
        let tx_hash = tx_hash.clone();
        let wallet_connected = props.wallet_connected;

        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();

            if !wallet_connected {
                return;
            }

            let deposit_id_val = (*deposit_id).clone();
            let amount_val = (*amount).clone();
            
            if deposit_id_val.is_empty() || amount_val.is_empty() {
                return;
            }

            is_generating_proof.set(true);
            
            // Simulate proof generation and withdrawal
            let is_loading = is_loading.clone();
            let is_generating_proof = is_generating_proof.clone();
            let tx_hash = tx_hash.clone();
            
            wasm_bindgen_futures::spawn_local(async move {
                // Step 1: Generate proof (3 seconds)
                gloo_timers::future::TimeoutFuture::new(3000).await;
                is_generating_proof.set(false);
                is_loading.set(true);
                
                // Step 2: Submit withdrawal (2 seconds)
                gloo_timers::future::TimeoutFuture::new(2000).await;
                
                // Simulate successful withdrawal
                tx_hash.set(Some("0x1234...5678".to_string()));
                is_loading.set(false);
            });
        })
    };

    html! {
        <div class="form-container">
            <div class="form-header">
                <h2>{"Withdraw from Acki Nacki"}</h2>
                <p class="form-description">
                    {"Withdraw your funds back to Ethereum using ZK proof"}
                </p>
            </div>

            <form onsubmit={on_submit}>
                <div class="form-group">
                    <label class="form-label">
                        {"Deposit ID"}
                    </label>
                    <input 
                        type="text"
                        class="form-input"
                        placeholder="Enter your deposit ID"
                        value={(*deposit_id).clone()}
                        onchange={on_deposit_id_change}
                        disabled={!props.wallet_connected || *is_loading || *is_generating_proof}
                    />
                    <div class="input-hint">
                        {"The unique ID you received when depositing"}
                    </div>
                </div>

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
                            disabled={!props.wallet_connected || *is_loading || *is_generating_proof}
                        />
                        <span class="input-suffix">{"ETH"}</span>
                    </div>
                </div>

                <div class="info-box">
                    <div class="info-row">
                        <span>{"Network Fee"}</span>
                        <span class="info-value">{"~0.003 ETH"}</span>
                    </div>
                    <div class="info-row">
                        <span>{"ZK Proof Generation"}</span>
                        <span class="info-value">{"~3 seconds"}</span>
                    </div>
                    <div class="info-row">
                        <span>{"Estimated Time"}</span>
                        <span class="info-value">{"~5 minutes"}</span>
                    </div>
                </div>

                {
                    if *is_generating_proof {
                        html! {
                            <div class="progress-box">
                                <div class="progress-icon">
                                    <span class="spinner large"></span>
                                </div>
                                <div class="progress-content">
                                    <h3>{"Generating ZK Proof..."}</h3>
                                    <p>{"This may take a few seconds"}</p>
                                    <div class="progress-bar">
                                        <div class="progress-fill"></div>
                                    </div>
                                </div>
                            </div>
                        }
                    } else if let Some(hash) = (*tx_hash).as_ref() {
                        html! {
                            <div class="success-box">
                                <div class="success-icon">{"✓"}</div>
                                <div class="success-content">
                                    <h3>{"Withdrawal Successful!"}</h3>
                                    <p class="tx-hash">
                                        {"Transaction: "}
                                        <a href={format!("https://sepolia.etherscan.io/tx/{}", hash)} target="_blank">
                                            {hash}
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
                    disabled={!props.wallet_connected || *is_loading || *is_generating_proof || (*deposit_id).is_empty() || (*amount).is_empty()}
                >
                    {
                        if *is_generating_proof {
                            html! { 
                                <>
                                    <span class="spinner"></span>
                                    {"Generating Proof..."}
                                </>
                            }
                        } else if *is_loading {
                            html! { 
                                <>
                                    <span class="spinner"></span>
                                    {"Processing Withdrawal..."}
                                </>
                            }
                        } else if !props.wallet_connected {
                            html! { "Connect Wallet to Withdraw" }
                        } else {
                            html! { "Withdraw" }
                        }
                    }
                </button>
            </form>

            <div class="help-text">
                <p>{"🔒 Zero-knowledge proofs ensure your withdrawal is verified without revealing sensitive information."}</p>
            </div>
        </div>
    }
}

