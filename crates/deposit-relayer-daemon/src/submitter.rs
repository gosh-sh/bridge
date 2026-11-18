//! [`AnSubmitter`] — abstraction over "how do we deliver the deposit to Acki
//! Nacki and finalise it?".
//!
//! The AN-side entry point is `eccBridge.finalizeDeposit(uint256 chainId,
//! bytes header, uint16 txIndex, bytes[] receiptProof, uint16 logIndex)`. The
//! bridge requires `keccak256(header)` to be an accepted block of `chainId`
//! (exit 224 otherwise, before it accepts the message), proves the receipt of
//! transaction `txIndex` against the header's `receiptsRoot`, reads log
//! `logIndex` of that receipt as the bridge's `Deposit` event, and deploys a
//! per-deposit `DepositVoucher`, whose `confirmTokenDeposit` call back into the
//! bridge mints. Every deposit field comes from the proven receipt; the
//! relayer cannot change any of them.
//!
//! The bridge records whether that identity is absent, pending, or minted.
//! A pending record can be minted with `retryDeposit` without another proof.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use acki_nacki_interface::{ContractCallRequest, ExtendedAddress, IAckiNacki, TransactionStatus};
use alloy::primitives::U256;
use async_trait::async_trait;
use serde_json::json;
use tokio::time::Instant;

use crate::{
    error::RelayerError,
    types::DepositEvent,
    voucher::DepositIdentity,
    witness::{check_gas_credit, check_witness_matches, DepositWitness},
};

/// Outcome of an [`AnSubmitter::submit`] call.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    /// The bridge reports that the deposit was minted.
    Finalized {
        /// `Some` for the live client (the AN tx hash), `None` for the mock.
        tx_hash: Option<[u8; 32]>,
    },
    /// The `depositId` nullifier was already consumed (someone else
    /// finalised it, or we retried after a successful submit we didn't
    /// observe). Treated as success for progress purposes.
    AlreadyFinalized,
    /// AN rejected the submission (receipt proof refused, malformed call).
    /// The relayer logs and records an attempt.
    Rejected { reason: String },
    /// The deposit is waiting for a retry or mint confirmation.
    /// Distinct from [`Rejected`] — the relayer retries without counting it
    /// as a failed attempt.
    Pending { reason: String },
}

#[async_trait]
pub trait AnSubmitter: Send + Sync {
    /// Read the AN bridge's canonical-chain gate before building the receipt
    /// proof. Errors must propagate: an unreadable gate is not permission to
    /// submit.
    async fn is_block_accepted(&self, event: &DepositEvent) -> Result<bool, RelayerError>;

    /// Check the bridge before proving. A minted deposit is done, a pending
    /// deposit can be retried by its identity, and an absent deposit needs a
    /// proof.
    async fn resume_deposit(
        &self,
        id: DepositIdentity,
    ) -> Result<Option<SubmitOutcome>, RelayerError>;

    /// Send `finalizeDeposit` with `witness` and finalise the deposit.
    async fn submit(
        &self,
        event: &DepositEvent,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError>;
}

// ─────────────────────────────────────────────────────────────────────
// MockAnSubmitter — in-memory nullifier mirror
// ─────────────────────────────────────────────────────────────────────

/// Verifier decision callback — returns `true` if the AN bridge would accept
/// the receipt proof. Tests inject `false` to drive the rejection path.
pub type VerifierDecision = Arc<dyn Fn(&DepositWitness) -> bool + Send + Sync>;

/// In-memory mirror of the AN `finalizeDeposit` voucher semantics.
pub struct MockAnSubmitter {
    inner: Mutex<MockInner>,
    verifier: VerifierDecision,
}

struct MockInner {
    nullifiers: HashSet<U256>,
    finalized_log: Vec<u64>,
}

impl MockAnSubmitter {
    /// Every receipt proof accepted (the common happy path).
    pub fn accepting() -> Self {
        Self::with_verifier(Arc::new(|_| true))
    }

    pub fn with_verifier(verifier: VerifierDecision) -> Self {
        Self {
            inner: Mutex::new(MockInner {
                nullifiers: HashSet::new(),
                finalized_log: Vec::new(),
            }),
            verifier,
        }
    }

    /// Pre-seed a finalised deposit (simulate another relayer having already
    /// processed it).
    pub fn seed_finalized(&self, deposit_id: u64) {
        let mut inner = self.inner.lock().expect("poisoned lock");
        inner.nullifiers.insert(U256::from(deposit_id));
    }

    pub fn finalized_count(&self) -> usize {
        self.inner
            .lock()
            .expect("poisoned lock")
            .finalized_log
            .len()
    }

    pub fn finalized_log(&self) -> Vec<u64> {
        self.inner
            .lock()
            .expect("poisoned lock")
            .finalized_log
            .clone()
    }
}

#[async_trait]
impl AnSubmitter for MockAnSubmitter {
    async fn is_block_accepted(&self, _: &DepositEvent) -> Result<bool, RelayerError> {
        Ok(true)
    }

    async fn resume_deposit(
        &self,
        id: DepositIdentity,
    ) -> Result<Option<SubmitOutcome>, RelayerError> {
        Ok(self
            .inner
            .lock()
            .expect("poisoned lock")
            .nullifiers
            .contains(&id.deposit_id)
            .then_some(SubmitOutcome::AlreadyFinalized))
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError> {
        check_witness_matches(event, witness)?;
        check_gas_credit(witness)?;

        let mut inner = self.inner.lock().expect("poisoned lock");
        if inner.nullifiers.contains(&U256::from(event.deposit_id)) {
            return Ok(SubmitOutcome::AlreadyFinalized);
        }
        if !(self.verifier)(witness) {
            return Ok(SubmitOutcome::Rejected {
                reason: "the mock bridge refused the receipt proof".to_string(),
            });
        }
        inner.nullifiers.insert(U256::from(event.deposit_id));
        inner.finalized_log.push(event.deposit_id);
        Ok(SubmitOutcome::Finalized {
            tx_hash: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────
// AnInterfaceSubmitter — live delivery over IAckiNacki
// ─────────────────────────────────────────────────────────────────────

/// Static configuration for the live submitter. `finalizeDeposit` carries
/// no caller-supplied gas: the bridge pays its own after `tvm.accept()`.
#[derive(Clone, Debug)]
pub struct AnSubmitConfig {
    /// Relayer signer in SDK 3.0 `dapp_id::account_id` form.
    pub from: String,
    /// Bridge contract in SDK 3.0 `dapp_id::account_id` form.
    pub token_bridge: String,
    /// Seconds to wait for the finalize tx to confirm.
    pub confirm_timeout_secs: u64,
}

/// The result of the bridge's `getDepositStatus` getter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepositStatus {
    Absent,
    Pending,
    Minted,
}

impl From<&DepositEvent> for DepositIdentity {
    fn from(event: &DepositEvent) -> Self {
        Self::new(
            event.deposit_id,
            event.source_contract,
            event.source_chain_id,
        )
    }
}

impl DepositIdentity {
    pub(crate) fn new(
        deposit_id: u64,
        contract: alloy::primitives::Address,
        chain_id: u64,
    ) -> Self {
        Self {
            deposit_id: U256::from(deposit_id),
            contract_addr: U256::from_be_slice(contract.as_slice()),
            chain_id: U256::from(chain_id),
        }
    }

    fn params(self) -> serde_json::Value {
        json!({
            "depositId": self.deposit_id.to_string(),
            "contractAddr": self.contract_addr.to_string(),
            "chainId": self.chain_id.to_string(),
        })
    }
}

const RETRY_DEPOSIT: &str = "retryDeposit";

pub struct AnInterfaceSubmitter<C: IAckiNacki> {
    client: Arc<C>,
    config: AnSubmitConfig,
}

impl<C: IAckiNacki> AnInterfaceSubmitter<C> {
    pub fn new(client: Arc<C>, config: AnSubmitConfig) -> Self {
        Self {
            client,
            config,
        }
    }

    pub async fn deposit_status(&self, id: DepositIdentity) -> Result<DepositStatus, RelayerError> {
        let output = self
            .client
            .run_getter(&self.config.token_bridge, "getDepositStatus", id.params())
            .await?;
        let status = &output["status"];
        match status.as_u64().or_else(|| status.as_str()?.parse().ok()) {
            Some(0) => Ok(DepositStatus::Absent),
            Some(1) => Ok(DepositStatus::Pending),
            Some(2) => Ok(DepositStatus::Minted),
            _ => Err(RelayerError::other(format!(
                "getDepositStatus: unknown status {status}"
            ))),
        }
    }

    async fn resume(&self, id: DepositIdentity) -> Result<Option<SubmitOutcome>, RelayerError> {
        match self.deposit_status(id).await? {
            DepositStatus::Absent => Ok(None),
            DepositStatus::Pending => self.send(id, RETRY_DEPOSIT, id.params()).await.map(Some),
            DepositStatus::Minted => Ok(Some(SubmitOutcome::AlreadyFinalized)),
        }
    }

    /// Submit a saved witness. Read the identity from the proven log so older
    /// witness files can also resume a pending deposit.
    pub async fn submit_witness(
        &self,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError> {
        let (contract, deposit_id) = witness.deposit_identity().map_err(RelayerError::other)?;
        let id = DepositIdentity {
            deposit_id: U256::from_be_bytes(deposit_id.0),
            contract_addr: U256::from_be_slice(contract.as_slice()),
            chain_id: U256::from(witness.chain_id),
        };
        if let Some(outcome) = self.resume(id).await? {
            return Ok(outcome);
        }
        check_gas_credit(witness)?;
        self.send(id, "finalizeDeposit", witness.finalize_params())
            .await
    }

    async fn send(
        &self,
        id: DepositIdentity,
        function: &str,
        params: serde_json::Value,
    ) -> Result<SubmitOutcome, RelayerError> {
        let call = ContractCallRequest {
            from: ExtendedAddress::parse(&self.config.from)?,
            to: ExtendedAddress::parse(&self.config.token_bridge)?,
            function: function.to_string(),
            params,
        };
        let sent = match self.client.call_contract(call).await {
            Ok(hash) => hash,
            Err(e) => return Ok(classify_call_error(function, &e.to_string())),
        };
        let receipt = match self
            .client
            .wait_for_confirmation(&sent, self.config.confirm_timeout_secs)
            .await
        {
            Ok(receipt) => receipt,
            Err(e) if function == RETRY_DEPOSIT => {
                return Ok(SubmitOutcome::Pending {
                    reason: format!("{function} {} confirmation failed: {e}", hex::encode(sent)),
                })
            },
            Err(e) => return Err(e.into()),
        };
        match receipt.status {
            TransactionStatus::Reverted | TransactionStatus::Failed => {
                return Ok(classify_reverted(
                    function,
                    receipt.exit_code,
                    &format!("{function} {:?}", receipt.status),
                ))
            },
            TransactionStatus::Pending => {
                return Ok(SubmitOutcome::Pending {
                    reason: format!("{function} tx still pending after timeout"),
                })
            },
            TransactionStatus::Confirmed | TransactionStatus::Included => {},
        }

        let deadline = Instant::now() + Duration::from_secs(self.config.confirm_timeout_secs);
        loop {
            let status = self.deposit_status(id).await;
            if matches!(status, Ok(DepositStatus::Minted)) {
                return Ok(SubmitOutcome::Finalized {
                    tx_hash: Some(sent),
                });
            }
            let now = Instant::now();
            if now >= deadline {
                let observed = match status {
                    Ok(s) => format!("{s:?}"),
                    Err(e) => e.to_string(),
                };
                return Ok(SubmitOutcome::Pending {
                    reason: format!(
                        "{function} {} confirmed, awaiting mint (getDepositStatus: {observed})",
                        hex::encode(sent)
                    ),
                });
            }
            tokio::time::sleep((deadline - now).min(Duration::from_secs(2))).await;
        }
    }
}

// eccBridge `modifiers/errors.sol`.
const EXIT_ZERO_AMOUNT: i32 = 204;
const EXIT_OVERFLOW: i32 = 214;
const EXIT_UNSUPPORTED_SRC_CHAIN: i32 = 222;
const EXIT_UNKNOWN_BLOCK: i32 = 224;
const EXIT_ZERO_RECIPIENT: i32 = 230;
const EXIT_PAUSED: i32 = 231;
const EXIT_TRANSFER_TOO_LARGE: i32 = 234;
const EXIT_TVL_CAP: i32 = 235;
const EXIT_TOKEN_UNREGISTERED: i32 = 241;
const EXIT_TOKEN_DEPOSITS_DISABLED: i32 = 243;
const EXIT_NOT_DEPOSIT_LOG: i32 = 244;

// eccBridge `EthTrie.sol`: the receipt proof.
const EXIT_TRIE_SHORT: i32 = 300;
const EXIT_TRIE_RLP: i32 = 301;
const EXIT_TRIE_NODE_HASH: i32 = 302;
const EXIT_TRIE_PATH: i32 = 303;
const EXIT_TRIE_INLINE: i32 = 304;
const EXIT_TRIE_ABSENT: i32 = 305;
const EXIT_NO_LOG: i32 = 307;
const EXIT_RECEIPT_STATUS: i32 = 308;

/// What 300–305 share: the proof does not lead from the header's
/// `receiptsRoot` to the receipt.
const RECEIPT_PROOF_HINT: &str = "the receipt proof does not match the header's receiptsRoot; the \
                                  relayer builds a fresh one on the next attempt. If it keeps \
                                  failing, the source RPC serves receipts that are not the \
                                  block's: point --rpc-url at another node";

/// Names the `finalizeDeposit` revert and, where the cause is bridge
/// configuration rather than a bad deposit, says which call fixes it. The
/// fail-closed settings must be re-set by hand after every deploy or code
/// upgrade (they are not threaded through `onCodeUpgrade`), so without this the
/// first deposit after a redeploy reads as a broken bridge.
fn describe_exit_code(code: i32) -> Option<&'static str> {
    match code {
        EXIT_ZERO_AMOUNT => Some("ERR_ZERO_AMOUNT — the proven deposit amount is zero"),
        EXIT_OVERFLOW => Some("ERR_OVERFLOW — the proven deposit amount does not fit a uint64"),
        EXIT_NOT_DEPOSIT_LOG => Some(
            "ERR_NOT_DEPOSIT_LOG — the log at logIndex of the proven receipt is not a Deposit \
             event in the shape the bridge reads (three topics, four data words); the EVM bridge \
             emitting it is not the one this AN bridge was built for",
        ),
        EXIT_TRIE_SHORT => Some("EthTrie ERR_SHORT — a receipt proof item ends early"),
        EXIT_TRIE_RLP => Some("EthTrie ERR_RLP — a receipt proof item is malformed"),
        EXIT_TRIE_NODE_HASH => {
            Some("EthTrie ERR_NODE_HASH — a receipt proof node does not hash to its parent")
        },
        EXIT_TRIE_PATH => Some("EthTrie ERR_PATH — the receipt proof does not follow txIndex"),
        EXIT_TRIE_INLINE => Some(
            "EthTrie ERR_INLINE — the receipt trie of this block has a node under 32 bytes, which \
             the bridge cannot walk; the deposit needs the EVM bridge operator",
        ),
        EXIT_TRIE_ABSENT => Some("EthTrie ERR_ABSENT — no receipt at txIndex in this block"),
        EXIT_NO_LOG => Some(
            "EthTrie ERR_NO_LOG — the proven receipt has no log at logIndex; logIndex counts \
             within the receipt, not the block",
        ),
        EXIT_RECEIPT_STATUS => Some(
            "EthTrie ERR_RECEIPT_STATUS — the deposit transaction failed on the source chain, so \
             there is no deposit to finalize",
        ),
        EXIT_UNSUPPORTED_SRC_CHAIN => Some(
            "ERR_UNSUPPORTED_SRC_CHAIN — the proven (chainId, source contract) pair is not \
             allowlisted; owner must call setTrustedL1Bridge(chainId, l1Bridge, true)",
        ),
        EXIT_UNKNOWN_BLOCK => Some(
            "ERR_UNKNOWN_BLOCK — the deposit's block is not in the canonical-chain anchor set; \
             confirm canonicality with `scripts/deposit_anchor_params.py --verify`, then the \
             chain's anchor writer must admit it — the owner's setAcceptedBlockHash(chainId, \
             blockHash, true) while owner anchors are enabled for that chain, the light client \
             for Ethereum, the Arbitrum block verifier (`eth-lc-relayer arbitrum-anchors`) for \
             Arbitrum",
        ),
        EXIT_ZERO_RECIPIENT => Some("ERR_ZERO_RECIPIENT — the proven AN recipient is zero"),
        EXIT_TRANSFER_TOO_LARGE => {
            Some("ERR_TRANSFER_TOO_LARGE — the proven deposit exceeds the per-transfer ceiling")
        },
        _ => None,
    }
}

fn rejected(code: i32, context: &str, detail: &str) -> SubmitOutcome {
    let reason = match (describe_exit_code(code), code) {
        (
            Some(hint),
            EXIT_TRIE_SHORT | EXIT_TRIE_RLP | EXIT_TRIE_NODE_HASH | EXIT_TRIE_PATH
            | EXIT_TRIE_ABSENT,
        ) => format!("{hint}; {RECEIPT_PROOF_HINT} (exit_code={code}): {detail}"),
        (Some(hint), _) => format!("{hint} (exit_code={code}): {detail}"),
        (None, _) => format!("{context} (exit_code={code}): {detail}"),
    };
    SubmitOutcome::Rejected {
        reason,
    }
}

fn classify_exit(code: i32, context: &str, detail: &str) -> SubmitOutcome {
    match code {
        // Duplicate constructor, existing registration, or a retry racing
        // another mint. Only the bridge's next status read settles the id.
        51 | 245 | 246 => SubmitOutcome::Pending {
            reason: format!(
                "deposit state may have changed; checking getDepositStatus on retry \
                 (exit_code={code}): {detail}"
            ),
        },
        // A paused lane rejects every deposit; counting that against the
        // deposit would park healthy deposits for the length of the pause.
        EXIT_PAUSED => SubmitOutcome::Pending {
            reason: format!(
                "ERR_PAUSED — the bridge lane is paused (setPaused); retrying until it resumes \
                 (exit_code={code}): {detail}"
            ),
        },
        EXIT_TVL_CAP => SubmitOutcome::Pending {
            reason: format!(
                "ERR_TVL_CAP — source-chain deposit capacity is full; waiting for withdrawals \
                 (exit_code={code}): {detail}"
            ),
        },
        EXIT_TOKEN_UNREGISTERED => SubmitOutcome::Pending {
            reason: format!(
                "ERR_TOKEN_UNREGISTERED — this token is not registered for the source chain; \
                 retrying until the owner calls setBridgeToken (exit_code={code}): {detail}"
            ),
        },
        EXIT_TOKEN_DEPOSITS_DISABLED => SubmitOutcome::Pending {
            reason: format!(
                "ERR_TOKEN_DEPOSITS_DISABLED — deposits of this token are closed on AN; retrying \
                 until setBridgeToken reopens them (exit_code={code}): {detail}"
            ),
        },
        400 | 402 => SubmitOutcome::Pending {
            reason: format!(
                "external message already seen or expired; check getDepositStatus before retrying \
                 (exit_code={code}): {detail}"
            ),
        },
        401 => rejected(
            code,
            "external message expiry exceeds the five-minute window",
            detail,
        ),
        code => rejected(code, context, detail),
    }
}

fn classify_reverted(function: &str, exit_code: Option<i32>, detail: &str) -> SubmitOutcome {
    match exit_code {
        Some(code) => classify_exit(code, "deposit call reverted", detail),
        None if function == RETRY_DEPOSIT => SubmitOutcome::Pending {
            reason: format!("{detail} without an exit code; checking getDepositStatus on retry"),
        },
        None => SubmitOutcome::Rejected {
            reason: format!("deposit tx status = {detail}"),
        },
    }
}

fn classify_call_error(function: &str, msg: &str) -> SubmitOutcome {
    // Prefer acki_nacki_interface parser when the tvm-sdk feature is on; fall
    // back to a local regex-free scan so this module still builds without it.
    match parse_exit_code_loose(msg) {
        Some(code) => classify_exit(code, "deposit call failed", msg),
        // process_message wraps transport errors in TransactionFailed too.
        None if function == RETRY_DEPOSIT => SubmitOutcome::Pending {
            reason: format!(
                "{function} returned no exit code; checking getDepositStatus on retry: {msg}"
            ),
        },
        None => SubmitOutcome::Rejected {
            reason: format!("deposit call failed: {msg}"),
        },
    }
}

fn parse_exit_code_loose(msg: &str) -> Option<i32> {
    if let Some(c) = acki_nacki_interface::parse_exit_code_from_message(msg) {
        return Some(c);
    }
    for marker in ["exit_code=Some(", "exit_code=", "local_exit_code="] {
        if let Some(rest) = msg.split(marker).nth(1) {
            let digits: String = rest
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

fn accepted_block_hash_params(event: &DepositEvent) -> serde_json::Value {
    json!({
        "chainId": event.source_chain_id.to_string(),
        "blockHash": format!("{:#x}", event.block_hash),
    })
}

#[async_trait]
impl<C: IAckiNacki> AnSubmitter for AnInterfaceSubmitter<C> {
    async fn is_block_accepted(&self, event: &DepositEvent) -> Result<bool, RelayerError> {
        let output = self
            .client
            .run_getter(
                &self.config.token_bridge,
                "isAcceptedBlockHash",
                accepted_block_hash_params(event),
            )
            .await?;
        output
            .get("value0")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| RelayerError::other("isAcceptedBlockHash: expected a boolean value0"))
    }

    async fn resume_deposit(
        &self,
        id: DepositIdentity,
    ) -> Result<Option<SubmitOutcome>, RelayerError> {
        self.resume(id).await
    }

    async fn submit(
        &self,
        event: &DepositEvent,
        witness: &DepositWitness,
    ) -> Result<SubmitOutcome, RelayerError> {
        check_witness_matches(event, witness)?;
        check_gas_credit(witness)?;
        self.send(event.into(), "finalizeDeposit", witness.finalize_params())
            .await
    }
}

#[cfg(test)]
mod tests {
    use acki_nacki_interface::MockAckiNacki;
    use alloy::primitives::{Address, B256};

    use super::*;
    use crate::{types::DepositEvent, witness::MockWitnessBuilder};

    #[test]
    fn anchor_getter_params_use_decimal_chain_and_ethereum_order_hex_hash() {
        let mut deposit = event(0);
        deposit.source_chain_id = 11_155_111;
        deposit.block_hash = "0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
            .parse()
            .unwrap();
        assert_eq!(
            accepted_block_hash_params(&deposit),
            json!({
                "chainId": "11155111",
                "blockHash": "0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            })
        );
    }

    #[tokio::test]
    async fn anchor_getter_requires_a_boolean_and_propagates_read_errors() {
        use acki_nacki_interface::MockAckiNacki;
        let client = Arc::new(MockAckiNacki::new());
        let sub = AnInterfaceSubmitter::new(client.clone(), AnSubmitConfig {
            from: "unused".into(),
            token_bridge: "unused".into(),
            confirm_timeout_secs: 1,
        });
        assert!(sub.is_block_accepted(&event(0)).await.is_err());
        for value in [false, true] {
            client.set_getter_response(json!({"value0": value}));
            assert_eq!(sub.is_block_accepted(&event(0)).await.unwrap(), value);
        }
        for output in [json!({}), json!({"value0": "false"}), json!({"value0": 1})] {
            client.set_getter_response(output);
            assert!(sub.is_block_accepted(&event(0)).await.is_err());
        }
        assert_eq!(client.transaction_count(), 0);
    }

    fn event(id: u64) -> DepositEvent {
        DepositEvent {
            deposit_id: id,
            token: Address::repeat_byte(0x7c),
            sender: Address::repeat_byte(0x11),
            amount: U256::from(42u64),
            an_account: B256::repeat_byte(0x77),
            timestamp: U256::ZERO,
            tx_hash: B256::repeat_byte(0xaa),
            log_index: 0,
            block_number: 1,
            block_hash: B256::repeat_byte(0xcd),
            source_contract: Address::repeat_byte(0x22),
            source_chain_id: 1,
        }
    }

    fn witness(ev: &DepositEvent) -> DepositWitness {
        MockWitnessBuilder::new().witness_for(ev)
    }

    #[tokio::test]
    async fn mock_finalizes_then_replay_is_already_finalized() {
        let sub = MockAnSubmitter::accepting();
        let ev = event(1);
        let b = witness(&ev);

        assert!(sub.resume_deposit((&ev).into()).await.unwrap().is_none());
        assert!(matches!(
            sub.submit(&ev, &b).await.unwrap(),
            SubmitOutcome::Finalized { .. }
        ));
        assert!(matches!(
            sub.resume_deposit((&ev).into()).await.unwrap(),
            Some(SubmitOutcome::AlreadyFinalized)
        ));
        assert!(matches!(
            sub.submit(&ev, &b).await.unwrap(),
            SubmitOutcome::AlreadyFinalized
        ));
        assert_eq!(sub.finalized_count(), 1);
    }

    #[tokio::test]
    async fn mock_rejects_when_verifier_says_no() {
        let sub = MockAnSubmitter::with_verifier(Arc::new(|_| false));
        let ev = event(2);
        let b = witness(&ev);
        match sub.submit(&ev, &b).await.unwrap() {
            SubmitOutcome::Rejected {
                reason,
            } => assert!(reason.contains("refused the receipt proof")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_oversized_witness_is_refused_before_anything_is_sent() {
        use acki_nacki_interface::MockAckiNacki;

        let client = Arc::new(MockAckiNacki::new());
        let sub = AnInterfaceSubmitter::new(client.clone(), AnSubmitConfig {
            from: "unused".into(),
            token_bridge: "unused".into(),
            confirm_timeout_secs: 1,
        });
        let ev = event(4);
        let big = MockWitnessBuilder::with_header_len(48 * 1024).witness_for(&ev);
        assert!(matches!(
            sub.submit(&ev, &big).await,
            Err(RelayerError::Unprovable(_))
        ));
        assert_eq!(client.transaction_count(), 0);
    }

    fn live_submitter(client: Arc<MockAckiNacki>) -> AnInterfaceSubmitter<MockAckiNacki> {
        let dapp = "ee".repeat(32);
        AnInterfaceSubmitter::new(client, AnSubmitConfig {
            from: format!("{dapp}::{}", "ff".repeat(32)),
            token_bridge: format!("{dapp}::{dapp}"),
            confirm_timeout_secs: 0,
        })
    }

    #[tokio::test]
    async fn confirmed_finalize_waits_for_minted_status() {
        let client = Arc::new(MockAckiNacki::new());
        let sub = live_submitter(client.clone());
        let ev = event(3);
        for output in [json!({}), json!({"status":"0"}), json!({"status":1})] {
            client.set_getter_response(output);
            assert!(matches!(
                sub.submit(&ev, &witness(&ev)).await.unwrap(),
                SubmitOutcome::Pending { .. }
            ));
        }
        client.set_getter_response(json!({"status":"2"}));
        assert!(matches!(
            sub.submit(&ev, &witness(&ev)).await.unwrap(),
            SubmitOutcome::Finalized {
                tx_hash: Some(_)
            }
        ));
    }

    #[tokio::test]
    async fn resume_only_submits_a_retry_for_a_pending_deposit() {
        let client = Arc::new(MockAckiNacki::new());
        let sub = live_submitter(client.clone());
        let ev = event(5);
        assert!(sub.resume_deposit((&ev).into()).await.is_err());
        for output in [json!({}), json!({"status":3}), json!({"status":false})] {
            client.set_getter_response(output);
            assert!(sub.resume_deposit((&ev).into()).await.is_err());
        }
        client.set_getter_response(json!({"status":0}));
        assert!(sub.resume_deposit((&ev).into()).await.unwrap().is_none());
        client.set_getter_response(json!({"status":"2"}));
        assert!(matches!(
            sub.resume_deposit((&ev).into()).await.unwrap(),
            Some(SubmitOutcome::AlreadyFinalized)
        ));
        assert_eq!(client.transaction_count(), 0);
        client.set_getter_response(json!({"status":"1"}));
        assert!(matches!(
            sub.resume_deposit((&ev).into()).await.unwrap(),
            Some(SubmitOutcome::Pending { .. })
        ));
        assert_eq!(client.transaction_count(), 1);
        assert_eq!(
            DepositIdentity::from(&ev).params(),
            json!({
                "depositId": "5",
                "contractAddr": U256::from_be_slice(&[0x22;20]).to_string(),
                "chainId": "1",
            })
        );
    }

    #[test]
    fn paused_bridge_is_pending_not_rejected() {
        for outcome in [
            classify_call_error("finalizeDeposit", "contract call aborted (exit_code=231)"),
            classify_reverted("finalizeDeposit", Some(231), "Reverted"),
        ] {
            match outcome {
                SubmitOutcome::Pending {
                    reason,
                } => assert!(reason.contains("ERR_PAUSED"), "{reason}"),
                other => panic!("expected Pending, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn unclassified_retry_submission_failure_keeps_the_deposit_pending() {
        let client = Arc::new(MockAckiNacki::new());
        let sub = live_submitter(client.clone());
        let ev = event(6);
        client.set_getter_response(json!({"status":1}));
        client.enable_fail_mode();

        let outcome = sub.resume((&ev).into()).await.unwrap();
        assert!(
            matches!(outcome, Some(SubmitOutcome::Pending { .. })),
            "{outcome:?}"
        );
        assert!(matches!(
            sub.submit(&ev, &witness(&ev)).await.unwrap(),
            SubmitOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn a_retry_abort_needs_an_exit_code_to_reject_the_deposit() {
        let abort = "Transaction failed: contract call aborted (exit_code=None, aborted=true)";
        assert!(matches!(
            classify_reverted("retryDeposit", None, "retryDeposit Reverted"),
            SubmitOutcome::Pending { .. }
        ));
        assert!(matches!(
            classify_reverted("finalizeDeposit", None, "finalizeDeposit Reverted"),
            SubmitOutcome::Rejected { .. }
        ));
        assert!(matches!(
            classify_call_error("retryDeposit", abort),
            SubmitOutcome::Pending { .. }
        ));
        assert!(matches!(
            classify_call_error(
                "retryDeposit",
                "contract call aborted (exit_code=Some(234))"
            ),
            SubmitOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn duplicate_constructor_needs_a_status_check() {
        assert!(matches!(
            classify_call_error(
                "finalizeDeposit",
                "contract call aborted (exit_code=Some(51))"
            ),
            SubmitOutcome::Pending { .. }
        ));
        assert!(matches!(
            classify_reverted("finalizeDeposit", Some(51), "Reverted"),
            SubmitOutcome::Pending { .. }
        ));
    }

    #[test]
    fn receipt_proof_reverts_are_rejected_with_a_hint() {
        for (code, name) in [
            (244, "ERR_NOT_DEPOSIT_LOG"),
            (300, "ERR_SHORT"),
            (301, "ERR_RLP"),
            (302, "ERR_NODE_HASH"),
            (303, "ERR_PATH"),
            (304, "ERR_INLINE"),
            (305, "ERR_ABSENT"),
            (307, "ERR_NO_LOG"),
            (308, "ERR_RECEIPT_STATUS"),
        ] {
            for outcome in [
                classify_call_error(
                    "finalizeDeposit",
                    &format!("contract call aborted (exit_code={code})"),
                ),
                classify_reverted("finalizeDeposit", Some(code), "Reverted"),
            ] {
                match outcome {
                    SubmitOutcome::Rejected {
                        reason,
                    } => {
                        assert!(reason.contains(name), "{code}: {reason}");
                        assert!(reason.contains(&format!("exit_code={code}")), "{reason}");
                        let rebuilt = reason.contains("builds a fresh one");
                        assert_eq!(
                            rebuilt,
                            (300..=305).contains(&code) && code != 304,
                            "{reason}"
                        );
                    },
                    other => panic!("expected Rejected for {code}, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn capacity_and_message_retries_do_not_park_a_deposit() {
        for code in [235, 241, 243, 245, 246, 400, 402] {
            for result in [
                classify_reverted("finalizeDeposit", Some(code), "Reverted"),
                classify_call_error("finalizeDeposit", &format!("exit_code={code}")),
            ] {
                assert!(
                    matches!(result, SubmitOutcome::Pending { .. }),
                    "{code}: {result:?}"
                );
            }
        }
        assert!(matches!(
            classify_reverted("finalizeDeposit", Some(234), "Reverted"),
            SubmitOutcome::Rejected { .. }
        ));
    }

    /// The fail-closed config reverts must name the call that fixes them — an
    /// operator seeing a bare `exit_code=224` after a redeploy has no way to
    /// tell an unconfigured bridge from a rejected deposit.
    #[test]
    fn config_reverts_name_the_setter_that_fixes_them() {
        for (code, err_name, setter) in [
            (222, "ERR_UNSUPPORTED_SRC_CHAIN", "setTrustedL1Bridge"),
            (224, "ERR_UNKNOWN_BLOCK", "setAcceptedBlockHash"),
        ] {
            for outcome in [
                classify_call_error(
                    "finalizeDeposit",
                    &format!("contract call aborted (exit_code={code})"),
                ),
                classify_reverted("finalizeDeposit", Some(code), "Reverted"),
            ] {
                match outcome {
                    SubmitOutcome::Rejected {
                        reason,
                    } => {
                        assert!(reason.contains(err_name), "{code}: {reason}");
                        assert!(reason.contains(setter), "{code}: {reason}");
                        assert!(reason.contains(&code.to_string()), "{code}: {reason}");
                    },
                    other => panic!("expected Rejected for {code}, got {other:?}"),
                }
            }
        }
    }
}
