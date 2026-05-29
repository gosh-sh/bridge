use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct HeaderProps {
    pub wallet_connected: bool,
    pub wallet_address: Option<String>,
    pub on_connect: Callback<()>,
}

#[function_component(Header)]
pub fn header(props: &HeaderProps) -> Html {
    let network_name = "Sepolia Testnet";

    html! {
        <header class="header">
            <div class="header-content">
                <div class="logo">
                    <svg width="40" height="40" viewBox="0 0 40 40" fill="none">
                        <circle cx="20" cy="20" r="18" stroke="url(#gradient)" stroke-width="3"/>
                        <path d="M15 20L18 23L25 16" stroke="url(#gradient)" stroke-width="3" stroke-linecap="round"/>
                        <defs>
                            <@{"linearGradient"} id="gradient" x1="0" y1="0" x2="40" y2="40">
                                <stop offset="0%" stop-color="#667eea"/>
                                <stop offset="100%" stop-color="#764ba2"/>
                            </@>
                        </defs>
                    </svg>
                    <span class="logo-text">{"Acki Nacki Bridge"}</span>
                </div>

                <div class="header-right">
                    <div class="network-badge">
                        <span class="network-dot"></span>
                        {network_name}
                    </div>

                    {
                        if props.wallet_connected {
                            html! {
                                <div class="wallet-info">
                                    <div class="wallet-badge">
                                        <span class="wallet-icon">{"🔐"}</span>
                                        {props.wallet_address.as_ref().unwrap_or(&"Unknown".to_string())}
                                    </div>
                                </div>
                            }
                        } else {
                            html! {
                                <button class="connect-button" onclick={props.on_connect.reform(|_| ())}>
                                    {"Connect Wallet"}
                                </button>
                            }
                        }
                    }
                </div>
            </div>
        </header>
    }
}
