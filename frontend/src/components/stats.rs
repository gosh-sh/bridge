use yew::prelude::*;

#[function_component(Stats)]
pub fn stats() -> Html {
    html! {
        <div class="stats-container">
            <div class="stat-card">
                <div class="stat-icon">{"💰"}</div>
                <div class="stat-content">
                    <div class="stat-value">{"1,234.56"}</div>
                    <div class="stat-label">{"Total Value Locked (USDT)"}</div>
                </div>
            </div>

            <div class="stat-card">
                <div class="stat-icon">{"🔄"}</div>
                <div class="stat-content">
                    <div class="stat-value">{"5,678"}</div>
                    <div class="stat-label">{"Total Transactions"}</div>
                </div>
            </div>

            <div class="stat-card">
                <div class="stat-icon">{"👥"}</div>
                <div class="stat-content">
                    <div class="stat-value">{"892"}</div>
                    <div class="stat-label">{"Unique Users"}</div>
                </div>
            </div>

            <div class="stat-card">
                <div class="stat-icon">{"⚡"}</div>
                <div class="stat-content">
                    <div class="stat-value">{"~2 min"}</div>
                    <div class="stat-label">{"Avg. Bridge Time"}</div>
                </div>
            </div>
        </div>
    }
}
