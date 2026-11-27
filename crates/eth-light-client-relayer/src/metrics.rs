//! Prometheus metrics of `eth-lc-relayer daemon` (`metrics` facade).
//!
//! Every `metrics::*` macro in this crate records into whatever recorder the
//! binary installs; `eth-lc-relayer daemon --metrics-addr` installs the
//! Prometheus text exporter (`GET /metrics`). Without a recorder the macros
//! are no-ops, so the library and its tests carry no cost. The metrics of
//! `eth-lc-relayer arbitrum-anchors` are in [`crate::arbitrum_metrics`].
//!
//! Names are stable operator API (dashboards and alert rules key on them).
//! Every series of a counter or a histogram is there at zero from the start
//! ([`init`]), so `increase()` sees the first event of a kind, unless it
//! comes before the first scrape (the gap the first poll after a long stop
//! finds):
//!
//! | Metric | Type | Labels | Meaning |
//! |---|---|---|---|
//! | `eth_lc_relayer_ticks_total` | counter | `outcome` | One per round of the daemon's loop: `submitted` (a `submitUpdate` was accepted), `rotated` (a `submitRotate` was accepted), `already_on_head`, `unchanged` (no checkpoint is queued), `proving` (the oldest queued checkpoint is still being proven), `rotate_required`, `proof_failed`, `an_rejected`, `an_pending`, `weak_subjectivity_lag`, `error`. The in-memory `RelayerMetrics` counters are sums of these: `submitted_total` = `submitted` + `rotated`, `unchanged_total` = `unchanged` + `already_on_head`, `an_rejected_total` = `an_rejected` + `an_pending`, `rotate_required_total` and `proof_failed_total` their own outcome, `ticks_total` every outcome |
//! | `eth_lc_relayer_last_tick_timestamp_seconds` | gauge | | Unix time the last round ended, whatever its outcome |
//! | `eth_lc_relayer_last_successful_tick_timestamp_seconds` | gauge | | Unix time the last round ended with `submitted`, `rotated`, `already_on_head`, `unchanged` or `proving`: the outcomes after which the daemon does not back off |
//! | `eth_lc_relayer_checkpoint_failures_total` | counter | `stage` | Failed attempts to get the oldest queued checkpoint on chain: `prove` (the step proof did not come out), `submit` (Acki Nacki did not take it or did not answer), `rotate` (it waits at a period boundary for the committee rotate) |
//! | `eth_lc_relayer_queued_checkpoints` | gauge | | Checkpoints taken from the beacon node and not on chain yet (`<state file>.steps/`) |
//! | `eth_lc_relayer_last_finalized_slot` | gauge | | Finalized slot of the newest checkpoint on the light client, as the daemon recorded it (state file `last_finalized_slot`); absent until the first one |
//! | `eth_lc_relayer_last_committee_period` | gauge | | Sync-committee period the light client holds, as the daemon recorded it (state file `last_committee_period`); absent until the first update |
//! | `eth_lc_relayer_beacon_finalized_slot` | gauge | | Finalized slot of the beacon node's last `finality_update` the watcher read |
//! | `eth_lc_relayer_beacon_polls_total` | counter | `outcome` | The watcher's reads of the beacon node's `finality_update`, one per slot: `ok` (it answered with an update of the network the state file is pinned to), `error` (it did not answer, the answer did not parse, or it is another network's) |
//! | `eth_lc_relayer_missed_checkpoints_total` | counter | | Checkpoints the watcher never saw (the daemon or the beacon node was down); their blocks are closed with `submitAncestry` |
//! | `eth_lc_relayer_an_calls_total` | counter | `function`, `outcome` | Messages sent to Acki Nacki: `function` is `submitUpdate`, `submitRotate`, `submitAncestry`, `rePushAnchor`, `setCommitteeCommitment` (the owner hop) or `flipOwner`; `outcome` is `accepted`, `already_on_head`, `rejected`, `pending`, `not_sent` or `error` |
//! | `eth_lc_relayer_step_proof_duration_seconds` | histogram | `outcome` | Wall time of one step prover run that finished: `ok` or `failed`. `_count` is the number of proofs |
//! | `eth_lc_relayer_step_proofs_running` | gauge | | Step provers still running at the end of the last round, at most `--parallel-proofs` |
//! | `eth_lc_relayer_rotate_proof_duration_seconds` | histogram | `outcome` | Wall time of one background rotate, all of its stages this run proved: `ok` or `failed` |
//! | `eth_lc_relayer_rotate_running` | gauge | | 1 while a rotate is being proven in the background, else 0 |
//! | `eth_lc_relayer_owed_spans` | gauge | | Spans of blocks still owed to `submitAncestry` (state file `owed_spans`) |
//! | `eth_lc_relayer_ancestry_spans_total` | counter | `outcome` | Owed spans that left the queue: `closed` (sent, or nothing lay between the two checkpoints), `given_up` (their blocks need the owner's `setAcceptedBlockHash`) |
//! | `eth_lc_relayer_build_info` | gauge | `version` | Always 1; the crate version |
//! | `eth_lc_relayer_start_timestamp_seconds` | gauge | | Unix time the daemon started |

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use metrics::{
    counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram, Unit,
};

use crate::{error::RelayerError, submitter::SubmitOutcome};

pub const TICKS_TOTAL: &str = "eth_lc_relayer_ticks_total";
pub const LAST_TICK_TIMESTAMP_SECONDS: &str = "eth_lc_relayer_last_tick_timestamp_seconds";
pub const LAST_SUCCESSFUL_TICK_TIMESTAMP_SECONDS: &str =
    "eth_lc_relayer_last_successful_tick_timestamp_seconds";
pub const CHECKPOINT_FAILURES: &str = "eth_lc_relayer_checkpoint_failures_total";
pub const QUEUED_CHECKPOINTS: &str = "eth_lc_relayer_queued_checkpoints";
pub const LAST_FINALIZED_SLOT: &str = "eth_lc_relayer_last_finalized_slot";
pub const LAST_COMMITTEE_PERIOD: &str = "eth_lc_relayer_last_committee_period";
pub const BEACON_FINALIZED_SLOT: &str = "eth_lc_relayer_beacon_finalized_slot";
pub const BEACON_POLLS_TOTAL: &str = "eth_lc_relayer_beacon_polls_total";
pub const MISSED_CHECKPOINTS_TOTAL: &str = "eth_lc_relayer_missed_checkpoints_total";
pub const AN_CALLS_TOTAL: &str = "eth_lc_relayer_an_calls_total";
pub const STEP_PROOF_DURATION_SECONDS: &str = "eth_lc_relayer_step_proof_duration_seconds";
pub const STEP_PROOFS_RUNNING: &str = "eth_lc_relayer_step_proofs_running";
pub const ROTATE_PROOF_DURATION_SECONDS: &str = "eth_lc_relayer_rotate_proof_duration_seconds";
pub const ROTATE_RUNNING: &str = "eth_lc_relayer_rotate_running";
pub const OWED_SPANS: &str = "eth_lc_relayer_owed_spans";
pub const ANCESTRY_SPANS_TOTAL: &str = "eth_lc_relayer_ancestry_spans_total";
pub const BUILD_INFO: &str = "eth_lc_relayer_build_info";
pub const START_TIMESTAMP_SECONDS: &str = "eth_lc_relayer_start_timestamp_seconds";

/// Bucket bounds of [`STEP_PROOF_DURATION_SECONDS`], in seconds: a step proof
/// takes about three minutes with its key stored and seven with the key
/// generation, and is killed at `--prove-timeout-secs` (default 7200).
pub const STEP_PROOF_BUCKETS: &[f64] = &[
    60.0, 120.0, 180.0, 240.0, 300.0, 420.0, 600.0, 900.0, 1200.0, 1800.0, 3600.0, 7200.0,
];

/// Bucket bounds of [`ROTATE_PROOF_DURATION_SECONDS`], in seconds: the three
/// stages take about an hour and a half together, less when a restart finds
/// some of them done, and each is killed at `--rotate-timeout-secs` (default
/// 10800).
pub const ROTATE_PROOF_BUCKETS: &[f64] = &[
    300.0, 900.0, 1800.0, 2700.0, 3600.0, 4500.0, 5400.0, 7200.0, 10800.0, 14400.0, 21600.0,
    32400.0,
];

/// Register the help strings and units with the installed recorder. Call once
/// after the exporter is installed; harmless without one.
pub fn describe() {
    describe_counter!(
        TICKS_TOTAL,
        "Rounds of the daemon's loop by outcome (label `outcome`)."
    );
    describe_gauge!(
        LAST_TICK_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time the last round ended, whatever its outcome."
    );
    describe_gauge!(
        LAST_SUCCESSFUL_TICK_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time the last round ended without a failure (submitted, rotated, already_on_head, \
         unchanged or proving)."
    );
    describe_counter!(
        CHECKPOINT_FAILURES,
        "Failed attempts to get a checkpoint on chain: stage=prove, submit or rotate"
    );
    describe_gauge!(
        QUEUED_CHECKPOINTS,
        "Checkpoints taken from the beacon node and not on chain yet."
    );
    describe_gauge!(
        LAST_FINALIZED_SLOT,
        "Finalized slot of the newest checkpoint on the light client, as the daemon recorded it."
    );
    describe_gauge!(
        LAST_COMMITTEE_PERIOD,
        "Sync-committee period the light client holds, as the daemon recorded it."
    );
    describe_gauge!(
        BEACON_FINALIZED_SLOT,
        "Finalized slot of the beacon node's last finality_update the watcher read."
    );
    describe_counter!(
        BEACON_POLLS_TOTAL,
        "The watcher's reads of the beacon node's finality_update (label `outcome`: ok, error)."
    );
    describe_counter!(
        MISSED_CHECKPOINTS_TOTAL,
        "Checkpoints the watcher never saw; their blocks are closed with submitAncestry."
    );
    describe_counter!(
        AN_CALLS_TOTAL,
        "Messages sent to Acki Nacki (label `function`) by what came back (label `outcome`: \
         accepted, already_on_head, rejected, pending, not_sent, error)."
    );
    describe_histogram!(
        STEP_PROOF_DURATION_SECONDS,
        Unit::Seconds,
        "Wall time of one finished step prover run (label `outcome`: ok, failed)."
    );
    describe_gauge!(
        STEP_PROOFS_RUNNING,
        "Step provers still running at the end of the last round."
    );
    describe_histogram!(
        ROTATE_PROOF_DURATION_SECONDS,
        Unit::Seconds,
        "Wall time of one finished background rotate (label `outcome`: ok, failed)."
    );
    describe_gauge!(
        ROTATE_RUNNING,
        "1 while a rotate is being proven in the background, else 0."
    );
    describe_gauge!(
        OWED_SPANS,
        "Spans of blocks still owed to submitAncestry (state file owed_spans)."
    );
    describe_counter!(
        ANCESTRY_SPANS_TOTAL,
        "Owed spans that left the queue (label `outcome`: closed, given_up)."
    );
    describe_gauge!(
        BUILD_INFO,
        "Always 1; label `version` is the crate version."
    );
    describe_gauge!(
        START_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time the daemon started."
    );
}

/// Seconds since the Unix epoch, as the gauges want it.
pub fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Every `outcome` of [`TICKS_TOTAL`].
pub const TICK_OUTCOMES: &[&str] = &[
    "submitted",
    "rotated",
    "already_on_head",
    "unchanged",
    "proving",
    "rotate_required",
    "proof_failed",
    "an_rejected",
    "an_pending",
    "weak_subjectivity_lag",
    "error",
];

/// Every `stage` of [`CHECKPOINT_FAILURES`].
pub const CHECKPOINT_STAGES: &[&str] = &["prove", "submit", "rotate"];

/// Every `outcome` of [`AN_CALLS_TOTAL`].
pub const SUBMIT_OUTCOMES: &[&str] = &[
    "accepted",
    "already_on_head",
    "rejected",
    "pending",
    "not_sent",
    "error",
];

/// A message the daemon sends to Acki Nacki: the `function` label of
/// [`AN_CALLS_TOTAL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnCall {
    SubmitUpdate,
    SubmitRotate,
    SubmitAncestry,
    RePushAnchor,
    /// The owner hop.
    SetCommitteeCommitment,
    /// `disableOwnerAnchorsOn` and `disableOwnerRotation`, with `--flip-owner`.
    FlipOwner,
}

impl AnCall {
    pub const ALL: [Self; 6] = [
        Self::SubmitUpdate,
        Self::SubmitRotate,
        Self::SubmitAncestry,
        Self::RePushAnchor,
        Self::SetCommitteeCommitment,
        Self::FlipOwner,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::SubmitUpdate => "submitUpdate",
            Self::SubmitRotate => "submitRotate",
            Self::SubmitAncestry => "submitAncestry",
            Self::RePushAnchor => "rePushAnchor",
            Self::SetCommitteeCommitment => "setCommitteeCommitment",
            Self::FlipOwner => "flipOwner",
        }
    }
}

/// Once at daemon start: stamp the process gauges, and put every series of
/// the counters and histograms in `/metrics` at zero. A series that first
/// shows with its first event hides that event from `increase()` and
/// `rate()`, so an alert on a rare outcome would miss the first one.
pub fn init() {
    gauge!(BUILD_INFO, "version" => env!("CARGO_PKG_VERSION")).set(1.0);
    gauge!(START_TIMESTAMP_SECONDS).set(unix_now());
    for outcome in TICK_OUTCOMES {
        counter!(TICKS_TOTAL, "outcome" => *outcome).increment(0);
    }
    for stage in CHECKPOINT_STAGES {
        counter!(CHECKPOINT_FAILURES, "stage" => *stage).increment(0);
    }
    for ok in [true, false] {
        counter!(BEACON_POLLS_TOTAL, "outcome" => ok_label(ok)).increment(0);
        counter!(ANCESTRY_SPANS_TOTAL, "outcome" => span_label(!ok)).increment(0);
        // Registered, no sample: the exporter shows them with a zero count.
        let _ = histogram!(STEP_PROOF_DURATION_SECONDS, "outcome" => proof_label(ok));
        let _ = histogram!(ROTATE_PROOF_DURATION_SECONDS, "outcome" => proof_label(ok));
    }
    counter!(MISSED_CHECKPOINTS_TOTAL).increment(0);
    for call in AnCall::ALL {
        for outcome in SUBMIT_OUTCOMES {
            counter!(AN_CALLS_TOTAL, "function" => call.label(), "outcome" => *outcome)
                .increment(0);
        }
    }
}

/// One round of the daemon's loop ended with `outcome`; `success` when the
/// daemon does not back off after it.
pub fn record_tick(outcome: &'static str, success: bool) {
    counter!(TICKS_TOTAL, "outcome" => outcome).increment(1);
    let now = unix_now();
    gauge!(LAST_TICK_TIMESTAMP_SECONDS).set(now);
    if success {
        gauge!(LAST_SUCCESSFUL_TICK_TIMESTAMP_SECONDS).set(now);
    }
}

/// One more failed attempt at the front checkpoint, at `stage`.
pub fn count_checkpoint_failure(stage: &'static str) {
    counter!(CHECKPOINT_FAILURES, "stage" => stage).increment(1);
}

/// What a message to Acki Nacki came back with, as the `outcome` label.
pub fn submit_outcome_label(sent: &Result<SubmitOutcome, RelayerError>) -> &'static str {
    match sent {
        Ok(SubmitOutcome::Accepted {
            ..
        }) => "accepted",
        Ok(SubmitOutcome::AlreadyOnHead) => "already_on_head",
        Ok(SubmitOutcome::Rejected {
            ..
        }) => "rejected",
        Ok(SubmitOutcome::Pending {
            ..
        }) => "pending",
        Ok(SubmitOutcome::NotSent {
            ..
        }) => "not_sent",
        Err(_) => "error",
    }
}

/// Count one message sent to Acki Nacki and what came back.
pub fn count_an_call(call: AnCall, sent: &Result<SubmitOutcome, RelayerError>) {
    counter!(AN_CALLS_TOTAL, "function" => call.label(), "outcome" => submit_outcome_label(sent))
        .increment(1);
}

/// Count one read of the beacon node's `finality_update` by the watcher.
pub fn count_beacon_poll(ok: bool) {
    counter!(BEACON_POLLS_TOTAL, "outcome" => ok_label(ok)).increment(1);
}

/// The finalized slot of the `finality_update` the watcher just read.
pub fn set_beacon_finalized_slot(slot: u64) {
    gauge!(BEACON_FINALIZED_SLOT).set(slot as f64);
}

/// The watcher took a checkpoint and found `missed` of them missing before
/// it.
pub fn count_missed_checkpoints(missed: u64) {
    counter!(MISSED_CHECKPOINTS_TOTAL).increment(missed);
}

/// The queue holds `queued` checkpoints now: set by the watcher as it takes
/// one, between the rounds that set it with the other gauges.
pub fn set_queued_checkpoints(queued: usize) {
    gauge!(QUEUED_CHECKPOINTS).set(queued as f64);
}

/// One step prover run finished after `elapsed`.
pub fn record_step_proof(elapsed: Duration, ok: bool) {
    histogram!(STEP_PROOF_DURATION_SECONDS, "outcome" => proof_label(ok))
        .record(elapsed.as_secs_f64());
}

/// One background rotate finished after `elapsed`.
pub fn record_rotate_proof(elapsed: Duration, ok: bool) {
    histogram!(ROTATE_PROOF_DURATION_SECONDS, "outcome" => proof_label(ok))
        .record(elapsed.as_secs_f64());
}

/// An owed span left the queue: `given_up` when its blocks now need the
/// owner, otherwise closed.
pub fn count_ancestry_span(given_up: bool) {
    counter!(ANCESTRY_SPANS_TOTAL, "outcome" => span_label(given_up)).increment(1);
}

/// The gauges that mirror what the daemon holds at the end of a round.
pub struct RoundGauges {
    pub queued_checkpoints: usize,
    pub last_finalized_slot: Option<u64>,
    pub last_committee_period: Option<u64>,
    pub owed_spans: usize,
    pub step_proofs_running: usize,
    pub rotate_running: bool,
}

impl RoundGauges {
    pub fn publish(&self) {
        gauge!(QUEUED_CHECKPOINTS).set(self.queued_checkpoints as f64);
        if let Some(slot) = self.last_finalized_slot {
            gauge!(LAST_FINALIZED_SLOT).set(slot as f64);
        }
        if let Some(period) = self.last_committee_period {
            gauge!(LAST_COMMITTEE_PERIOD).set(period as f64);
        }
        gauge!(OWED_SPANS).set(self.owed_spans as f64);
        gauge!(STEP_PROOFS_RUNNING).set(self.step_proofs_running as f64);
        gauge!(ROTATE_RUNNING).set(if self.rotate_running { 1.0 } else { 0.0 });
    }
}

fn ok_label(ok: bool) -> &'static str {
    if ok {
        "ok"
    } else {
        "error"
    }
}

fn proof_label(ok: bool) -> &'static str {
    if ok {
        "ok"
    } else {
        "failed"
    }
}

fn span_label(given_up: bool) -> &'static str {
    if given_up {
        "given_up"
    } else {
        "closed"
    }
}

/// The names a metrics module declares (its `pub const NAME: &str = "…"`
/// lines) and the names its table documents, each sorted: the two must match,
/// or a metric is exported that the operator cannot look up.
#[cfg(test)]
pub(crate) fn declared_and_documented(source: &str, prefix: &str) -> (Vec<String>, Vec<String>) {
    let quoted = |line: &str, open: &str, close: char| -> Option<String> {
        let rest = &line[line.find(open)? + open.len()..];
        let name = &rest[..rest.find(close)?];
        name.starts_with(prefix).then(|| name.to_string())
    };
    let mut declared = Vec::new();
    let mut documented = Vec::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("//! | `") {
            documented.extend(quoted(line, "//! | `", '`'));
        } else if line.starts_with("pub const ") && line.contains(": &str =") {
            // rustfmt puts a long name's value on the next line.
            let value = if line.contains('"') {
                line
            } else {
                lines.peek().copied().unwrap_or_default()
            };
            declared.extend(quoted(value, "\"", '"'));
        }
    }
    declared.sort();
    documented.sort();
    (declared, documented)
}

/// Reads what a test recorded into a `DebuggingRecorder`.
#[cfg(test)]
pub(crate) mod testing {
    use metrics_util::debugging::{DebugValue, Snapshotter};

    /// One series: its name, its labels and what it holds.
    struct Series {
        name: String,
        labels: Vec<(String, String)>,
        value: DebugValue,
    }

    /// Everything recorded up to now. Taking it empties the recorder, so a
    /// test takes one and reads all it checks from it.
    pub(crate) struct Recorded(Vec<Series>);

    impl Recorded {
        pub(crate) fn take(snapshotter: &Snapshotter) -> Self {
            Self(
                snapshotter
                    .snapshot()
                    .into_vec()
                    .into_iter()
                    .map(|(key, _, _, value)| {
                        let key = key.key();
                        Series {
                            name: key.name().to_string(),
                            labels: key
                                .labels()
                                .map(|l| (l.key().to_string(), l.value().to_string()))
                                .collect(),
                            value,
                        }
                    })
                    .collect(),
            )
        }

        /// The series of `name` that carry every label in `labels`, added
        /// up: a counter's count, a gauge's value, the number of samples a
        /// histogram took. `None` when nothing recorded one.
        pub(crate) fn get(&self, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
            let mut sum = None;
            for series in &self.0 {
                let wanted = labels
                    .iter()
                    .all(|(k, v)| series.labels.iter().any(|(ck, cv)| ck == k && cv == v));
                if series.name != name || !wanted {
                    continue;
                }
                let n = match &series.value {
                    DebugValue::Counter(n) => *n as f64,
                    DebugValue::Gauge(g) => g.into_inner(),
                    DebugValue::Histogram(samples) => samples.len() as f64,
                };
                sum = Some(sum.unwrap_or(0.0) + n);
            }
            sum
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_metric_is_in_the_table() {
        let (declared, documented) =
            declared_and_documented(include_str!("metrics.rs"), "eth_lc_relayer_");
        assert_eq!(declared, documented);
        assert_eq!(declared.len(), 19);
    }

    #[test]
    fn a_message_is_labelled_by_what_came_back() {
        let labels: Vec<&str> = [
            Ok(SubmitOutcome::Accepted {
                tx_hash: None,
            }),
            Ok(SubmitOutcome::AlreadyOnHead),
            Ok(SubmitOutcome::Rejected {
                reason: "exit_code=244".into(),
            }),
            Ok(SubmitOutcome::Pending {
                reason: "no answer yet".into(),
            }),
            Ok(SubmitOutcome::NotSent {
                reason: "timeout".into(),
            }),
            Err(RelayerError::other("no endpoint")),
        ]
        .iter()
        .map(submit_outcome_label)
        .collect();
        assert_eq!(labels, SUBMIT_OUTCOMES);
    }

    /// `init` puts every series the daemon can ever count in `/metrics` at
    /// zero, so the first failure of a kind is an increase from zero.
    #[test]
    fn every_series_is_there_at_zero_from_the_start() {
        let recorder = metrics_util::debugging::DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, init);
        let m = testing::Recorded::take(&snapshotter);

        assert_eq!(
            m.get(BUILD_INFO, &[("version", env!("CARGO_PKG_VERSION"))]),
            Some(1.0)
        );
        assert!(m.get(START_TIMESTAMP_SECONDS, &[]) > Some(0.0));
        for outcome in TICK_OUTCOMES {
            assert_eq!(
                m.get(TICKS_TOTAL, &[("outcome", outcome)]),
                Some(0.0),
                "{outcome}"
            );
        }
        for stage in CHECKPOINT_STAGES {
            assert_eq!(
                m.get(CHECKPOINT_FAILURES, &[("stage", stage)]),
                Some(0.0),
                "{stage}"
            );
        }
        for outcome in ["ok", "error"] {
            assert_eq!(
                m.get(BEACON_POLLS_TOTAL, &[("outcome", outcome)]),
                Some(0.0)
            );
        }
        for outcome in ["closed", "given_up"] {
            assert_eq!(
                m.get(ANCESTRY_SPANS_TOTAL, &[("outcome", outcome)]),
                Some(0.0)
            );
        }
        for outcome in ["ok", "failed"] {
            for name in [STEP_PROOF_DURATION_SECONDS, ROTATE_PROOF_DURATION_SECONDS] {
                assert_eq!(
                    m.get(name, &[("outcome", outcome)]),
                    Some(0.0),
                    "{name} {outcome}"
                );
            }
        }
        assert_eq!(m.get(MISSED_CHECKPOINTS_TOTAL, &[]), Some(0.0));
        for call in AnCall::ALL {
            for outcome in SUBMIT_OUTCOMES {
                assert_eq!(
                    m.get(AN_CALLS_TOTAL, &[
                        ("function", call.label()),
                        ("outcome", outcome)
                    ]),
                    Some(0.0),
                    "{call:?} {outcome}"
                );
            }
        }
    }
}
