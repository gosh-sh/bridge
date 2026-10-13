//! AN `EthBeaconLightClient` submit (`submitUpdate` / `submitRotate` /
//! `submitAncestry`).

use std::{collections::HashSet, sync::Mutex};

use acki_nacki_interface::{ContractCallRequest, ExtendedAddress, IAckiNacki, TransactionStatus};
use async_trait::async_trait;
use serde_json::json;

use crate::{
    error::RelayerError,
    types::{RotateProofBundle, StepProofBundle},
};

#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    Accepted {
        tx_hash: Option<[u8; 32]>,
    },
    AlreadyOnHead,
    Rejected {
        reason: String,
    },
    Pending {
        reason: String,
    },
    /// The call never reached the chain — the endpoint was unreachable, the
    /// request timed out. Nothing was sent and nothing was paid for, so a
    /// caller that budgets paid attempts must not count this.
    NotSent {
        reason: String,
    },
}

#[async_trait]
pub trait AnSubmitter: Send + Sync {
    async fn submit_update(&self, bundle: &StepProofBundle) -> Result<SubmitOutcome, RelayerError>;

    async fn submit_rotate(
        &self,
        bundle: &RotateProofBundle,
    ) -> Result<SubmitOutcome, RelayerError>;

    async fn submit_ancestry(&self, header_rlps: &[Vec<u8>])
        -> Result<SubmitOutcome, RelayerError>;

    /// Re-send an already-proven hash to `USDCBridge` (`rePushAnchor`).
    /// `block_hash` is Ethereum byte order, as the relayer carries hashes;
    /// [`anchor_key_hex`] re-packs it into the stored key at the ABI boundary.
    async fn re_push_anchor(&self, block_hash: [u8; 32]) -> Result<SubmitOutcome, RelayerError>;

    /// Owner flip: check `getAnchorConfig().lightClient` against
    /// `AN_LIGHT_CLIENT`, then `disableOwnerAnchors` (USDCBridge) unless
    /// `ownerAnchorsEnabled` is already false, then `disableOwnerRotation`
    /// (EthBeaconLightClient). The bridge derives the light-client address
    /// from `setLightClientCode`. Rejected without a USDCBridge client.
    /// Idempotent.
    async fn flip_owner(&self) -> Result<SubmitOutcome, RelayerError>;

    /// Owner `setCommitteeCommitment(commitment, period)`: the
    /// weak-subjectivity bootstrap of a fresh contract and the manual
    /// committee hop while `submitRotate` is off (`--no-rotate`). Only works
    /// before `disableOwnerRotation`. `commitment` is the 32-byte LE field
    /// element exactly as it appears in the step public inputs (word 5).
    async fn set_committee_commitment(
        &self,
        commitment: [u8; 32],
        period: u64,
    ) -> Result<SubmitOutcome, RelayerError>;
}

/// Step PI word (32-byte little-endian BN254 scalar) → `uint256` argument
/// (`0x`-prefixed big-endian hex), the encoding tvm ABI accepts.
pub fn le_word_to_uint256_hex(word: &[u8; 32]) -> String {
    let mut be = *word;
    be.reverse();
    format!("0x{}", hex::encode(be))
}

/// Ethereum-order block hash → the anchor key `EthBeaconLightClient` stores.
///
/// The step circuit splits a 32-byte hash with `node_hi_lo`, which reads each
/// 16-byte half little-endian, and the contract keeps `(hi << 128) | lo`. So a
/// hash an explorer prints as `0xaf0919eb…` is keyed as `0xa3e073c2…`, and
/// `rePushAnchor` (like every `_provenEthSlot` lookup) speaks that word. This
/// is `EthBeaconLightClient._piForm` on the contract side.
pub fn anchor_key_hex(block_hash: &[u8; 32]) -> String {
    let mut key = *block_hash;
    key[..16].reverse();
    key[16..].reverse();
    format!("0x{}", hex::encode(key))
}

pub struct MockAnSubmitter {
    inner: Mutex<MockInner>,
}

struct MockInner {
    head_slot: Option<u64>,
    proven: HashSet<[u8; 32]>,
    period: Option<u64>,
    reject: bool,
    reject_ancestry: bool,
    not_send_ancestry: bool,
    error_ancestry: bool,
    ancestry_attempts: u32,
    reject_flip: bool,
    owner_anchors_enabled: bool,
    owner_rotation_enabled: bool,
    re_push_count: u32,
    ancestry_count: u32,
    /// A state file to read when `re_push_anchor` is called, and what it held.
    /// The one observable that tells apart "the span was persisted with the
    /// head" from "the span was persisted after this call": a tick dropped at
    /// this await must already have the span on disk.
    watch_state: Option<std::path::PathBuf>,
    state_at_re_push: Option<String>,
}

impl MockAnSubmitter {
    pub fn accepting() -> Self {
        Self {
            inner: Mutex::new(MockInner {
                head_slot: None,
                proven: HashSet::new(),
                period: None,
                reject: false,
                reject_ancestry: false,
                not_send_ancestry: false,
                error_ancestry: false,
                ancestry_attempts: 0,
                reject_flip: false,
                owner_anchors_enabled: true,
                owner_rotation_enabled: true,
                re_push_count: 0,
                ancestry_count: 0,
                watch_state: None,
                state_at_re_push: None,
            }),
        }
    }

    /// Read this state file when `re_push_anchor` is called and keep what it
    /// said, so a test can see what was already durable at that moment.
    pub fn watch_state_file(&self, path: std::path::PathBuf) {
        self.inner.lock().expect("poisoned").watch_state = Some(path);
    }

    pub fn state_at_re_push(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("poisoned")
            .state_at_re_push
            .clone()
    }

    /// Refuse only `submitAncestry`, so an update still lands.
    /// Fail the `submitAncestry` call itself, the way a transport error does
    /// after the message has already gone out.
    pub fn error_ancestry(&self) {
        self.inner.lock().expect("poisoned").error_ancestry = true;
    }

    /// Answer `submitAncestry` the way a wait timeout does: the chain may or
    /// may not have run it.
    pub fn not_send_ancestry(&self) {
        self.inner.lock().expect("poisoned").not_send_ancestry = true;
    }

    pub fn reject_ancestry(&self) {
        self.inner.lock().expect("poisoned").reject_ancestry = true;
    }

    /// Every `submitAncestry` reaching the mock, refused ones included.
    pub fn ancestry_attempts(&self) -> u32 {
        self.inner.lock().expect("poisoned").ancestry_attempts
    }

    /// Pretend an earlier submission already moved the head to `slot`, the
    /// way a landed message whose confirmation was lost looks to the daemon.
    pub fn set_head_slot(&self, slot: u64) {
        self.inner.lock().expect("poisoned").head_slot = Some(slot);
    }

    /// Stop refusing `submitAncestry`.
    pub fn accept_ancestry(&self) {
        self.inner.lock().expect("poisoned").reject_ancestry = false;
    }

    pub fn rejecting() -> Self {
        let s = Self::accepting();
        s.inner.lock().unwrap().reject = true;
        s
    }

    /// Accepts everything except `flip_owner`, which it rejects until
    /// [`Self::accept_flip`].
    pub fn rejecting_flip() -> Self {
        let s = Self::accepting();
        s.inner.lock().unwrap().reject_flip = true;
        s
    }

    pub fn accept_flip(&self) {
        self.inner.lock().unwrap().reject_flip = false;
    }

    pub fn head_slot(&self) -> Option<u64> {
        self.inner.lock().unwrap().head_slot
    }

    pub fn proven_count(&self) -> usize {
        self.inner.lock().unwrap().proven.len()
    }

    pub fn mark_proven(&self, h: [u8; 32]) {
        self.inner.lock().unwrap().proven.insert(h);
    }

    pub fn is_proven(&self, h: &[u8; 32]) -> bool {
        self.inner.lock().unwrap().proven.contains(h)
    }

    pub fn owner_rotation_enabled(&self) -> bool {
        self.inner.lock().unwrap().owner_rotation_enabled
    }

    pub fn owner_anchors_enabled(&self) -> bool {
        self.inner.lock().unwrap().owner_anchors_enabled
    }

    pub fn re_push_count(&self) -> u32 {
        self.inner.lock().unwrap().re_push_count
    }

    pub fn ancestry_count(&self) -> u32 {
        self.inner.lock().unwrap().ancestry_count
    }
}

#[async_trait]
impl AnSubmitter for MockAnSubmitter {
    async fn submit_update(&self, bundle: &StepProofBundle) -> Result<SubmitOutcome, RelayerError> {
        let mut inner = self.inner.lock().expect("poisoned");
        if inner.reject {
            return Ok(SubmitOutcome::Rejected {
                reason: "ZKHALO2VERIFYWITHVK rejected the step proof".into(),
            });
        }
        let slot = bundle.parsed.finalized_slot;
        let hash = bundle.parsed.execution_block_hash;
        let advance = inner.head_slot.map(|h| slot > h).unwrap_or(true);
        let unseen = !inner.proven.contains(&hash);
        let late = inner.head_slot.map(|h| slot < h).unwrap_or(false) && unseen;
        if inner.head_slot == Some(slot) {
            return Ok(SubmitOutcome::AlreadyOnHead);
        }
        if !advance && !late {
            return Ok(SubmitOutcome::Rejected {
                reason: format!("ERR_STALE_UPDATE slot={slot}"),
            });
        }
        inner.proven.insert(hash);
        if advance {
            inner.head_slot = Some(slot);
        }
        Ok(SubmitOutcome::Accepted {
            tx_hash: None,
        })
    }

    async fn submit_rotate(
        &self,
        bundle: &RotateProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        let mut inner = self.inner.lock().expect("poisoned");
        if inner.reject {
            return Ok(SubmitOutcome::Rejected {
                reason: "ZKHALO2VERIFYWITHVK rejected the rotate proof".into(),
            });
        }
        if inner.period.map(|p| bundle.period <= p).unwrap_or(false) {
            return Ok(SubmitOutcome::Rejected {
                reason: format!("ERR_STALE_PERIOD period={}", bundle.period),
            });
        }
        inner.period = Some(bundle.period);
        Ok(SubmitOutcome::Accepted {
            tx_hash: None,
        })
    }

    async fn submit_ancestry(
        &self,
        header_rlps: &[Vec<u8>],
    ) -> Result<SubmitOutcome, RelayerError> {
        let (older, newer) = crate::header_rlp::link_headers(header_rlps)?;
        let mut inner = self.inner.lock().expect("poisoned");
        inner.ancestry_attempts += 1;
        if inner.error_ancestry {
            return Err(RelayerError::other(
                "submitAncestry: connection reset by peer",
            ));
        }
        if inner.not_send_ancestry {
            return Ok(SubmitOutcome::NotSent {
                reason: "wait timed out".into(),
            });
        }
        if inner.reject || inner.reject_ancestry {
            return Ok(SubmitOutcome::Rejected {
                reason: "submitAncestry rejected".into(),
            });
        }
        if !inner.proven.contains(&older) || !inner.proven.contains(&newer) {
            return Ok(SubmitOutcome::Rejected {
                reason: "ERR_UNKNOWN_CHECKPOINT".into(),
            });
        }
        let last = header_rlps.len() - 1;
        for rlp in &header_rlps[1..last] {
            inner.proven.insert(crate::header_rlp::keccak256(rlp));
        }
        inner.ancestry_count += 1;
        Ok(SubmitOutcome::Accepted {
            tx_hash: None,
        })
    }

    async fn re_push_anchor(&self, block_hash: [u8; 32]) -> Result<SubmitOutcome, RelayerError> {
        let mut inner = self.inner.lock().expect("poisoned");
        if let Some(path) = inner.watch_state.clone() {
            inner.state_at_re_push = std::fs::read_to_string(&path).ok();
        }
        if inner.reject {
            return Ok(SubmitOutcome::Rejected {
                reason: "rePushAnchor rejected".into(),
            });
        }
        if !inner.proven.contains(&block_hash) {
            return Ok(SubmitOutcome::Rejected {
                reason: "ERR_NOT_PROVEN".into(),
            });
        }
        inner.re_push_count += 1;
        Ok(SubmitOutcome::Accepted {
            tx_hash: None,
        })
    }

    async fn flip_owner(&self) -> Result<SubmitOutcome, RelayerError> {
        let mut inner = self.inner.lock().expect("poisoned");
        if inner.reject || inner.reject_flip {
            return Ok(SubmitOutcome::Rejected {
                reason: "flip-owner rejected".into(),
            });
        }
        inner.owner_anchors_enabled = false;
        inner.owner_rotation_enabled = false;
        Ok(SubmitOutcome::Accepted {
            tx_hash: None,
        })
    }

    async fn set_committee_commitment(
        &self,
        _commitment: [u8; 32],
        period: u64,
    ) -> Result<SubmitOutcome, RelayerError> {
        let mut inner = self.inner.lock().expect("poisoned");
        if inner.reject {
            return Ok(SubmitOutcome::Rejected {
                reason: "setCommitteeCommitment rejected".into(),
            });
        }
        if !inner.owner_rotation_enabled {
            return Ok(SubmitOutcome::Rejected {
                reason: "ERR_OWNER_ROTATION_DISABLED".into(),
            });
        }
        inner.period = Some(period);
        Ok(SubmitOutcome::Accepted {
            tx_hash: None,
        })
    }
}

#[cfg(test)]
mod set_committee_tests {
    use super::*;

    #[test]
    fn same_tvm_account_accepts_workchain_and_extended_forms() {
        let acc = "aa".repeat(32);
        assert!(same_tvm_account(&format!("0:{acc}"), &acc));
        assert!(same_tvm_account(&format!("0x{acc}"), &acc));
        assert!(same_tvm_account(&format!("{acc}::{acc}"), &acc));
        assert!(!same_tvm_account(&format!("0:{}", "bb".repeat(32)), &acc));
        // The account half of `dapp::account` is compared, not the dapp.
        let dapp = "cc".repeat(32);
        assert!(same_tvm_account(&format!("{dapp}::{acc}"), &acc));
        assert!(!same_tvm_account(&format!("{acc}::{dapp}"), &acc));
        assert!(!same_tvm_account(&format!("-1:{acc}"), &acc));
        assert!(!same_tvm_account("", &acc));
    }

    #[test]
    fn le_word_becomes_big_endian_uint256() {
        let mut w = [0u8; 32];
        w[0] = 0x2a; // 42 as LE field element
        assert_eq!(
            le_word_to_uint256_hex(&w),
            format!("0x{}2a", "00".repeat(31))
        );
    }

    /// `rePushAnchor` must speak the same word `submitUpdate` stored, so the
    /// key is rebuilt here from the step public inputs the contract decodes:
    /// words 6 and 7 are the hi/lo halves, recombined as `(hi << 128) | lo`.
    #[test]
    fn anchor_key_matches_step_public_inputs() {
        let mut hash = [0u8; 32];
        for (i, b) in hash.iter_mut().enumerate() {
            *b = (i as u8) + 1;
        }
        let root = format!("0x{}", "11".repeat(32));
        let json = format!(
            r#"{{"data":{{
              "attested_header":{{"beacon":{{"slot":"9","state_root":"{root}"}}}},
              "finalized_header":{{
                "beacon":{{"slot":"8","state_root":"{root}"}},
                "execution":{{"block_hash":"0x{}"}}
              }},
              "sync_aggregate":{{"sync_committee_bits":"0x{}01"}}
            }}}}"#,
            hex::encode(hash),
            "00".repeat(63)
        );
        let update = crate::types::parse_finality_update(&json).unwrap();
        let (blob, _) = crate::types::pack_step_public_inputs(&update, [0u8; 32]);
        let word = |i: usize| -> [u8; 32] { blob[i * 32..(i + 1) * 32].try_into().unwrap() };
        // Each half occupies the low 16 bytes of its uint256 — the last 32 hex
        // characters — and hi sits above lo in the key.
        let hi = le_word_to_uint256_hex(&word(6));
        let lo = le_word_to_uint256_hex(&word(7));
        assert_eq!(
            anchor_key_hex(&hash),
            format!("0x{}{}", &hi[34..], &lo[34..])
        );
        assert_ne!(anchor_key_hex(&hash), format!("0x{}", hex::encode(hash)));
    }

    #[tokio::test]
    async fn mock_set_committee_refused_after_flip() {
        let mock = MockAnSubmitter::accepting();
        assert!(matches!(
            mock.set_committee_commitment([1; 32], 7).await.unwrap(),
            SubmitOutcome::Accepted { .. }
        ));
        mock.flip_owner().await.unwrap();
        assert!(matches!(
            mock.set_committee_commitment([1; 32], 8).await.unwrap(),
            SubmitOutcome::Rejected { .. }
        ));
    }
}

#[cfg(test)]
mod ancestry_submit_tests {
    use super::*;
    use crate::header_rlp::keccak256;

    #[tokio::test]
    async fn mock_submit_ancestry_admits_the_blocks_between_the_anchors() {
        let chain = crate::header_rlp::dummy_linked_chain(4);
        let mock = MockAnSubmitter::accepting();
        mock.mark_proven(keccak256(&chain[0]));
        mock.mark_proven(keccak256(&chain[3]));
        match mock.submit_ancestry(&chain).await.unwrap() {
            SubmitOutcome::Accepted {
                ..
            } => {},
            other => panic!("{other:?}"),
        }
        assert!(mock.is_proven(&keccak256(&chain[1])));
        assert!(mock.is_proven(&keccak256(&chain[2])));
    }

    #[tokio::test]
    async fn mock_submit_ancestry_unknown_checkpoint() {
        let chain = crate::header_rlp::dummy_linked_chain(4);
        let mock = MockAnSubmitter::accepting();
        mock.mark_proven(keccak256(&chain[0]));
        let out = mock.submit_ancestry(&chain).await.unwrap();
        match out {
            SubmitOutcome::Rejected {
                reason,
            } => {
                assert!(reason.contains("UNKNOWN_CHECKPOINT"), "{reason}");
            },
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_flip_owner_drops_owner_writers() {
        let mock = MockAnSubmitter::accepting();
        assert!(mock.owner_rotation_enabled());
        match mock.flip_owner().await.unwrap() {
            SubmitOutcome::Accepted {
                ..
            } => {},
            other => panic!("{other:?}"),
        }
        assert!(!mock.owner_anchors_enabled());
        assert!(!mock.owner_rotation_enabled());
        mock.flip_owner().await.unwrap();
        assert!(!mock.owner_rotation_enabled());
    }

    #[tokio::test]
    async fn mock_re_push_requires_proven_hash() {
        let mock = MockAnSubmitter::accepting();
        let h = [0x11; 32];
        match mock.re_push_anchor(h).await.unwrap() {
            SubmitOutcome::Rejected {
                reason,
            } => {
                assert!(reason.contains("NOT_PROVEN"), "{reason}");
            },
            other => panic!("{other:?}"),
        }
        mock.mark_proven(h);
        match mock.re_push_anchor(h).await.unwrap() {
            SubmitOutcome::Accepted {
                ..
            } => {},
            other => panic!("{other:?}"),
        }
        assert_eq!(mock.re_push_count(), 1);
    }

    #[tokio::test]
    async fn mock_late_register_keeps_head() {
        let mock = MockAnSubmitter::accepting();
        let first = crate::types::StepProofBundle {
            parsed: crate::types::StepPublicInputs {
                attested_slot: 200,
                finalized_slot: 192,
                finalized_beacon_root: [0; 32],
                participation: 400,
                committee_commitment: [0xC0; 32],
                execution_block_hash: [0xAA; 32],
                attested_state_root: [0; 32],
            },
            public_inputs: vec![0u8; 320],
            proof: vec![0xAB; 8],
        };
        mock.submit_update(&first).await.unwrap();
        let late = crate::types::StepProofBundle {
            parsed: crate::types::StepPublicInputs {
                attested_slot: 100,
                finalized_slot: 96,
                finalized_beacon_root: [0; 32],
                participation: 400,
                committee_commitment: [0xC0; 32],
                execution_block_hash: [0xBB; 32],
                attested_state_root: [0; 32],
            },
            public_inputs: vec![0u8; 320],
            proof: vec![0xAB; 8],
        };
        match mock.submit_update(&late).await.unwrap() {
            SubmitOutcome::Accepted {
                ..
            } => {},
            other => panic!("{other:?}"),
        }
        assert_eq!(mock.head_slot(), Some(192));
        assert!(mock.is_proven(&[0xAA; 32]));
        assert!(mock.is_proven(&[0xBB; 32]));
        match mock.submit_update(&late).await.unwrap() {
            SubmitOutcome::Rejected {
                reason,
            } => {
                assert!(reason.contains("STALE_UPDATE"), "{reason}");
            },
            other => panic!("{other:?}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnSubmitConfig {
    pub from: String,
    pub light_client: String,
    pub confirm_timeout_secs: u64,
    pub usdc_bridge: Option<String>,
}

pub fn build_submit_params(proof: &[u8], public_inputs: &[u8]) -> serde_json::Value {
    json!({
        "proof": hex::encode(proof),
        "publicInputs": hex::encode(public_inputs),
    })
}

pub struct AnInterfaceSubmitter<C: IAckiNacki> {
    client: std::sync::Arc<C>,
    usdc: Option<(String, std::sync::Arc<C>)>,
    config: AnSubmitConfig,
}

impl<C: IAckiNacki> AnInterfaceSubmitter<C> {
    pub fn new(client: std::sync::Arc<C>, config: AnSubmitConfig) -> Self {
        Self {
            client,
            usdc: None,
            config,
        }
    }

    pub fn with_usdc(mut self, address: String, client: std::sync::Arc<C>) -> Self {
        self.usdc = Some((address, client));
        self
    }

    async fn call(
        &self,
        function: &str,
        params: serde_json::Value,
    ) -> Result<SubmitOutcome, RelayerError> {
        self.call_on(&self.client, &self.config.light_client, function, params)
            .await
    }

    async fn call_on(
        &self,
        client: &C,
        to: &str,
        function: &str,
        params: serde_json::Value,
    ) -> Result<SubmitOutcome, RelayerError> {
        let from = ExtendedAddress::parse(&self.config.from).map_err(RelayerError::from)?;
        let to = ExtendedAddress::parse(to).map_err(RelayerError::from)?;
        let call = ContractCallRequest {
            from,
            to,
            function: function.to_string(),
            params,
        };
        let sent = match client.call_contract(call).await {
            Ok(hash) => hash,
            Err(e) => return Ok(classify_call_error(function, &e.to_string())),
        };
        let receipt = client
            .wait_for_confirmation(&sent, self.config.confirm_timeout_secs)
            .await?;
        match receipt.status {
            TransactionStatus::Confirmed | TransactionStatus::Included => {
                Ok(SubmitOutcome::Accepted {
                    tx_hash: Some(sent),
                })
            },
            TransactionStatus::Reverted | TransactionStatus::Failed => {
                Ok(SubmitOutcome::Rejected {
                    reason: format!(
                        "{function} status={:?} exit_code={:?}",
                        receipt.status, receipt.exit_code
                    ),
                })
            },
            TransactionStatus::Pending => Ok(SubmitOutcome::Pending {
                reason: format!("{function} still pending after timeout"),
            }),
        }
    }
}

#[async_trait]
impl<C: IAckiNacki> AnSubmitter for AnInterfaceSubmitter<C> {
    async fn submit_update(&self, bundle: &StepProofBundle) -> Result<SubmitOutcome, RelayerError> {
        self.call(
            "submitUpdate",
            build_submit_params(&bundle.proof, &bundle.public_inputs),
        )
        .await
    }

    async fn submit_rotate(
        &self,
        bundle: &RotateProofBundle,
    ) -> Result<SubmitOutcome, RelayerError> {
        self.call(
            "submitRotate",
            build_submit_params(&bundle.proof, &bundle.public_inputs),
        )
        .await
    }

    async fn submit_ancestry(
        &self,
        header_rlps: &[Vec<u8>],
    ) -> Result<SubmitOutcome, RelayerError> {
        crate::header_rlp::link_headers(header_rlps)?;
        let header_rlps: Vec<String> = header_rlps.iter().map(hex::encode).collect();
        self.call("submitAncestry", json!({ "headerRlps": header_rlps }))
            .await
    }

    async fn re_push_anchor(&self, block_hash: [u8; 32]) -> Result<SubmitOutcome, RelayerError> {
        self.call(
            "rePushAnchor",
            json!({ "blockHash": anchor_key_hex(&block_hash) }),
        )
        .await
    }

    async fn flip_owner(&self) -> Result<SubmitOutcome, RelayerError> {
        // `disableOwnerRotation` alone would report a flip while the owner
        // still admits anchors, so both contracts are required.
        let Some((usdc_addr, usdc_client)) = &self.usdc else {
            return Ok(SubmitOutcome::Rejected {
                reason: "flip-owner needs USDCBridge (AN_USDC_BRIDGE + AN_USDC_ABI_PATH)".into(),
            });
        };
        let expected =
            ExtendedAddress::parse(&self.config.light_client).map_err(RelayerError::from)?;
        // A failed read is retried on the next accepted update like any other
        // refusal; it must not abort the tick before `rePushAnchor`.
        let cfg = match usdc_client
            .run_getter(usdc_addr, "getAnchorConfig", json!({}))
            .await
        {
            Ok(cfg) => cfg,
            Err(e) => {
                return Ok(SubmitOutcome::Rejected {
                    reason: format!("getAnchorConfig failed: {e}"),
                })
            },
        };
        let on_chain = cfg
            .get("lightClient")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if is_zero_tvm_account(on_chain) || !same_tvm_account(on_chain, expected.account_id()) {
            return Ok(SubmitOutcome::Rejected {
                reason: format!(
                    "getAnchorConfig.lightClient={on_chain} != AN_LIGHT_CLIENT {}",
                    expected.workchain_address()
                ),
            });
        }
        // `disableOwnerAnchors` does not revert a second call, so the getter is
        // the only "already flipped" signal; anything but an explicit `false`
        // sends the call.
        let already_off = cfg.get("ownerAnchorsEnabled").and_then(|v| v.as_bool()) == Some(false);
        if !already_off {
            match self
                .call_on(
                    usdc_client.as_ref(),
                    usdc_addr,
                    "disableOwnerAnchors",
                    json!({}),
                )
                .await?
            {
                SubmitOutcome::Accepted {
                    ..
                } => {},
                other => return Ok(other),
            }
        }
        self.call("disableOwnerRotation", json!({})).await
    }

    async fn set_committee_commitment(
        &self,
        commitment: [u8; 32],
        period: u64,
    ) -> Result<SubmitOutcome, RelayerError> {
        self.call(
            "setCommitteeCommitment",
            json!({
                "committeeCommitment": le_word_to_uint256_hex(&commitment),
                "period": period,
            }),
        )
        .await
    }
}

/// Compare a TVM `address` getter (`0:<hex>` / `0x<hex>` / `dapp::account`)
/// with the 64-hex account id from `AN_LIGHT_CLIENT`.
fn same_tvm_account(on_chain: &str, expected_account_id: &str) -> bool {
    let got = tvm_account_hex(on_chain);
    let exp = tvm_account_hex(expected_account_id);
    match (got, exp) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
        _ => false,
    }
}

/// `getAnchorConfig` returns the zero address until `setLightClientCode` runs.
fn is_zero_tvm_account(on_chain: &str) -> bool {
    tvm_account_hex(on_chain).is_some_and(|h| h.bytes().all(|b| b == b'0'))
}

fn tvm_account_hex(s: &str) -> Option<String> {
    if let Ok(ext) = ExtendedAddress::parse(s) {
        return Some(ext.account_id().to_ascii_lowercase());
    }
    let bare = s
        .trim()
        .strip_prefix("0:")
        .or_else(|| s.trim().strip_prefix("0x"))
        .unwrap_or(s.trim());
    if bare.len() == 64 && bare.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(bare.to_ascii_lowercase())
    } else {
        None
    }
}

/// The exit code out of an error string, or `None` when the message carries
/// none. The same shapes `acki-nacki-interface` emits; kept local because that
/// crate's parser lives behind the `tvm-sdk` feature, which the default build
/// and the tests do not enable.
fn parsed_exit_code(msg: &str) -> Option<i32> {
    for marker in [
        "exit_code=Some(",
        "exit_code=",
        "local_exit_code=",
        "\"exit_code\":",
    ] {
        if let Some(rest) = msg.split(marker).nth(1) {
            let digits: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(code) = digits.parse::<i32>() {
                return Some(code);
            }
        }
    }
    None
}

fn classify_call_error(function: &str, msg: &str) -> SubmitOutcome {
    // An exit code means the chain ran the call, so the outcome is a refusal
    // and never a transport failure. It is parsed out of `exit_code=N`, not
    // matched as a substring: the message carries the account address and
    // timestamps, and a `244` anywhere in those would have turned any refusal
    // into "already on head" — recording a head the chain never took, which
    // then costs the span behind it its anchors as well.
    if let Some(code) = parsed_exit_code(msg) {
        if code == 244 {
            return SubmitOutcome::AlreadyOnHead;
        }
        return SubmitOutcome::Rejected {
            reason: format!("{function} failed: {msg}"),
        };
    }
    if msg.contains("STALE_UPDATE") {
        return SubmitOutcome::AlreadyOnHead;
    }
    // No exit code: the chain did not execute the call. Transport failures and
    // an expired message are free retries — a caller budgeting paid attempts
    // must not spend one here. Matched on words rather than on bare status
    // numbers, for the same reason as above.
    let not_sent = [
        "Connection",
        "connect",
        "timed out",
        "timeout",
        "dns",
        "DNS",
        "unreachable",
        "reset by peer",
        "sending request",
        "Bad Gateway",
        "Service Unavailable",
        "Gateway Timeout",
        "was not executed",
        "Message expired",
    ];
    if not_sent.iter().any(|m| msg.contains(m)) {
        return SubmitOutcome::NotSent {
            reason: format!("{function}: {msg}"),
        };
    }
    SubmitOutcome::Rejected {
        reason: format!("{function} failed: {msg}"),
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    /// The error text carries the account address and timestamps. A 64-hex
    /// address holding `244` must not turn a refusal into "already on head":
    /// the daemon would record a head the chain refused, never resubmit that
    /// slot, and pay out the span behind it against an unproven anchor.
    #[test]
    fn digits_inside_the_address_are_not_read_as_a_refusal_code() {
        let msg = "Contract execution was terminated with error \
                   (account=244be1a9c3d05507f0e1b2449f3cc8a7d4e5f60718293a4b5c6d7e8f90a1b2c3) \
                   data={\"exit_code\":220}";
        let out = classify_call_error("submitUpdate", msg);
        assert!(
            matches!(out, SubmitOutcome::Rejected { .. }),
            "a 244 in the address must not be read as ERR_STALE_UPDATE: {out:?}"
        );
    }

    #[test]
    fn the_parsed_exit_code_244_is_already_on_head() {
        let msg = "tvm error (account=0abc) (exit_code=244)";
        assert!(matches!(
            classify_call_error("submitUpdate", msg),
            SubmitOutcome::AlreadyOnHead
        ));
    }

    #[test]
    fn another_parsed_exit_code_is_a_refusal_not_a_transport_failure() {
        // The address holds 503; without the exit code this used to be read as
        // an unreachable endpoint and retried for free, forever.
        let msg = "tvm error (account=503f1a2b) (exit_code=249)";
        assert!(matches!(
            classify_call_error("submitAncestry", msg),
            SubmitOutcome::Rejected { .. }
        ));
    }

    /// SDK 507. The chain never ran it, so it must not cost a paid attempt.
    #[test]
    fn an_expired_message_was_never_sent() {
        let msg = "Message expired. Contract was not executed on chain.";
        assert!(matches!(
            classify_call_error("submitAncestry", msg),
            SubmitOutcome::NotSent { .. }
        ));
    }

    #[test]
    fn a_refusal_with_no_exit_code_and_no_transport_word_is_a_refusal() {
        let msg = "unexpected failure (account=502aa1)";
        assert!(matches!(
            classify_call_error("submitAncestry", msg),
            SubmitOutcome::Rejected { .. }
        ));
    }
}

#[cfg(test)]
mod flip_owner_tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use acki_nacki_interface::{
        AckiNackiError, AckiNackiTransaction, TransactionReceipt, TransactionStatus,
    };
    use serde_json::{json, Value};

    use super::*;

    fn lc() -> String {
        "22".repeat(32)
    }

    fn bridge() -> String {
        "1a".repeat(32)
    }

    fn extended(account: &str) -> String {
        format!("{account}::{account}")
    }

    #[derive(Clone)]
    enum Reply {
        Err(String),
        Reverted(i32),
        Pending,
    }

    /// Records `(account_id, function)` for every getter and call, in order,
    /// in one log shared by the light-client and the bridge client. Each
    /// client stands for one contract's ABI; a call it is asked to send to
    /// the other contract is logged as `"<function> via <account> client"`.
    struct RecordingClient {
        serves: String,
        calls: Arc<Mutex<Vec<(String, String)>>>,
        getter: std::result::Result<Value, String>,
        replies: HashMap<&'static str, Reply>,
        sent: Mutex<HashMap<[u8; 32], String>>,
    }

    impl RecordingClient {
        fn new(getter: std::result::Result<Value, String>) -> Self {
            Self {
                serves: String::new(),
                calls: Arc::new(Mutex::new(Vec::new())),
                getter,
                replies: HashMap::new(),
                sent: Mutex::new(HashMap::new()),
            }
        }

        fn reply(mut self, function: &'static str, reply: Reply) -> Self {
            self.replies.insert(function, reply);
            self
        }

        /// A client for `account`'s contract sharing this one's log and script.
        fn serving(&self, account: String) -> Self {
            Self {
                serves: account,
                calls: self.calls.clone(),
                getter: self.getter.clone(),
                replies: self.replies.clone(),
                sent: Mutex::new(HashMap::new()),
            }
        }

        fn record(&self, account: &str, function: &str) -> usize {
            let entry = if account == self.serves {
                function.to_string()
            } else {
                format!("{function} via {} client", self.serves)
            };
            let mut calls = self.calls.lock().unwrap();
            calls.push((account.to_string(), entry));
            calls.len()
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl IAckiNacki for RecordingClient {
        async fn send_transaction(
            &self,
            _tx: AckiNackiTransaction,
        ) -> acki_nacki_interface::Result<[u8; 32]> {
            unimplemented!("flip_owner does not send raw transactions")
        }

        async fn call_contract(
            &self,
            call: ContractCallRequest,
        ) -> acki_nacki_interface::Result<[u8; 32]> {
            let function = call.function.clone();
            let n = self.record(call.to.account_id(), &function);
            if let Some(Reply::Err(msg)) = self.replies.get(function.as_str()) {
                return Err(AckiNackiError::ContractError(msg.clone()));
            }
            let hash = [n as u8; 32];
            self.sent.lock().unwrap().insert(hash, function);
            Ok(hash)
        }

        async fn get_transaction_status(
            &self,
            _tx_hash: &[u8; 32],
        ) -> acki_nacki_interface::Result<TransactionStatus> {
            unimplemented!()
        }

        async fn get_transaction_receipt(
            &self,
            _tx_hash: &[u8; 32],
        ) -> acki_nacki_interface::Result<TransactionReceipt> {
            unimplemented!()
        }

        async fn wait_for_confirmation(
            &self,
            tx_hash: &[u8; 32],
            _timeout_secs: u64,
        ) -> acki_nacki_interface::Result<TransactionReceipt> {
            let function = self.sent.lock().unwrap()[tx_hash].clone();
            let (status, exit_code) = match self.replies.get(function.as_str()) {
                Some(Reply::Reverted(code)) => (TransactionStatus::Reverted, Some(*code)),
                Some(Reply::Pending) => (TransactionStatus::Pending, None),
                _ => (TransactionStatus::Confirmed, Some(0)),
            };
            let mut receipt = TransactionReceipt::new(*tx_hash, status, None, 0, Vec::new());
            receipt.exit_code = exit_code;
            Ok(receipt)
        }

        async fn get_block_number(&self) -> acki_nacki_interface::Result<u64> {
            unimplemented!()
        }

        async fn get_balance(&self, _address: &str) -> acki_nacki_interface::Result<u64> {
            unimplemented!()
        }

        async fn run_getter(
            &self,
            to: &str,
            function: &str,
            _params: Value,
        ) -> acki_nacki_interface::Result<Value> {
            self.record(ExtendedAddress::parse(to).unwrap().account_id(), function);
            self.getter.clone().map_err(AckiNackiError::NetworkError)
        }
    }

    fn submitter(
        script: RecordingClient,
        with_usdc: bool,
    ) -> (AnInterfaceSubmitter<RecordingClient>, Arc<RecordingClient>) {
        submitter_for(script, with_usdc, lc())
    }

    /// `light_client` is `AN_LIGHT_CLIENT`'s account. The returned client is
    /// the light-client one; its log also holds the bridge client's calls.
    fn submitter_for(
        script: RecordingClient,
        with_usdc: bool,
        light_client: String,
    ) -> (AnInterfaceSubmitter<RecordingClient>, Arc<RecordingClient>) {
        let lc_client = Arc::new(script.serving(light_client.clone()));
        let s = AnInterfaceSubmitter::new(lc_client.clone(), AnSubmitConfig {
            from: extended(&"33".repeat(32)),
            light_client: extended(&light_client),
            confirm_timeout_secs: 1,
            usdc_bridge: with_usdc.then(|| extended(&bridge())),
        });
        let s = if with_usdc {
            s.with_usdc(extended(&bridge()), Arc::new(script.serving(bridge())))
        } else {
            s
        };
        (s, lc_client)
    }

    fn anchor_config(light_client: &str, owner_anchors_enabled: bool) -> Value {
        json!({
            "lightClient": format!("0:{light_client}"),
            "ownerAnchorsEnabled": owner_anchors_enabled,
        })
    }

    fn call(account: String, function: &str) -> (String, String) {
        (account, function.to_string())
    }

    #[tokio::test]
    async fn flips_bridge_anchors_then_light_client_rotation() {
        let (s, client) = submitter(RecordingClient::new(Ok(anchor_config(&lc(), true))), true);
        assert!(matches!(
            s.flip_owner().await.unwrap(),
            SubmitOutcome::Accepted { .. }
        ));
        assert_eq!(client.calls(), vec![
            call(bridge(), "getAnchorConfig"),
            call(bridge(), "disableOwnerAnchors"),
            call(lc(), "disableOwnerRotation"),
        ]);
    }

    #[tokio::test]
    async fn another_light_client_on_the_bridge_sends_nothing() {
        let other = "44".repeat(32);
        let (s, client) = submitter(RecordingClient::new(Ok(anchor_config(&other, true))), true);
        match s.flip_owner().await.unwrap() {
            SubmitOutcome::Rejected {
                reason,
            } => {
                assert!(reason.contains(&other), "{reason}");
                assert!(reason.contains(&lc()), "{reason}");
            },
            other => panic!("{other:?}"),
        }
        assert_eq!(client.calls(), vec![call(bridge(), "getAnchorConfig")]);
    }

    #[tokio::test]
    async fn light_client_code_not_installed_sends_nothing() {
        // `AN_LIGHT_CLIENT` is zero too, so only the zero check can refuse.
        let zero = "00".repeat(32);
        let (s, client) = submitter_for(
            RecordingClient::new(Ok(anchor_config(&zero, true))),
            true,
            zero.clone(),
        );
        assert!(matches!(
            s.flip_owner().await.unwrap(),
            SubmitOutcome::Rejected { .. }
        ));
        assert_eq!(client.calls(), vec![call(bridge(), "getAnchorConfig")]);
    }

    #[tokio::test]
    async fn malformed_getter_output_sends_nothing() {
        let (s, client) = submitter(
            RecordingClient::new(Ok(json!({ "value0": format!("0:{}", lc()) }))),
            true,
        );
        assert!(matches!(
            s.flip_owner().await.unwrap(),
            SubmitOutcome::Rejected { .. }
        ));
        assert_eq!(client.calls(), vec![call(bridge(), "getAnchorConfig")]);
    }

    #[tokio::test]
    async fn owner_anchors_already_off_goes_straight_to_rotation() {
        let (s, client) = submitter(RecordingClient::new(Ok(anchor_config(&lc(), false))), true);
        assert!(matches!(
            s.flip_owner().await.unwrap(),
            SubmitOutcome::Accepted { .. }
        ));
        assert_eq!(client.calls(), vec![
            call(bridge(), "getAnchorConfig"),
            call(lc(), "disableOwnerRotation"),
        ]);
    }

    #[tokio::test]
    async fn missing_owner_anchors_flag_still_disables_them() {
        let (s, client) = submitter(
            RecordingClient::new(Ok(json!({ "lightClient": format!("0:{}", lc()) }))),
            true,
        );
        assert!(matches!(
            s.flip_owner().await.unwrap(),
            SubmitOutcome::Accepted { .. }
        ));
        assert_eq!(client.calls()[1], call(bridge(), "disableOwnerAnchors"));
    }

    /// The error text carries addresses and message ids, so a code that
    /// appears in it by chance must not read as "already disabled".
    #[tokio::test]
    async fn failed_disable_stops_before_rotation_whatever_its_text() {
        let text = format!(
            "contract call aborted (exit_code=209) account=0:{}225 OWNER_ANCHORS_DISABLED",
            "ab".repeat(30)
        );
        let client = RecordingClient::new(Ok(anchor_config(&lc(), true)))
            .reply("disableOwnerAnchors", Reply::Err(text));
        let (s, client) = submitter(client, true);
        assert!(matches!(
            s.flip_owner().await.unwrap(),
            SubmitOutcome::Rejected { .. }
        ));
        assert_eq!(client.calls(), vec![
            call(bridge(), "getAnchorConfig"),
            call(bridge(), "disableOwnerAnchors"),
        ]);
    }

    #[tokio::test]
    async fn reverted_or_pending_disable_stops_before_rotation() {
        for reply in [Reply::Reverted(225), Reply::Pending] {
            let client = RecordingClient::new(Ok(anchor_config(&lc(), true)))
                .reply("disableOwnerAnchors", reply);
            let (s, client) = submitter(client, true);
            let out = s.flip_owner().await.unwrap();
            assert!(
                matches!(
                    out,
                    SubmitOutcome::Rejected { .. } | SubmitOutcome::Pending { .. }
                ),
                "{out:?}"
            );
            assert_eq!(client.calls().len(), 2, "{:?}", client.calls());
        }
    }

    #[tokio::test]
    async fn getter_failure_is_a_rejection_not_an_error() {
        let (s, client) = submitter(RecordingClient::new(Err("empty boc".into())), true);
        match s.flip_owner().await.unwrap() {
            SubmitOutcome::Rejected {
                reason,
            } => assert!(reason.contains("getAnchorConfig"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(client.calls(), vec![call(bridge(), "getAnchorConfig")]);
    }

    #[tokio::test]
    async fn without_the_bridge_client_nothing_is_sent() {
        let (s, client) = submitter(RecordingClient::new(Ok(anchor_config(&lc(), true))), false);
        assert!(matches!(
            s.flip_owner().await.unwrap(),
            SubmitOutcome::Rejected { .. }
        ));
        assert!(client.calls().is_empty());
    }
}
