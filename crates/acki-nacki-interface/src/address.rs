//! Acki Nacki extended address form (`dapp_id::account_id`) required by
//! tvm-sdk / tvm-cli 3.0.0+.
//!
//! See <https://github.com/tvmlabs/tvm-sdk/blob/main/docs/MIGRATION-3.0.md>.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{AckiNackiError, Result};

/// Extended Acki Nacki address: `<dapp_id_hex64>::<account_id_hex64>`.
///
/// Both halves are strict 64-character hex (no `0x`, no workchain prefix).
/// For self-rooted contracts `dapp_id == account_id`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ExtendedAddress {
    dapp_id: String,
    account_id: String,
}

impl ExtendedAddress {
    /// Parse the strict `dapp_id::account_id` form required by SDK 3.0.
    pub fn parse(input: &str) -> Result<Self> {
        Self::from_str(input)
    }

    /// 64-hex dapp identifier (no `0x`).
    pub fn dapp_id(&self) -> &str {
        &self.dapp_id
    }

    /// 64-hex account identifier (no workchain, no `0x`).
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Legacy workchain address `0:<account_id>` for tvm_client message
    /// encoding.
    pub fn workchain_address(&self) -> String {
        format!("0:{}", self.account_id)
    }

    /// Convert a legacy `0:<hex>` or bare-hex address into the extended form.
    ///
    /// When `dapp_id` is omitted the account half is reused (self-rooted
    /// convention). Pass an explicit `dapp_id` when the contract lives inside
    /// an existing dApp.
    pub fn from_legacy(account: &str, dapp_id: Option<&str>) -> Result<Self> {
        let account_id = strip_to_account_hex(account)?;
        let dapp = match dapp_id {
            Some(d) => strip_to_account_hex(d)?,
            None => account_id.clone(),
        };
        Ok(Self {
            dapp_id: dapp,
            account_id,
        })
    }
}

impl FromStr for ExtendedAddress {
    type Err = AckiNackiError;

    fn from_str(input: &str) -> Result<Self> {
        let (dapp, account) = input
            .split_once("::")
            .ok_or_else(|| invalid_address(input))?;
        if !is_hex64(dapp) || !is_hex64(account) {
            return Err(invalid_address(input));
        }
        Ok(Self {
            dapp_id: dapp.to_owned(),
            account_id: account.to_owned(),
        })
    }
}

impl fmt::Display for ExtendedAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.dapp_id, self.account_id)
    }
}

impl Serialize for ExtendedAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ExtendedAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

fn invalid_address(input: &str) -> AckiNackiError {
    AckiNackiError::InvalidAddress(format!(
        "address `{input}` must be in the form `dapp_id::account_id` (two 64-character hex \
         strings separated by `::`, no `0x`, no workchain)"
    ))
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Strip `0:<hex>` / `0x<hex>` / bare-hex down to a 64-hex account id.
fn strip_to_account_hex(s: &str) -> Result<String> {
    let bare = s
        .strip_prefix("0:")
        .or_else(|| s.strip_prefix("0x"))
        .unwrap_or(s);
    if !is_hex64(bare) {
        return Err(AckiNackiError::InvalidAddress(format!(
            "expected 64-character hex account id, got `{s}`"
        )));
    }
    Ok(bare.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAPP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const ACC: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    #[test]
    fn parses_extended_form() {
        let addr = ExtendedAddress::parse(&format!("{DAPP}::{ACC}")).unwrap();
        assert_eq!(addr.dapp_id(), DAPP);
        assert_eq!(addr.account_id(), ACC);
        assert_eq!(addr.to_string(), format!("{DAPP}::{ACC}"));
    }

    #[test]
    fn rejects_legacy_workchain_form() {
        assert!(ExtendedAddress::parse(&format!("0:{ACC}")).is_err());
    }

    #[test]
    fn from_legacy_self_rooted() {
        let addr = ExtendedAddress::from_legacy(&format!("0:{ACC}"), None).unwrap();
        assert_eq!(addr.dapp_id(), ACC);
        assert_eq!(addr.account_id(), ACC);
    }

    #[test]
    fn from_legacy_with_explicit_dapp() {
        let addr = ExtendedAddress::from_legacy(&format!("0:{ACC}"), Some(DAPP)).unwrap();
        assert_eq!(addr.dapp_id(), DAPP);
        assert_eq!(addr.account_id(), ACC);
    }

    #[test]
    fn workchain_address_prepends_zero() {
        let addr = ExtendedAddress::parse(&format!("{DAPP}::{ACC}")).unwrap();
        assert_eq!(addr.workchain_address(), format!("0:{ACC}"));
    }
}
