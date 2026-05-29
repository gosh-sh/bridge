use yew::prelude::*;

#[function_component(TransactionHistory)]
pub fn transaction_history() -> Html {
    html! {
        <div class="history-container">
            <h2 class="history-title">{"Recent Transactions"}</h2>

            <div class="empty-state">
                <p>{"No transactions yet."}</p>
                <p class="empty-state-hint">
                    {"Your bridge activity will appear here once on-chain history is connected."}
                </p>
            </div>
        </div>
    }
}
