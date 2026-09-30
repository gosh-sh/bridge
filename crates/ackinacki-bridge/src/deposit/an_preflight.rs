//! Preflight on the Acki Nacki side: the bridge answers over REST and
//! GraphQL, is new enough, is not paused, trusts this EVM bridge, deploys
//! the voucher this build knows, and the recipient is where `--to` says.

use alloy_primitives::{Address, U256};
use serde_json::json;

use crate::{
    deposit::{
        an::{AccStatus, AccountInfo, AnRead},
        args::AnTarget,
        identity::{BRIDGE_ABI, EXPECTED_VOUCHER_CODE_HASH},
        retry::transient,
        ui::Ui,
    },
    errors::{CliError, CliResult},
};

/// A bridge contract version, `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BridgeVersion(pub u32, pub u32, pub u32);

impl BridgeVersion {
    /// Reads `"1.5.0"`; anything else is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.trim().split('.').map(|p| p.parse::<u32>().ok());
        Some(BridgeVersion(it.next()??, it.next()??, it.next()??))
    }
}

impl std::fmt::Display for BridgeVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The first bridge release that cannot mint one deposit twice whatever
/// happens to its voucher code. Set when that release exists; until then
/// this build refuses every bridge unless built with `dev-unfixed-bridge`.
pub const MIN_BRIDGE_VERSION: Option<BridgeVersion> = None;
/// The newest bridge this build was checked against.
pub const NEWEST_KNOWN_BRIDGE_VERSION: BridgeVersion = BridgeVersion(1, 5, 0);

/// Judges a bridge version: `Err` refuses, `Ok` carries the warnings.
pub fn version_verdict(
    v: BridgeVersion,
    min: Option<BridgeVersion>,
    newest: BridgeVersion,
    dev_unfixed: bool,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    match min {
        None if dev_unfixed => warnings.push(format!(
            "development build: bridge {v} is accepted without the replay-protection check; never \
             ship this build"
        )),
        None => {
            return Err(format!(
                "this build does not know which bridge version it needs (bridge is {v}); it \
                 cannot retry finalizeDeposit safely. Install a released CLI"
            ))
        },
        Some(m) if v < m => {
            return Err(format!(
                "the bridge is version {v}; this CLI needs {m} or newer, which cannot mint one \
                 deposit twice"
            ))
        },
        Some(_) => {},
    }
    if v > newest {
        warnings.push(format!(
            "the bridge is version {v}, newer than {newest} this CLI was checked against"
        ));
    }
    Ok(warnings)
}

/// Where the recipient account stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientState {
    /// Deployed, in `dapp`, holding `ecc3` micro-USDC.
    Active {
        /// The dapp it lives in.
        dapp: [u8; 32],
        /// Its eccUSDC balance.
        ecc3: u128,
    },
    /// On chain without code.
    NotDeployed {
        /// Its state.
        status: AccStatus,
        /// Its eccUSDC balance.
        ecc3: u128,
    },
    /// Not on chain.
    Missing,
}

/// Judges the recipient: `Err` refuses, `Ok` carries a warning when the dapp
/// cannot be checked yet.
pub fn recipient_verdict(
    target: &AnTarget,
    info: Option<&AccountInfo>,
) -> Result<(RecipientState, Option<String>), String> {
    match info {
        Some(AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some(d),
            ecc3,
        }) => {
            if *d != target.dapp_id {
                return Err(format!(
                    "--to names dapp {}, but account {} lives in dapp {}",
                    target.dapp_hex(),
                    target.account_hex(),
                    hex::encode(d)
                ));
            }
            Ok((
                RecipientState::Active {
                    dapp: *d,
                    ecc3: *ecc3,
                },
                None,
            ))
        },
        Some(a) => Ok((
            RecipientState::NotDeployed {
                status: a.status,
                ecc3: a.ecc3,
            },
            Some(format!(
                "account {} is not deployed yet ({:?}); the eccUSDC will wait on it, and the dapp \
                 in --to cannot be checked until it is deployed",
                target.account_hex(),
                a.status
            )),
        )),
        None => Ok((
            RecipientState::Missing,
            Some(format!(
                "account {} does not exist yet; the deposit will create it with the eccUSDC on \
                 it, and it gets its dapp when it is deployed, so the dapp in --to cannot be \
                 checked now",
                target.account_hex()
            )),
        )),
    }
}

/// Reads `"0:<hex>"` into an account id; the zero address is `None`.
pub fn parse_an_address(s: &str) -> Option<[u8; 32]> {
    let (_, h) = s.split_once(':')?;
    let mut out = [0u8; 32];
    hex::decode_to_slice(h, &mut out).ok()?;
    (out != [0u8; 32]).then_some(out)
}

/// What the preflight learned about the Acki Nacki side.
#[derive(Debug, Clone)]
pub struct AnPreflight {
    /// The dapp the bridge account lives in.
    pub bridge_dapp: [u8; 32],
    /// The bridge version.
    pub version: BridgeVersion,
    /// The light client the bridge anchors through, if one is set.
    pub light_client: Option<[u8; 32]>,
    /// The owner may add anchors by hand.
    pub owner_anchors_enabled: bool,
    /// The code hash of the vouchers the bridge deploys.
    pub voucher_code_hash: [u8; 32],
    /// The recipient; `None` when resuming.
    pub recipient: Option<RecipientState>,
}

/// A preflight refusal (exit 2).
fn refuse(reason: String) -> CliError {
    CliError::Preflight {
        reason,
        source: None,
    }
}

/// Runs the preflight. When `resuming`, a pause, lost trust in the EVM bridge
/// and a moved voucher code are warnings and the recipient is not checked;
/// the version is checked either way.
#[allow(clippy::too_many_arguments)]
pub async fn run_with(
    an: &dyn AnRead,
    bridge: [u8; 32],
    chain_id: u64,
    evm_bridge: Address,
    target: &AnTarget,
    ui: &dyn Ui,
    min: Option<BridgeVersion>,
    dev_unfixed: bool,
    resuming: bool,
) -> CliResult<AnPreflight> {
    let hb = hex::encode(bridge);
    an.rest_probe(bridge).await.map_err(|e| {
        refuse(format!(
            "the Acki Nacki REST API (/v2/account) does not answer for the bridge {hb}: {e:#}"
        ))
    })?;
    let info = an
        .account(bridge)
        .await
        .map_err(|e| refuse(format!("the Acki Nacki GraphQL API does not answer: {e:#}")))?
        .ok_or_else(|| refuse(format!("bridge account {hb} not found over GraphQL")))?;
    let bridge_dapp = info.dapp_id.unwrap_or([0u8; 32]);
    let get = |f: &'static str, input: serde_json::Value| async move {
        transient(ui, "reading the Acki Nacki bridge", || {
            an.run_getter(bridge, BRIDGE_ABI, f, input.clone())
        })
        .await
    };
    let v = get("getVersion", json!({})).await;
    let version = v["value0"]
        .as_str()
        .and_then(BridgeVersion::parse)
        .ok_or_else(|| refuse(format!("getVersion answered {v}")))?;
    for w in
        version_verdict(version, min, NEWEST_KNOWN_BRIDGE_VERSION, dev_unfixed).map_err(refuse)?
    {
        ui.warn(&w);
    }
    if get("isPaused", json!({})).await["value0"].as_bool() != Some(false) {
        let msg =
            "the bridge is paused by its owner: deposits would not finalize until it is lifted";
        if !resuming {
            return Err(refuse(msg.into()));
        }
        ui.warn(&format!("{msg}; the resumed run waits for it"));
    }
    let evm_as_uint = U256::from_be_slice(evm_bridge.as_slice()).to_string();
    let trusted = get(
        "isTrustedL1Bridge",
        json!({ "chainId": chain_id.to_string(), "l1Bridge": evm_as_uint }),
    )
    .await;
    if trusted["value0"].as_bool() != Some(true) {
        let msg = format!(
            "the Acki Nacki bridge does not trust {evm_bridge} on chain {chain_id}: a deposit \
             there would never finalize"
        );
        if !resuming {
            return Err(refuse(msg));
        }
        ui.warn(&format!(
            "{msg} until its owner restores the allowlist; finalizeDeposit would be refused with \
             222"
        ));
    }
    let cfg = get("getAnchorConfig", json!({})).await;
    let light_client = cfg["lightClient"].as_str().and_then(parse_an_address);
    let owner_anchors_enabled = cfg["ownerAnchorsEnabled"]
        .as_bool()
        .ok_or_else(|| refuse(format!("getAnchorConfig answered {cfg}")))?;
    let ch = get("getDepositVoucherCodeHash", json!({})).await;
    let mut voucher_code_hash = [0u8; 32];
    hex::decode_to_slice(
        ch["value0"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("0x"),
        &mut voucher_code_hash,
    )
    .map_err(|_| refuse(format!("getDepositVoucherCodeHash answered {ch}")))?;
    if voucher_code_hash != EXPECTED_VOUCHER_CODE_HASH {
        let msg = format!(
            "the bridge deploys voucher code {}, this build knows {}: it cannot find this \
             deposit's voucher",
            hex::encode(voucher_code_hash),
            hex::encode(EXPECTED_VOUCHER_CODE_HASH)
        );
        if !resuming {
            return Err(refuse(format!("{msg}. Install a newer CLI")));
        }
        ui.warn(&format!(
            "{msg}; the credit will be confirmed by the bridge's events"
        ));
    }
    if resuming {
        // The deposit is made; the recipient is whatever it names.
        return Ok(AnPreflight {
            bridge_dapp,
            version,
            light_client,
            owner_anchors_enabled,
            voucher_code_hash,
            recipient: None,
        });
    }
    let r = transient(ui, "reading the recipient", || {
        an.account(target.account_id)
    })
    .await;
    let (recipient, warning) = recipient_verdict(target, r.as_ref()).map_err(refuse)?;
    if let Some(w) = warning {
        ui.warn(&w);
    }
    Ok(AnPreflight {
        bridge_dapp,
        version,
        light_client,
        owner_anchors_enabled,
        voucher_code_hash,
        recipient: Some(recipient),
    })
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;
    use serde_json::json;

    use super::*;
    use crate::deposit::{an::*, args::AnTarget, testkit::*, ui::RecordingUi};

    const BRIDGE: [u8; 32] = [0x1a; 32];
    const ACC: [u8; 32] = [0xa3; 32];

    fn healthy() -> FakeAn {
        let an = FakeAn::default();
        an.accounts.lock().unwrap().insert(BRIDGE, AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some([0; 32]),
            ecc3: 0,
        });
        an.getter(BRIDGE, "getVersion", vec![
            json!({ "value0": "1.6.0", "value1": "eccUSDCBridge" }),
        ]);
        an.getter(BRIDGE, "isPaused", vec![json!({ "value0": false })]);
        an.getter(BRIDGE, "isTrustedL1Bridge", vec![json!({ "value0": true })]);
        an.getter(BRIDGE, "getAnchorConfig", vec![
            json!({ "lightClient": format!("0:{}", "20".repeat(32)), "ownerAnchorsEnabled": true }),
        ]);
        an.getter(
            BRIDGE,
            "getDepositVoucherCodeHash",
            vec![json!({ "value0": format!("0x{}", hex::encode(crate::deposit::identity::EXPECTED_VOUCHER_CODE_HASH)) })],
        );
        an
    }

    async fn go(an: &FakeAn, dapp: [u8; 32]) -> CliResult<AnPreflight> {
        let ui = RecordingUi::new(true);
        let target = AnTarget {
            dapp_id: dapp,
            account_id: ACC,
        };
        let evm_bridge = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
        run_with(
            an,
            BRIDGE,
            11_155_111,
            evm_bridge,
            &target,
            &ui,
            Some(BridgeVersion(1, 6, 0)),
            false,
            false,
        )
        .await
    }

    #[tokio::test]
    async fn resuming_turns_pause_and_lost_trust_into_warnings() {
        let an = healthy();
        // A version inside the known range, so only the resume warnings count.
        an.getter(BRIDGE, "getVersion", vec![
            json!({ "value0": "1.5.0", "value1": "eccUSDCBridge" }),
        ]);
        an.getter(BRIDGE, "isPaused", vec![json!({ "value0": true })]);
        an.getter(BRIDGE, "isTrustedL1Bridge", vec![
            json!({ "value0": false }),
        ]);
        let ui = RecordingUi::new(true);
        let target = AnTarget {
            dapp_id: [9; 32],
            account_id: ACC,
        }; // not checked on resume
        let evm_bridge = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
        let min = Some(BridgeVersion(1, 5, 0));
        let p = run_with(
            &an, BRIDGE, 11_155_111, evm_bridge, &target, &ui, min, false, true,
        )
        .await
        .unwrap();
        assert!(p.recipient.is_none());
        assert_eq!(ui.warnings().len(), 2);
        // The version is not relaxed: a resend is only safe on a fixed bridge.
        an.getter(BRIDGE, "getVersion", vec![
            json!({ "value0": "1.4.0", "value1": "eccUSDCBridge" }),
        ]);
        assert!(
            run_with(&an, BRIDGE, 11_155_111, evm_bridge, &target, &ui, min, false, true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_moved_voucher_code_only_warns_on_resume() {
        let an = healthy();
        an.getter(BRIDGE, "getDepositVoucherCodeHash", vec![
            json!({ "value0": format!("0x{}", "11".repeat(32)) }),
        ]);
        let ui = RecordingUi::new(true);
        let target = AnTarget {
            dapp_id: [0; 32],
            account_id: ACC,
        };
        let evm_bridge = address!("0f4f8b7ef2e40587ff1cc5d3393b9c1fb8f02fc7");
        run_with(
            &an,
            BRIDGE,
            11_155_111,
            evm_bridge,
            &target,
            &ui,
            Some(BridgeVersion(1, 6, 0)),
            false,
            true,
        )
        .await
        .unwrap();
        assert!(ui.warnings().iter().any(|w| w.contains("voucher")));
    }

    #[test]
    fn version_rules() {
        let v = BridgeVersion::parse("1.5.0").unwrap();
        let min = Some(BridgeVersion(1, 6, 0));
        assert!(version_verdict(v, min, BridgeVersion(1, 6, 0), false).is_err());
        assert!(
            version_verdict(BridgeVersion(1, 6, 0), min, BridgeVersion(1, 6, 0), false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            version_verdict(BridgeVersion(1, 7, 0), min, BridgeVersion(1, 6, 0), false)
                .unwrap()
                .len(),
            1
        );
        let unknown = version_verdict(v, None, BridgeVersion(1, 5, 0), false).unwrap_err();
        assert!(unknown.contains("does not know"), "{unknown}");
        assert_eq!(
            version_verdict(v, None, BridgeVersion(1, 5, 0), true)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn recipient_rules() {
        let t = AnTarget {
            dapp_id: [1; 32],
            account_id: ACC,
        };
        let other = AccountInfo {
            status: AccStatus::Active,
            dapp_id: Some([2; 32]),
            ecc3: 5,
        };
        assert!(recipient_verdict(&t, Some(&other))
            .unwrap_err()
            .contains("lives in dapp"));
        let same = AccountInfo {
            dapp_id: Some([1; 32]),
            ..other.clone()
        };
        assert!(matches!(
            recipient_verdict(&t, Some(&same)).unwrap(),
            (RecipientState::Active { .. }, None)
        ));
        let uninit = AccountInfo {
            status: AccStatus::Uninit,
            dapp_id: None,
            ecc3: 5,
        };
        assert!(matches!(
            recipient_verdict(&t, Some(&uninit)).unwrap(),
            (RecipientState::NotDeployed { .. }, Some(_))
        ));
        assert!(matches!(
            recipient_verdict(&t, None).unwrap(),
            (RecipientState::Missing, Some(_))
        ));
    }

    #[tokio::test]
    async fn a_healthy_bridge_passes() {
        let p = go(&healthy(), [0; 32]).await.unwrap();
        assert_eq!(p.light_client, Some([0x20; 32]));
        assert!(p.owner_anchors_enabled);
    }

    #[tokio::test]
    async fn each_bridge_refusal_is_exit_2() {
        let paused = healthy();
        paused.getter(BRIDGE, "isPaused", vec![json!({ "value0": true })]);
        let untrusted = healthy();
        untrusted.getter(BRIDGE, "isTrustedL1Bridge", vec![
            json!({ "value0": false }),
        ]);
        let other_code = healthy();
        other_code.getter(BRIDGE, "getDepositVoucherCodeHash", vec![
            json!({ "value0": format!("0x{}", "11".repeat(32)) }),
        ]);
        for (an, needle) in [
            (paused, "paused"),
            (untrusted, "does not trust"),
            (other_code, "voucher"),
        ] {
            let e = go(&an, [0; 32]).await.unwrap_err();
            assert_eq!(e.exit_code(), crate::errors::ExitCode::PreflightRefused);
            assert!(e.to_string().contains(needle), "{needle}: {e}");
        }
    }
}
