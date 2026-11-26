//! Prometheus metrics of `eth-lc-relayer arbitrum-anchors` (`metrics`
//! facade).
//!
//! [`crate::arbitrum::tick`] records them into whatever recorder the binary
//! installs; `eth-lc-relayer arbitrum-anchors --metrics-addr` installs the
//! Prometheus text exporter (`GET /metrics`). Without a recorder the macros
//! are no-ops, so the library, `arbitrum-vector` and the tests carry no cost.
//! The metrics of `eth-lc-relayer daemon` are in [`crate::metrics`].
//!
//! Names are stable operator API (dashboards and alert rules key on them).
//! The counters named after a `TickReport` field add up that field over the
//! rounds, a failed round's part included. Every counter is there at zero
//! from the start ([`init`]), except the series of
//! `arbitrum_anchors_rpc_requests_total`, which show with the first request
//! of their kind:
//!
//! | Metric | Type | Labels | Meaning |
//! |---|---|---|---|
//! | `arbitrum_anchors_ticks_total` | counter | `outcome` | One per round: `ok`, `error` (a read of either chain or of Acki Nacki failed; the next round starts where this one stopped) |
//! | `arbitrum_anchors_last_tick_timestamp_seconds` | gauge | | Unix time the last round ended, whatever its outcome |
//! | `arbitrum_anchors_last_successful_tick_timestamp_seconds` | gauge | | Unix time the last round ended with `ok` |
//! | `arbitrum_anchors_new_deposit_blocks_total` | counter | | `new_deposit_blocks`: Arbitrum blocks with a deposit found that the bridge does not accept yet |
//! | `arbitrum_anchors_batches_taken_total` | counter | | `batches_taken`: batches at whose last block the slot proofs of the waiting deposit blocks were taken |
//! | `arbitrum_anchors_assertions_matched_total` | counter | | `assertions_matched`: assertions that end at a batch with proofs held |
//! | `arbitrum_anchors_assertions_missed_total` | counter | | `assertions_missed`: assertions that end at a batch with no proofs held. Every assertion counts here while no deposit block waits; with one waiting, its deposits wait for the next assertion |
//! | `arbitrum_anchors_proofs_sent_total` | counter | | `proofs_sent`: `proveBlock` calls Acki Nacki took |
//! | `arbitrum_anchors_proofs_failed_total` | counter | | `proofs_failed`: `proveBlock` calls Acki Nacki refused; each is sent again later |
//! | `arbitrum_anchors_accepted_total` | counter | | `accepted`: deposit blocks the bridge was found to accept after a `proveBlock` |
//! | `arbitrum_anchors_spans_sent_total` | counter | | `spans_sent`: `submitAncestry` calls for an assertion's parent-chain block that Acki Nacki took |
//! | `arbitrum_anchors_spans_failed_total` | counter | | `spans_failed`: the ones it refused, at most three per block |
//! | `arbitrum_anchors_waiting_for_parent` | gauge | | `waiting_for_parent` of the last `ok` round: proofs whose assertion landed in a parent-chain block the bridge does not accept yet |
//! | `arbitrum_anchors_pending_deposit_blocks` | gauge | | Deposit blocks the bridge does not accept yet (state file `pending`) |
//! | `arbitrum_anchors_oldest_pending_deposit_block` | gauge | | Number of the oldest of them, 0 when none. A block further than 393 168 blocks behind the Arbitrum head can no longer be proven |
//! | `arbitrum_anchors_expired_deposit_blocks_total` | counter | | Deposit blocks given up as older than the 393 168 blocks an Arbitrum block's state remembers; each needs the owner's `setAcceptedBlockHash` |
//! | `arbitrum_anchors_held_batch_ends` | gauge | | Batch ends with proofs, waiting for an assertion (state file `batch_ends`) |
//! | `arbitrum_anchors_ready_proofs` | gauge | | Proofs whose assertion has landed, waiting for the bridge (state file `ready`) |
//! | `arbitrum_anchors_unplaced_batches` | gauge | | Batches the Arbitrum node could not place yet (state file `unplaced_batches`) |
//! | `arbitrum_anchors_parent_cursor_block` | gauge | | Last parent-chain block scanned for batches and assertions (state file `parent_cursor`) |
//! | `arbitrum_anchors_parent_head_block` | gauge | | The parent chain's head as last read |
//! | `arbitrum_anchors_deposit_cursor_block` | gauge | | Last Arbitrum block scanned for deposits (state file `deposit_cursor`) |
//! | `arbitrum_anchors_arbitrum_head_block` | gauge | | The Arbitrum chain's head as last read |
//! | `arbitrum_anchors_rpc_requests_total` | counter | `rpc`, `method`, `outcome` | JSON-RPC requests sent: `rpc` is `parent` (`--parent-rpc-url`), `arbitrum` (`--arbitrum-rpc-url`) or `proof` (`--arbitrum-proof-rpc-url`, or `--arbitrum-rpc-url` in its place); `method` is the JSON-RPC method; `outcome` is `ok`, `rpc_error` (the node answered with a JSON-RPC error, whatever the HTTP status: a reverted `eth_call`, state it no longer keeps, a rate limit sent as one) or `transport_error` (no JSON-RPC answer: a timeout, a refused connection, an HTTP error status with any other body) |
//! | `arbitrum_anchors_build_info` | gauge | `version` | Always 1; the crate version |
//! | `arbitrum_anchors_start_timestamp_seconds` | gauge | | Unix time the relayer started |

use std::task::{Context, Poll};

use alloy::{
    rpc::json_rpc::{RequestPacket, ResponsePacket},
    transports::{TransportError, TransportFut},
};
use metrics::{counter, describe_counter, describe_gauge, gauge, Unit};
use tower::{Layer, Service};

use crate::{
    arbitrum::{AnchorState, TickReport},
    metrics::unix_now,
};

pub const TICKS_TOTAL: &str = "arbitrum_anchors_ticks_total";
pub const LAST_TICK_TIMESTAMP_SECONDS: &str = "arbitrum_anchors_last_tick_timestamp_seconds";
pub const LAST_SUCCESSFUL_TICK_TIMESTAMP_SECONDS: &str =
    "arbitrum_anchors_last_successful_tick_timestamp_seconds";
pub const NEW_DEPOSIT_BLOCKS_TOTAL: &str = "arbitrum_anchors_new_deposit_blocks_total";
pub const BATCHES_TAKEN_TOTAL: &str = "arbitrum_anchors_batches_taken_total";
pub const ASSERTIONS_MATCHED_TOTAL: &str = "arbitrum_anchors_assertions_matched_total";
pub const ASSERTIONS_MISSED_TOTAL: &str = "arbitrum_anchors_assertions_missed_total";
pub const PROOFS_SENT_TOTAL: &str = "arbitrum_anchors_proofs_sent_total";
pub const PROOFS_FAILED_TOTAL: &str = "arbitrum_anchors_proofs_failed_total";
pub const ACCEPTED_TOTAL: &str = "arbitrum_anchors_accepted_total";
pub const SPANS_SENT_TOTAL: &str = "arbitrum_anchors_spans_sent_total";
pub const SPANS_FAILED_TOTAL: &str = "arbitrum_anchors_spans_failed_total";
pub const WAITING_FOR_PARENT: &str = "arbitrum_anchors_waiting_for_parent";
pub const PENDING_DEPOSIT_BLOCKS: &str = "arbitrum_anchors_pending_deposit_blocks";
pub const OLDEST_PENDING_DEPOSIT_BLOCK: &str = "arbitrum_anchors_oldest_pending_deposit_block";
pub const EXPIRED_DEPOSIT_BLOCKS_TOTAL: &str = "arbitrum_anchors_expired_deposit_blocks_total";
pub const HELD_BATCH_ENDS: &str = "arbitrum_anchors_held_batch_ends";
pub const READY_PROOFS: &str = "arbitrum_anchors_ready_proofs";
pub const UNPLACED_BATCHES: &str = "arbitrum_anchors_unplaced_batches";
pub const PARENT_CURSOR_BLOCK: &str = "arbitrum_anchors_parent_cursor_block";
pub const PARENT_HEAD_BLOCK: &str = "arbitrum_anchors_parent_head_block";
pub const DEPOSIT_CURSOR_BLOCK: &str = "arbitrum_anchors_deposit_cursor_block";
pub const ARBITRUM_HEAD_BLOCK: &str = "arbitrum_anchors_arbitrum_head_block";
pub const RPC_REQUESTS_TOTAL: &str = "arbitrum_anchors_rpc_requests_total";
pub const BUILD_INFO: &str = "arbitrum_anchors_build_info";
pub const START_TIMESTAMP_SECONDS: &str = "arbitrum_anchors_start_timestamp_seconds";

/// The `rpc` label of [`RPC_REQUESTS_TOTAL`] for each of the three endpoints.
pub const RPC_PARENT: &str = "parent";
pub const RPC_ARBITRUM: &str = "arbitrum";
pub const RPC_PROOF: &str = "proof";

/// Register the help strings and units with the installed recorder. Call once
/// after the exporter is installed; harmless without one.
pub fn describe() {
    describe_counter!(
        TICKS_TOTAL,
        "Rounds by outcome (label `outcome`: ok, error)."
    );
    describe_gauge!(
        LAST_TICK_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time the last round ended, whatever its outcome."
    );
    describe_gauge!(
        LAST_SUCCESSFUL_TICK_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time the last round ended without an error."
    );
    describe_counter!(
        NEW_DEPOSIT_BLOCKS_TOTAL,
        "Arbitrum blocks with a deposit found that the bridge does not accept yet."
    );
    describe_counter!(
        BATCHES_TAKEN_TOTAL,
        "Batches at whose last block slot proofs were taken."
    );
    describe_counter!(
        ASSERTIONS_MATCHED_TOTAL,
        "Assertions that end at a batch with proofs held."
    );
    describe_counter!(
        ASSERTIONS_MISSED_TOTAL,
        "Assertions that end at a batch with no proofs held; every one while no deposit waits."
    );
    describe_counter!(PROOFS_SENT_TOTAL, "proveBlock calls Acki Nacki took.");
    describe_counter!(PROOFS_FAILED_TOTAL, "proveBlock calls Acki Nacki refused.");
    describe_counter!(
        ACCEPTED_TOTAL,
        "Deposit blocks the bridge was found to accept after a proveBlock."
    );
    describe_counter!(
        SPANS_SENT_TOTAL,
        "submitAncestry calls for an assertion's parent-chain block that Acki Nacki took."
    );
    describe_counter!(
        SPANS_FAILED_TOTAL,
        "submitAncestry calls for an assertion's parent-chain block that Acki Nacki refused."
    );
    describe_gauge!(
        WAITING_FOR_PARENT,
        "Proofs whose assertion's parent-chain block the bridge does not accept yet, at the last \
         round without an error."
    );
    describe_gauge!(
        PENDING_DEPOSIT_BLOCKS,
        "Deposit blocks the bridge does not accept yet."
    );
    describe_gauge!(
        OLDEST_PENDING_DEPOSIT_BLOCK,
        "Number of the oldest deposit block the bridge does not accept yet; 0 when none."
    );
    describe_counter!(
        EXPIRED_DEPOSIT_BLOCKS_TOTAL,
        "Deposit blocks given up as too old to prove; each needs the owner's setAcceptedBlockHash."
    );
    describe_gauge!(
        HELD_BATCH_ENDS,
        "Batch ends with proofs, waiting for an assertion."
    );
    describe_gauge!(
        READY_PROOFS,
        "Proofs whose assertion has landed, waiting for the bridge."
    );
    describe_gauge!(
        UNPLACED_BATCHES,
        "Batches the Arbitrum node could not place yet."
    );
    describe_gauge!(
        PARENT_CURSOR_BLOCK,
        "Last parent-chain block scanned for batches and assertions."
    );
    describe_gauge!(PARENT_HEAD_BLOCK, "The parent chain's head as last read.");
    describe_gauge!(
        DEPOSIT_CURSOR_BLOCK,
        "Last Arbitrum block scanned for deposits."
    );
    describe_gauge!(
        ARBITRUM_HEAD_BLOCK,
        "The Arbitrum chain's head as last read."
    );
    describe_counter!(
        RPC_REQUESTS_TOTAL,
        "JSON-RPC requests sent (labels `rpc`: parent, arbitrum, proof; `method`; `outcome`: ok, \
         rpc_error, transport_error)."
    );
    describe_gauge!(
        BUILD_INFO,
        "Always 1; label `version` is the crate version."
    );
    describe_gauge!(
        START_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time the relayer started."
    );
}

/// The counters named after a `TickReport` field, with what a round adds to
/// each.
fn report_counters(report: &TickReport) -> [(&'static str, usize); 9] {
    [
        (NEW_DEPOSIT_BLOCKS_TOTAL, report.new_deposit_blocks),
        (BATCHES_TAKEN_TOTAL, report.batches_taken),
        (ASSERTIONS_MATCHED_TOTAL, report.assertions_matched),
        (ASSERTIONS_MISSED_TOTAL, report.assertions_missed),
        (PROOFS_SENT_TOTAL, report.proofs_sent),
        (PROOFS_FAILED_TOTAL, report.proofs_failed),
        (ACCEPTED_TOTAL, report.accepted),
        (SPANS_SENT_TOTAL, report.spans_sent),
        (SPANS_FAILED_TOTAL, report.spans_failed),
    ]
}

/// Once at start: stamp the process gauges, and put every counter whose
/// labels are known ahead in `/metrics` at zero. A series that first shows
/// with its first event hides that event from `increase()` and `rate()`, so
/// an alert on a rare outcome would miss the first one.
pub fn init() {
    gauge!(BUILD_INFO, "version" => env!("CARGO_PKG_VERSION")).set(1.0);
    gauge!(START_TIMESTAMP_SECONDS).set(unix_now());
    for ok in [true, false] {
        counter!(TICKS_TOTAL, "outcome" => tick_label(ok)).increment(0);
    }
    for (name, _) in report_counters(&TickReport::default()) {
        counter!(name).increment(0);
    }
    counter!(EXPIRED_DEPOSIT_BLOCKS_TOTAL).increment(0);
}

fn tick_label(ok: bool) -> &'static str {
    if ok {
        "ok"
    } else {
        "error"
    }
}

/// One round ended: what it did (also the part a failed round got through),
/// how it ended and what the state holds now.
pub fn record_tick(report: &TickReport, ok: bool, state: &AnchorState) {
    for (name, n) in report_counters(report) {
        counter!(name).increment(n as u64);
    }
    counter!(TICKS_TOTAL, "outcome" => tick_label(ok)).increment(1);
    let now = unix_now();
    gauge!(LAST_TICK_TIMESTAMP_SECONDS).set(now);
    if ok {
        gauge!(LAST_SUCCESSFUL_TICK_TIMESTAMP_SECONDS).set(now);
        // A round that failed before the proofs counted only some of them.
        gauge!(WAITING_FOR_PARENT).set(report.waiting_for_parent as f64);
    }
    gauge!(PENDING_DEPOSIT_BLOCKS).set(state.pending.len() as f64);
    gauge!(OLDEST_PENDING_DEPOSIT_BLOCK)
        .set(state.pending.keys().next().copied().unwrap_or(0) as f64);
    gauge!(HELD_BATCH_ENDS).set(state.batch_ends.len() as f64);
    gauge!(READY_PROOFS).set(state.ready.len() as f64);
    gauge!(UNPLACED_BATCHES).set(state.unplaced_batches.len() as f64);
    gauge!(PARENT_CURSOR_BLOCK).set(state.parent_cursor as f64);
    gauge!(DEPOSIT_CURSOR_BLOCK).set(state.deposit_cursor as f64);
}

/// The Arbitrum chain's head as a round read it.
pub fn set_arbitrum_head(block: u64) {
    gauge!(ARBITRUM_HEAD_BLOCK).set(block as f64);
}

/// The parent chain's head as a round read it.
pub fn set_parent_head(block: u64) {
    gauge!(PARENT_HEAD_BLOCK).set(block as f64);
}

/// One deposit block given up as too old to prove.
pub fn count_expired_deposit_block() {
    counter!(EXPIRED_DEPOSIT_BLOCKS_TOTAL).increment(1);
}

/// A transport layer that counts every JSON-RPC request one endpoint is sent
/// in [`RPC_REQUESTS_TOTAL`], by method and by how it ended. `rpc` names the
/// endpoint: [`RPC_PARENT`], [`RPC_ARBITRUM`] or [`RPC_PROOF`].
///
/// The HTTP transport below hands on a JSON-RPC error body as an answer
/// whatever the HTTP status, so a rate limit sent as one (HTTP 429 with an
/// error object) counts as `rpc_error`, like a reverted call; only a request
/// with no JSON-RPC answer is a `transport_error`.
#[derive(Clone, Copy, Debug)]
pub struct CountRpcRequests {
    rpc: &'static str,
}

impl CountRpcRequests {
    pub const fn new(rpc: &'static str) -> Self {
        Self {
            rpc,
        }
    }
}

impl<S> Layer<S> for CountRpcRequests {
    type Service = CountedRpc<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CountedRpc {
            inner,
            rpc: self.rpc,
        }
    }
}

/// The transport [`CountRpcRequests`] wraps.
#[derive(Clone, Debug)]
pub struct CountedRpc<S> {
    inner: S,
    rpc: &'static str,
}

impl<S> Service<RequestPacket> for CountedRpc<S>
where
    S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError> + Send + 'static,
    S::Future: Send + 'static,
{
    type Error = TransportError;
    type Future = TransportFut<'static>;
    type Response = ResponsePacket;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let methods: Vec<String> = request.method_names().map(str::to_owned).collect();
        let rpc = self.rpc;
        let answer = self.inner.call(request);
        Box::pin(async move {
            let answer = answer.await;
            let outcome = match &answer {
                Ok(packet) if packet.is_error() => "rpc_error",
                Ok(_) => "ok",
                Err(_) => "transport_error",
            };
            for method in methods {
                counter!(RPC_REQUESTS_TOTAL, "rpc" => rpc, "method" => method, "outcome" => outcome)
                    .increment(1);
            }
            answer
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use alloy::{
        rpc::json_rpc::{ErrorPayload, Id, Request, Response, ResponsePayload},
        transports::TransportErrorKind,
    };
    use metrics_util::debugging::DebuggingRecorder;
    use serde_json::value::RawValue;

    use super::*;
    use crate::metrics::{declared_and_documented, testing::Recorded};

    #[test]
    fn every_metric_is_in_the_table() {
        let (declared, documented) =
            declared_and_documented(include_str!("arbitrum_metrics.rs"), "arbitrum_anchors_");
        assert_eq!(declared, documented);
        assert_eq!(declared.len(), 26);
    }

    /// `init` puts every counter with labels known ahead in `/metrics` at
    /// zero, so the first failed round is an increase from zero.
    #[test]
    fn the_counters_are_there_at_zero_from_the_start() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, init);
        let m = Recorded::take(&snapshotter);

        assert_eq!(
            m.get(BUILD_INFO, &[("version", env!("CARGO_PKG_VERSION"))]),
            Some(1.0)
        );
        assert!(m.get(START_TIMESTAMP_SECONDS, &[]) > Some(0.0));
        for outcome in ["ok", "error"] {
            assert_eq!(
                m.get(TICKS_TOTAL, &[("outcome", outcome)]),
                Some(0.0),
                "{outcome}"
            );
        }
        for name in [
            NEW_DEPOSIT_BLOCKS_TOTAL,
            BATCHES_TAKEN_TOTAL,
            ASSERTIONS_MATCHED_TOTAL,
            ASSERTIONS_MISSED_TOTAL,
            PROOFS_SENT_TOTAL,
            PROOFS_FAILED_TOTAL,
            ACCEPTED_TOTAL,
            SPANS_SENT_TOTAL,
            SPANS_FAILED_TOTAL,
            EXPIRED_DEPOSIT_BLOCKS_TOTAL,
        ] {
            assert_eq!(m.get(name, &[]), Some(0.0), "{name}");
        }
    }

    /// A transport that answers each request with the next of the answers it
    /// was given.
    #[derive(Clone)]
    struct Answers(Arc<Mutex<VecDeque<Result<ResponsePacket, TransportError>>>>);

    impl Service<RequestPacket> for Answers {
        type Error = TransportError;
        type Future = TransportFut<'static>;
        type Response = ResponsePacket;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: RequestPacket) -> Self::Future {
            let next = self.0.lock().unwrap().pop_front().expect("an answer left");
            Box::pin(async move { next })
        }
    }

    fn request(method: &'static str) -> RequestPacket {
        RequestPacket::Single(Request::new(method, Id::Number(1), ()).serialize().unwrap())
    }

    fn answer(payload: ResponsePayload) -> Result<ResponsePacket, TransportError> {
        Ok(ResponsePacket::Single(Response {
            id: Id::Number(1),
            payload,
        }))
    }

    /// Each request is counted once it ends, under its endpoint and method:
    /// answered, refused by the node (a reverted call, or a rate limit the
    /// node sends as a JSON-RPC error), or not answered at all. The answer
    /// itself goes through untouched.
    #[tokio::test]
    async fn every_request_is_counted_by_endpoint_method_and_how_it_ended() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _local = metrics::set_default_local_recorder(&recorder);

        let answers = Answers(Arc::new(Mutex::new(VecDeque::from([
            answer(ResponsePayload::Success(
                RawValue::from_string("\"0x10\"".into()).unwrap(),
            )),
            answer(ResponsePayload::Success(
                RawValue::from_string("[]".into()).unwrap(),
            )),
            answer(ResponsePayload::Failure(ErrorPayload {
                code: -32000,
                message: "execution reverted".into(),
                data: None,
            })),
            // What the HTTP transport makes of a 429 with a JSON-RPC error
            // body: an answer, not a transport error.
            answer(ResponsePayload::Failure(ErrorPayload {
                code: 429,
                message: "Monthly capacity limit exceeded".into(),
                data: None,
            })),
            Err(TransportErrorKind::custom_str("connection reset")),
        ]))));
        let mut arbitrum = CountRpcRequests::new(RPC_ARBITRUM).layer(answers.clone());
        let mut proof = CountRpcRequests::new(RPC_PROOF).layer(answers);

        let head = arbitrum.call(request("eth_blockNumber")).await.unwrap();
        assert!(head.is_success(), "the answer is passed on");
        arbitrum.call(request("eth_getLogs")).await.unwrap();
        let reverted = arbitrum.call(request("eth_call")).await.unwrap();
        assert!(reverted.is_error(), "and so is the node's refusal");
        let limited = proof.call(request("eth_getProof")).await.unwrap();
        assert_eq!(limited.first_error_code(), Some(429));
        assert!(proof.call(request("eth_getProof")).await.is_err());

        let m = Recorded::take(&snapshotter);
        let count = |rpc, method, outcome| {
            m.get(RPC_REQUESTS_TOTAL, &[
                ("rpc", rpc),
                ("method", method),
                ("outcome", outcome),
            ])
        };
        assert_eq!(count("arbitrum", "eth_blockNumber", "ok"), Some(1.0));
        assert_eq!(count("arbitrum", "eth_getLogs", "ok"), Some(1.0));
        assert_eq!(count("arbitrum", "eth_call", "rpc_error"), Some(1.0));
        assert_eq!(count("proof", "eth_getProof", "rpc_error"), Some(1.0));
        assert_eq!(count("proof", "eth_getProof", "transport_error"), Some(1.0));
        assert_eq!(
            m.get(RPC_REQUESTS_TOTAL, &[]),
            Some(5.0),
            "and nothing else"
        );
    }
}
