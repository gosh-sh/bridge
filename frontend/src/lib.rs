use yew::prelude::*;
use wasm_bindgen_futures::spawn_local;

mod components;
mod hooks;
mod utils;
mod config;
mod web3;

use components::{Header, DepositForm, WithdrawForm, Stats, TransactionHistory};
use web3::{connect_wallet, is_metamask_installed};

#[derive(Clone, PartialEq)]
pub enum Tab {
    Deposit,
    Withdraw,
}

#[function_component(App)]
fn app() -> Html {
    let active_tab = use_state(|| Tab::Deposit);
    let wallet_connected = use_state(|| false);
    let wallet_address = use_state(|| None::<String>);

    let on_tab_change = {
        let active_tab = active_tab.clone();
        Callback::from(move |tab: Tab| {
            active_tab.set(tab);
        })
    };

    let on_connect_wallet = {
        let wallet_connected = wallet_connected.clone();
        let wallet_address = wallet_address.clone();
        Callback::from(move |_| {
            web_sys::console::log_1(&"Connect Wallet button clicked".into());

            // Check if MetaMask is installed
            let is_installed = is_metamask_installed();
            web_sys::console::log_1(&format!("is_metamask_installed() returned: {}", is_installed).into());

            if !is_installed {
                web_sys::console::error_1(&"MetaMask not detected!".into());
                web_sys::window()
                    .and_then(|w| w.alert_with_message("Please install MetaMask to use this bridge!").ok());
                return;
            }

            web_sys::console::log_1(&"MetaMask detected, proceeding with connection...".into());

            // Connect to MetaMask
            let wallet_connected = wallet_connected.clone();
            let wallet_address = wallet_address.clone();

            spawn_local(async move {
                match connect_wallet().await {
                    Ok(address) => {
                        // Format address for display (0x1234...5678)
                        let formatted = if address.len() > 10 {
                            format!("{}...{}", &address[0..6], &address[address.len()-4..])
                        } else {
                            address.clone()
                        };

                        wallet_connected.set(true);
                        wallet_address.set(Some(formatted));

                        web_sys::console::log_1(&format!("Connected to wallet: {}", address).into());
                    }
                    Err(e) => {
                        web_sys::console::error_1(&format!("Failed to connect wallet: {}", e).into());
                        web_sys::window()
                            .and_then(|w| w.alert_with_message(&format!("Failed to connect: {}", e)).ok());
                    }
                }
            });
        })
    };

    html! {
        <div class="app">
            <Header 
                wallet_connected={*wallet_connected}
                wallet_address={(*wallet_address).clone()}
                on_connect={on_connect_wallet}
            />
            
            <main class="container">
                <div class="hero">
                    <h1>{"Acki Nacki Bridge"}</h1>
                    <p class="subtitle">{"Secure cross-chain bridge between Ethereum and Acki Nacki"}</p>
                </div>

                <Stats />

                <div class="bridge-card">
                    <div class="tabs">
                        <button 
                            class={if *active_tab == Tab::Deposit { "tab active" } else { "tab" }}
                            onclick={let tab = on_tab_change.clone(); move |_| tab.emit(Tab::Deposit)}
                        >
                            {"Deposit"}
                        </button>
                        <button 
                            class={if *active_tab == Tab::Withdraw { "tab active" } else { "tab" }}
                            onclick={let tab = on_tab_change.clone(); move |_| tab.emit(Tab::Withdraw)}
                        >
                            {"Withdraw"}
                        </button>
                    </div>

                    <div class="tab-content">
                        {
                            match *active_tab {
                                Tab::Deposit => html! { <DepositForm wallet_connected={*wallet_connected} /> },
                                Tab::Withdraw => html! { <WithdrawForm wallet_connected={*wallet_connected} /> },
                            }
                        }
                    </div>
                </div>

                <TransactionHistory />
            </main>

            <footer class="footer">
                <p>{"Built with ❤️ using Rust + Yew + WebAssembly"}</p>
                <p class="links">
                    <a href="https://github.com/your-repo" target="_blank">{"GitHub"}</a>
                    {" • "}
                    <a href="https://docs.your-bridge.com" target="_blank">{"Docs"}</a>
                    {" • "}
                    <a href="https://discord.gg/your-bridge" target="_blank">{"Discord"}</a>
                </p>
            </footer>
        </div>
    }
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn run_app() {
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<App>::new().render();
}

