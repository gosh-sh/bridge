// Utility functions for formatting, validation, etc.

pub fn format_address(address: &str) -> String {
    if address.len() > 10 {
        format!("{}...{}", &address[..6], &address[address.len() - 4..])
    } else {
        address.to_string()
    }
}

pub fn format_eth_amount(amount: &str) -> String {
    // Add proper ETH formatting
    amount.to_string()
}
