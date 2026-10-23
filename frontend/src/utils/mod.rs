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

/// Validate and normalise a non-zero 256-bit Acki Nacki account ID.
pub(crate) fn validate_an_account(raw: &str) -> Result<String, String> {
    let v = raw.trim().trim_start_matches("0x").to_lowercase();
    if v.is_empty() {
        return Err("Acki Nacki account is required".to_string());
    }
    if v.len() != 64 {
        return Err(format!(
            "Account must be exactly 64 hex chars (256 bits); got {}. An Ethereum address is 40 \
             and is not a valid AN recipient.",
            v.len()
        ));
    }
    if !v.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Account must be hex (0-9, a-f)".to_string());
    }
    if v.chars().all(|c| c == '0') {
        return Err("Account must be non-zero".to_string());
    }
    Ok(v)
}

pub(crate) fn validate_an_target(raw: &str) -> Result<String, String> {
    let (dapp, account) = raw
        .trim()
        .split_once("::")
        .ok_or("Use dapp_id::account_id, with exactly 64 hex chars for each part")?;
    let dapp = dapp.trim_start_matches("0x").to_lowercase();
    if dapp.len() != 64 || !dapp.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("DApp ID must be exactly 64 hex chars".into());
    }
    Ok(format!("{dapp}::{}", validate_an_account(account)?))
}

#[cfg(test)]
mod tests {
    use super::{validate_an_account, validate_an_target};

    #[test]
    fn a_target_requires_both_full_identifiers_and_a_nonzero_account() {
        let account = "a3".repeat(32);
        assert!(validate_an_target(&account).is_err());
        for dapp in ["00".repeat(32), "ff".repeat(32)] {
            let target = format!("{dapp}::{account}");
            assert_eq!(validate_an_target(&target).unwrap(), target);
        }
        assert!(validate_an_target(&format!("1::{account}")).is_err());
        assert!(validate_an_target(&format!("{}::{}", "ff".repeat(32), "00".repeat(32))).is_err());
    }

    /// The mistake this guard exists for: a pasted Ethereum address is 40 hex
    /// chars, non-zero, and all-hex, so every other check passes it.
    #[test]
    fn eth_address_is_rejected_for_length() {
        let err = validate_an_account("0x0F4F8b7EF2E40587ff1cC5d3393b9c1Fb8f02fc7")
            .expect_err("40 hex chars must not pass as an AN account");
        assert!(err.contains("exactly 64"), "{err}");
    }

    #[test]
    fn short_and_long_are_rejected() {
        assert!(validate_an_account(&"a".repeat(63)).is_err());
        assert!(validate_an_account(&"a".repeat(65)).is_err());
    }

    #[test]
    fn full_width_account_is_accepted_and_normalised() {
        let raw = format!("0x{}", "AB".repeat(32));
        assert_eq!(validate_an_account(&raw).unwrap(), "ab".repeat(32));
    }

    #[test]
    fn zero_account_is_rejected_even_at_full_width() {
        assert!(validate_an_account(&"0".repeat(64)).is_err());
    }
}
