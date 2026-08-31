//! AN `EthBeaconLightClient` submit (`submitUpdate` / `submitRotate`).

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
}

pub struct MockAnSubmitter {
    inner: Mutex<MockInner>,
}

struct MockInner {
    head_slot: Option<u64>,
    proven: HashSet<[u8; 32]>,
    period: Option<u64>,
    reject: bool,
}

impl MockAnSubmitter {
    pub fn accepting() -> Self {
        Self {
            inner: Mutex::new(MockInner {
                head_slot: None,
                proven: HashSet::new(),
                period: None,
                reject: false,
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
}

#[derive(Clone, Debug)]
pub struct AnSubmitConfig {
    pub from: String,
    pub light_client: String,
    pub confirm_timeout_secs: u64,
}

pub fn build_submit_params(proof: &[u8], public_inputs: &[u8]) -> serde_json::Value {
    json!({
        "proof": hex::encode(proof),
        "publicInputs": hex::encode(public_inputs),
    })
}

pub struct AnInterfaceSubmitter<C: IAckiNacki> {
    client: std::sync::Arc<C>,
    config: AnSubmitConfig,
}

impl<C: IAckiNacki> AnInterfaceSubmitter<C> {
    pub fn new(client: std::sync::Arc<C>, config: AnSubmitConfig) -> Self {
        Self {
            client,
            config,
        }
    }

    async fn call(
        &self,
        function: &str,
        params: serde_json::Value,
    ) -> Result<SubmitOutcome, RelayerError> {
        let from = ExtendedAddress::parse(&self.config.from).map_err(RelayerError::from)?;
        let to = ExtendedAddress::parse(&self.config.light_client).map_err(RelayerError::from)?;
        let call = ContractCallRequest {
            from,
            to,
            function: function.to_string(),
            params,
        };
        let sent = match self.client.call_contract(call).await {
            Ok(hash) => hash,
            Err(e) => return Ok(classify_call_error(function, &e.to_string())),
        };
        let receipt = self
            .client
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
}

fn classify_call_error(function: &str, msg: &str) -> SubmitOutcome {
    if msg.contains("244") || msg.contains("STALE_UPDATE") {
        return SubmitOutcome::AlreadyOnHead;
    }
    SubmitOutcome::Rejected {
        reason: format!("{function} failed: {msg}"),
    }
}
