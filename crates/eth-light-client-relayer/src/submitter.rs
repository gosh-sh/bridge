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
    Accepted { tx_hash: Option<[u8; 32]> },
    AlreadyOnHead,
    Rejected { reason: String },
    Pending { reason: String },
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
    /// `block_hash` is Ethereum byte order, like everything else in this
    /// crate; [`anchor_key_hex`] re-packs it at the ABI boundary.
    async fn re_push_anchor(&self, block_hash: [u8; 32]) -> Result<SubmitOutcome, RelayerError>;

    /// Owner one-way flip: `setLightClient` + `disableOwnerAnchors`
    /// (USDCBridge) and `disableOwnerRotation` (EthBeaconLightClient).
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
/// `rePushAnchor` (like every `_provenEthSlot` lookup) speaks that word.
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
    light_client_set: bool,
    owner_anchors_enabled: bool,
    owner_rotation_enabled: bool,
    re_push_count: u32,
    ancestry_count: u32,
}

impl MockAnSubmitter {
    pub fn accepting() -> Self {
        Self {
            inner: Mutex::new(MockInner {
                head_slot: None,
                proven: HashSet::new(),
                period: None,
                reject: false,
                light_client_set: false,
                owner_anchors_enabled: true,
                owner_rotation_enabled: true,
                re_push_count: 0,
                ancestry_count: 0,
            }),
        }
    }

    pub fn rejecting() -> Self {
        let s = Self::accepting();
        s.inner.lock().unwrap().reject = true;
        s
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

    pub fn light_client_set(&self) -> bool {
        self.inner.lock().unwrap().light_client_set
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
        let checkpoint = crate::header_rlp::link_headers(header_rlps)?;
        let mut inner = self.inner.lock().expect("poisoned");
        if inner.reject {
            return Ok(SubmitOutcome::Rejected {
                reason: "submitAncestry rejected".into(),
            });
        }
        if !inner.proven.contains(&checkpoint) {
            return Ok(SubmitOutcome::Rejected {
                reason: "ERR_UNKNOWN_CHECKPOINT".into(),
            });
        }
        for rlp in &header_rlps[1..] {
            inner.proven.insert(crate::header_rlp::keccak256(rlp));
        }
        inner.ancestry_count += 1;
        Ok(SubmitOutcome::Accepted {
            tx_hash: None,
        })
    }

    async fn re_push_anchor(&self, block_hash: [u8; 32]) -> Result<SubmitOutcome, RelayerError> {
        let mut inner = self.inner.lock().expect("poisoned");
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
        if inner.reject {
            return Ok(SubmitOutcome::Rejected {
                reason: "flip-owner rejected".into(),
            });
        }
        inner.light_client_set = true;
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
    use crate::header_rlp::{keccak256, link_headers};

    fn two_headers() -> (Vec<u8>, Vec<u8>) {
        let mut p = vec![0xf8, 33, 0xa0];
        p.extend(std::iter::repeat_n(0x01, 32));
        let p_hash = keccak256(&p);
        let mut c = vec![0xf8, 33, 0xa0];
        c.extend_from_slice(&p_hash);
        (c, p)
    }

    #[tokio::test]
    async fn mock_submit_ancestry_admits_parents() {
        let (c, p) = two_headers();
        let ckpt = keccak256(&c);
        let parent = keccak256(&p);
        let mock = MockAnSubmitter::accepting();
        mock.mark_proven(ckpt);
        match mock.submit_ancestry(&[c.clone(), p.clone()]).await.unwrap() {
            SubmitOutcome::Accepted {
                ..
            } => {},
            other => panic!("{other:?}"),
        }
        assert!(mock.is_proven(&parent));
        assert_eq!(link_headers(&[c, p]).unwrap(), ckpt);
    }

    #[tokio::test]
    async fn mock_submit_ancestry_unknown_checkpoint() {
        let (c, p) = two_headers();
        let mock = MockAnSubmitter::accepting();
        let out = mock.submit_ancestry(&[c, p]).await.unwrap();
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
        assert!(mock.light_client_set());
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
        if let Some((usdc_addr, usdc_client)) = &self.usdc {
            let lc = ExtendedAddress::parse(&self.config.light_client)
                .map_err(RelayerError::from)?
                .workchain_address();
            match self
                .call_on(
                    usdc_client.as_ref(),
                    usdc_addr,
                    "setLightClient",
                    json!({ "lightClient": lc }),
                )
                .await?
            {
                SubmitOutcome::Accepted {
                    ..
                } => {},
                other => return Ok(other),
            }
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
                // Second call after a wiped `state.json`: already one-way.
                SubmitOutcome::Rejected {
                    reason,
                } if reason.contains("228") || reason.contains("OWNER_ANCHORS_DISABLED") => {},
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

fn classify_call_error(function: &str, msg: &str) -> SubmitOutcome {
    if msg.contains("244") || msg.contains("STALE_UPDATE") {
        return SubmitOutcome::AlreadyOnHead;
    }
    SubmitOutcome::Rejected {
        reason: format!("{function} failed: {msg}"),
    }
}
