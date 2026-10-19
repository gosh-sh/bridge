//! Circle Iris `GET /v2/messages/{sourceDomainId}`.
//!
//! Mainnet host: `https://iris-api.circle.com`.
//! Sandbox host: `https://iris-api-sandbox.circle.com`.
//! The caller passes the host; this module does not pick one.

use alloy::primitives::Address;
use serde::Deserialize;

use crate::error::RelayerError;

/// One attested CCTP message, ready to pass to `receiveBurn`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestedMessage {
    pub message: Vec<u8>,
    pub attestation: Vec<u8>,
}

/// What Iris (or a parsed body) says about a burn transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrisClass {
    /// 404, an empty list, or a message Circle has not signed yet.
    Pending,
    /// At least one v2 message is signed and names our receiver.
    /// `complete` is false when another message in the same response is
    /// still pending, so the source transaction must stay in the queue.
    Ready {
        messages: Vec<AttestedMessage>,
        complete: bool,
    },
    /// Messages exist, none are pending, and none are for this receiver.
    /// Retrying will not help.
    Unrelated,
}

#[derive(Debug, Deserialize)]
struct MessagesBody {
    #[serde(default)]
    messages: Vec<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    message: Option<String>,
    attestation: Option<String>,
    #[serde(rename = "cctpVersion")]
    cctp_version: Option<u32>,
    status: Option<String>,
    #[serde(rename = "decodedMessage")]
    decoded_message: Option<DecodedMessage>,
}

#[derive(Debug, Deserialize)]
struct DecodedMessage {
    #[serde(rename = "destinationCaller")]
    destination_caller: Option<String>,
    #[serde(rename = "decodedMessageBody")]
    decoded_message_body: Option<DecodedBody>,
}

#[derive(Debug, Deserialize)]
struct DecodedBody {
    #[serde(rename = "mintRecipient")]
    mint_recipient: Option<String>,
}

/// HTTP client for one Iris host. `base` is the origin, without a path.
pub struct IrisClient {
    http: reqwest::Client,
    base: String,
}

impl IrisClient {
    pub fn new(base: impl Into<String>) -> Result<Self, RelayerError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent("acki-nacki-bridge-cctp-forwarder")
            .build()
            .map_err(|e| RelayerError::other(format!("Iris HTTP client: {e}")))?;
        let base = base.into().trim_end_matches('/').to_string();
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(RelayerError::other(format!(
                "CCTP_IRIS_URL must be an http(s) origin, got {base}"
            )));
        }
        Ok(Self {
            http,
            base,
        })
    }

    /// Look up every message Circle has for this source-domain burn.
    /// A 404 is [`IrisClass::Pending`]: Iris has not observed the burn yet.
    pub async fn lookup(
        &self,
        source_domain: u32,
        tx_hash: &str,
        receiver: Address,
    ) -> Result<IrisClass, RelayerError> {
        let tx_hash = normalize_tx_hash(tx_hash)?;
        let url = format!(
            "{}/v2/messages/{source_domain}?transactionHash={tx_hash}",
            self.base
        );
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| RelayerError::other(format!("Iris request failed: {e}")))?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(IrisClass::Pending);
        }
        if status.as_u16() == 429 {
            return Err(RelayerError::other("Iris rate limited (HTTP 429)"));
        }
        if !status.is_success() {
            return Err(RelayerError::other(format!("Iris HTTP {status}")));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| RelayerError::other(format!("Iris body: {e}")))?;
        classify_messages(&bytes, receiver)
    }
}

/// Parse an Iris JSON body and keep the messages this receiver may submit.
pub fn classify_messages(body: &[u8], receiver: Address) -> Result<IrisClass, RelayerError> {
    let parsed: MessagesBody =
        serde_json::from_slice(body).map_err(|e| RelayerError::other(format!("Iris JSON: {e}")))?;
    Ok(classify_parsed(&parsed.messages, receiver))
}

fn classify_parsed(messages: &[RawMessage], receiver: Address) -> IrisClass {
    if messages.is_empty() {
        return IrisClass::Pending;
    }
    let mut ready = Vec::new();
    let mut saw_pending = false;
    let mut saw_reject = false;
    for message in messages {
        match readiness(message, receiver) {
            Readiness::Ready(attested) => ready.push(attested),
            Readiness::Pending => saw_pending = true,
            Readiness::Reject => saw_reject = true,
        }
    }
    if !ready.is_empty() {
        IrisClass::Ready {
            messages: ready,
            complete: !saw_pending,
        }
    } else if saw_pending {
        IrisClass::Pending
    } else if saw_reject {
        IrisClass::Unrelated
    } else {
        IrisClass::Pending
    }
}

enum Readiness {
    Ready(AttestedMessage),
    Pending,
    Reject,
}

fn readiness(message: &RawMessage, receiver: Address) -> Readiness {
    if message.cctp_version == Some(1) {
        return Readiness::Reject;
    }
    if message.status.as_deref() == Some("pending_confirmations") {
        return Readiness::Pending;
    }
    let Some(message_bytes) = message.message.as_deref().and_then(decode_payload) else {
        return Readiness::Pending;
    };
    let Some(attestation) = message.attestation.as_deref().and_then(decode_payload) else {
        return Readiness::Pending;
    };
    match caller_matches(&message.decoded_message, receiver) {
        Caller::Mismatch => Readiness::Reject,
        Caller::Match | Caller::Unknown => Readiness::Ready(AttestedMessage {
            message: message_bytes,
            attestation,
        }),
    }
}

enum Caller {
    Match,
    Mismatch,
    Unknown,
}

fn caller_matches(decoded: &Option<DecodedMessage>, receiver: Address) -> Caller {
    let Some(decoded) = decoded else {
        return Caller::Unknown;
    };
    if let Some(caller) = decoded.destination_caller.as_deref() {
        if let Some(address) = parse_address(caller) {
            return if address == receiver {
                Caller::Match
            } else {
                Caller::Mismatch
            };
        }
    }
    if let Some(body) = &decoded.decoded_message_body {
        if let Some(recipient) = body.mint_recipient.as_deref() {
            if let Some(address) = parse_address(recipient) {
                return if address == receiver {
                    Caller::Match
                } else {
                    Caller::Mismatch
                };
            }
        }
    }
    Caller::Unknown
}

fn parse_address(raw: &str) -> Option<Address> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "0x" {
        return None;
    }
    raw.parse().ok()
}

/// `0x`-hex payload. `PENDING`, empty, and bare `0x` are not a payload yet.
fn decode_payload(raw: &str) -> Option<Vec<u8>> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("PENDING") || raw == "0x" {
        return None;
    }
    let hex = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    hex::decode(hex).ok()
}

pub fn normalize_tx_hash(raw: &str) -> Result<String, RelayerError> {
    let raw = raw.trim();
    let hex = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RelayerError::other(format!(
            "not a 32-byte transaction hash: {raw}"
        )));
    }
    Ok(format!("0x{}", hex.to_ascii_lowercase()))
}

/// Circle domain id this chain uses, when we know it. Testnets reuse the
/// mainnet domain ids (Arbitrum Sepolia is 3, same as Arbitrum One).
/// `None` means "no opinion".
pub fn expected_domain(chain_id: u64) -> Option<u32> {
    match chain_id {
        1 | 11_155_111 => Some(0),
        10 | 11_155_420 => Some(2),
        42_161 | 421_614 => Some(3),
        8_453 | 84_532 => Some(6),
        137 | 80_002 => Some(7),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIVER: &str = "0x00000000000000000000000000000000000000aa";

    fn receiver() -> Address {
        RECEIVER.parse().unwrap()
    }

    fn complete(caller: &str) -> String {
        format!(
            r#"{{
                "messages": [{{
                    "message": "0x0102",
                    "attestation": "0xabcd",
                    "cctpVersion": 2,
                    "status": "complete",
                    "decodedMessage": {{
                        "destinationCaller": "{caller}",
                        "decodedMessageBody": {{ "mintRecipient": "{caller}" }}
                    }}
                }}],
                "sourceTxHash": "0x1111111111111111111111111111111111111111111111111111111111111111"
            }}"#
        )
    }

    #[test]
    fn complete_message_for_our_receiver_is_ready() {
        let class = classify_messages(complete(RECEIVER).as_bytes(), receiver()).unwrap();
        match class {
            IrisClass::Ready {
                messages,
                complete,
            } => {
                assert!(complete);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].message, vec![0x01, 0x02]);
                assert_eq!(messages[0].attestation, vec![0xab, 0xcd]);
            },
            other => panic!("expected ready, got {other:?}"),
        }
    }

    #[test]
    fn wrong_caller_is_unrelated() {
        let other = "0x00000000000000000000000000000000000000bb";
        let class = classify_messages(complete(other).as_bytes(), receiver()).unwrap();
        assert_eq!(class, IrisClass::Unrelated);
    }

    #[test]
    fn pending_attestation_is_not_submitted() {
        let body = r#"{
            "messages": [{
                "message": "0x",
                "attestation": "PENDING",
                "cctpVersion": 2,
                "status": "pending_confirmations"
            }],
            "sourceTxHash": "0x1111111111111111111111111111111111111111111111111111111111111111"
        }"#;
        let class = classify_messages(body.as_bytes(), receiver()).unwrap();
        assert_eq!(class, IrisClass::Pending);
    }

    #[test]
    fn empty_list_and_legacy_version() {
        let empty = br#"{"messages":[],"sourceTxHash":"0x11"}"#;
        assert_eq!(
            classify_messages(empty, receiver()).unwrap(),
            IrisClass::Pending
        );
        let v1 = complete(RECEIVER).replace("\"cctpVersion\": 2", "\"cctpVersion\": 1");
        assert_eq!(
            classify_messages(v1.as_bytes(), receiver()).unwrap(),
            IrisClass::Unrelated
        );
    }

    #[test]
    fn tx_hash_is_normalised() {
        let raw = "0xAAAA111111111111111111111111111111111111111111111111111111111111";
        assert_eq!(
            normalize_tx_hash(raw).unwrap(),
            "0xaaaa111111111111111111111111111111111111111111111111111111111111"
        );
        assert!(normalize_tx_hash("0x1234").is_err());
    }

    #[test]
    fn arbitrum_domain_is_three() {
        assert_eq!(expected_domain(42_161), Some(3));
        assert_eq!(expected_domain(421_614), Some(3));
        assert_eq!(expected_domain(11_155_111), Some(0));
        assert_eq!(expected_domain(999), None);
    }

    #[test]
    fn a_second_pending_message_keeps_the_transaction_open() {
        let body = format!(
            r#"{{
                "messages": [
                    {{
                        "message": "0x0102",
                        "attestation": "0xabcd",
                        "cctpVersion": 2,
                        "status": "complete",
                        "decodedMessage": {{
                            "destinationCaller": "{RECEIVER}",
                            "decodedMessageBody": {{ "mintRecipient": "{RECEIVER}" }}
                        }}
                    }},
                    {{
                        "message": "0x",
                        "attestation": "PENDING",
                        "cctpVersion": 2,
                        "status": "pending_confirmations"
                    }}
                ],
                "sourceTxHash": "0x1111111111111111111111111111111111111111111111111111111111111111"
            }}"#
        );
        match classify_messages(body.as_bytes(), receiver()).unwrap() {
            IrisClass::Ready {
                messages,
                complete,
            } => {
                assert!(!complete);
                assert_eq!(messages.len(), 1);
            },
            other => panic!("expected incomplete ready, got {other:?}"),
        }
    }
}
