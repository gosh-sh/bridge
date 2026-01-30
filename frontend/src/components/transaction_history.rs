use yew::prelude::*;

#[derive(Clone, PartialEq)]
struct Transaction {
    id: u64,
    tx_type: String,
    amount: String,
    status: String,
    time: String,
    tx_hash: String,
}

#[function_component(TransactionHistory)]
pub fn transaction_history() -> Html {
    let transactions = vec![
        Transaction {
            id: 12345,
            tx_type: "Deposit".to_string(),
            amount: "0.5 ETH".to_string(),
            status: "Completed".to_string(),
            time: "2 hours ago".to_string(),
            tx_hash: "0xabcd...ef01".to_string(),
        },
        Transaction {
            id: 12344,
            tx_type: "Withdraw".to_string(),
            amount: "1.2 ETH".to_string(),
            status: "Completed".to_string(),
            time: "5 hours ago".to_string(),
            tx_hash: "0x1234...5678".to_string(),
        },
        Transaction {
            id: 12343,
            tx_type: "Deposit".to_string(),
            amount: "0.1 ETH".to_string(),
            status: "Pending".to_string(),
            time: "1 day ago".to_string(),
            tx_hash: "0x9876...4321".to_string(),
        },
    ];

    html! {
        <div class="history-container">
            <h2 class="history-title">{"Recent Transactions"}</h2>
            
            <div class="table-container">
                <table class="transaction-table">
                    <thead>
                        <tr>
                            <th>{"ID"}</th>
                            <th>{"Type"}</th>
                            <th>{"Amount"}</th>
                            <th>{"Status"}</th>
                            <th>{"Time"}</th>
                            <th>{"Transaction"}</th>
                        </tr>
                    </thead>
                    <tbody>
                        {
                            transactions.iter().map(|tx| {
                                let status_class = match tx.status.as_str() {
                                    "Completed" => "status-completed",
                                    "Pending" => "status-pending",
                                    _ => "status-failed",
                                };
                                
                                html! {
                                    <tr key={tx.id}>
                                        <td class="tx-id">{tx.id}</td>
                                        <td>
                                            <span class={format!("tx-type {}", if tx.tx_type == "Deposit" { "type-deposit" } else { "type-withdraw" })}>
                                                {&tx.tx_type}
                                            </span>
                                        </td>
                                        <td class="tx-amount">{&tx.amount}</td>
                                        <td>
                                            <span class={format!("status-badge {}", status_class)}>
                                                {&tx.status}
                                            </span>
                                        </td>
                                        <td class="tx-time">{&tx.time}</td>
                                        <td>
                                            <a 
                                                href={format!("https://sepolia.etherscan.io/tx/{}", &tx.tx_hash)} 
                                                target="_blank"
                                                class="tx-link"
                                            >
                                                {&tx.tx_hash}
                                            </a>
                                        </td>
                                    </tr>
                                }
                            }).collect::<Html>()
                        }
                    </tbody>
                </table>
            </div>
        </div>
    }
}

