//! Prometheus metrics of the deposit relayer (`metrics` facade).
//!
//! Every `metrics::*` macro in this crate records into whatever recorder the
//! binary installs; `deposit-relayer daemon --metrics-addr` installs the
//! Prometheus text exporter (`GET /metrics`). Without a recorder the macros
//! are no-ops, so the library and its tests carry no cost.
//!
//! Names are stable operator API (dashboards and alert rules key on them):
//!
//! | Metric | Type | Labels | Meaning |
//! |---|---|---|---|
//! | `deposit_relayer_ticks_total` | counter | `outcome` | One per `tick()`: `finalized`, `already_finalized`, `not_yet_available`, `proof_failed`, `an_rejected`, `an_pending`, `skipped`, `error` |
//! | `deposit_relayer_stage_duration_seconds` | histogram | `stage` | Wall time of `is_finalized`, `fetch_event`, `prove`, `submit`, and the whole `tick` |
//! | `deposit_relayer_prover_stage_duration_seconds` | histogram | `example` | Wall time of each `deposit-prover` subprocess (`fetch_deposit_data`, `export_vk_blob`, `export_blake2b_proof`) |
//! | `deposit_relayer_prover_stage_failures_total` | counter | `example` | Subprocess exits that were not 0 (spawn failure, timeout, non-zero status) |
//! | `deposit_relayer_eth_get_logs_total` | counter | `outcome` | `eth_getLogs` calls: `ok`, `retry` (retryable error, retried), `error` (given up) |
//! | `deposit_relayer_eth_scanned_blocks_total` | counter | | Blocks covered by `eth_getLogs` windows (the idle-scan cost) |
//! | `deposit_relayer_eth_safe_head_block` | gauge | | `head - confirmations` at the last scan |
//! | `deposit_relayer_eth_scan_from_block` | gauge | | First block of the last `eth_getLogs` scan (cursor + 1, or `--from-block`); a tick that finds no new deposit by `depositCounter()` makes no scan and leaves it |
//! | `deposit_relayer_eth_deposit_counter` | gauge | | `AckiNackiBridge.depositCounter()` on Ethereum, polled by the daemon; `counter - (last_finalized + 1)` is the backlog |
//! | `deposit_relayer_target_deposit_id` | gauge | | The `depositId` the loop is working on |
//! | `deposit_relayer_last_finalized_deposit_id` | gauge | | Highest `depositId` finalized (or found finalized) on AN in this process |
//! | `deposit_relayer_attempts_since_progress` | gauge | | Consecutive failed attempts at the current target (`state.json`); waiting for a deposit nobody made yet is not an attempt |
//! | `deposit_relayer_parked_deposits` | gauge | | Deposits parked by `--skip-after-attempts` that still need `finalize-one` |
//! | `deposit_relayer_backoff_seconds` | gauge | | Sleep the daemon applies after the last tick |
//! | `deposit_relayer_last_tick_timestamp_seconds` | gauge | | Unix time of the last completed tick |
//! | `deposit_relayer_last_finalized_timestamp_seconds` | gauge | | Unix time of the last `finalized` / `already_finalized` outcome |
//! | `deposit_relayer_scan_done_through_block` | gauge | | The log-scan cursor persisted in `state.json` (`scan_done_through_block`): no block up to it holds a deposit still to deliver |
//! | `deposit_relayer_build_info` | gauge | `version` | Always 1; the crate version |
//! | `deposit_relayer_start_timestamp_seconds` | gauge | | Unix time the daemon started |

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use metrics::{
    counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram, Unit,
};

pub const TICKS_TOTAL: &str = "deposit_relayer_ticks_total";
pub const STAGE_DURATION_SECONDS: &str = "deposit_relayer_stage_duration_seconds";
pub const PROVER_STAGE_DURATION_SECONDS: &str = "deposit_relayer_prover_stage_duration_seconds";
pub const PROVER_STAGE_FAILURES_TOTAL: &str = "deposit_relayer_prover_stage_failures_total";
pub const ETH_GET_LOGS_TOTAL: &str = "deposit_relayer_eth_get_logs_total";
pub const ETH_SCANNED_BLOCKS_TOTAL: &str = "deposit_relayer_eth_scanned_blocks_total";
pub const ETH_SAFE_HEAD_BLOCK: &str = "deposit_relayer_eth_safe_head_block";
pub const ETH_SCAN_FROM_BLOCK: &str = "deposit_relayer_eth_scan_from_block";
pub const ETH_DEPOSIT_COUNTER: &str = "deposit_relayer_eth_deposit_counter";
pub const TARGET_DEPOSIT_ID: &str = "deposit_relayer_target_deposit_id";
pub const LAST_FINALIZED_DEPOSIT_ID: &str = "deposit_relayer_last_finalized_deposit_id";
pub const ATTEMPTS_SINCE_PROGRESS: &str = "deposit_relayer_attempts_since_progress";
pub const PARKED_DEPOSITS: &str = "deposit_relayer_parked_deposits";
pub const BACKOFF_SECONDS: &str = "deposit_relayer_backoff_seconds";
pub const LAST_TICK_TIMESTAMP_SECONDS: &str = "deposit_relayer_last_tick_timestamp_seconds";
pub const LAST_FINALIZED_TIMESTAMP_SECONDS: &str =
    "deposit_relayer_last_finalized_timestamp_seconds";
pub const SCAN_DONE_THROUGH_BLOCK: &str = "deposit_relayer_scan_done_through_block";
pub const BUILD_INFO: &str = "deposit_relayer_build_info";
pub const START_TIMESTAMP_SECONDS: &str = "deposit_relayer_start_timestamp_seconds";

/// Register the help strings and units with the installed recorder. Call once
/// after the exporter is installed; harmless without one.
pub fn describe() {
    describe_counter!(TICKS_TOTAL, "Relayer ticks by outcome (label `outcome`).");
    describe_histogram!(
        STAGE_DURATION_SECONDS,
        Unit::Seconds,
        "Wall time of one tick stage (label `stage`: is_finalized, fetch_event, prove, submit, \
         tick)."
    );
    describe_histogram!(
        PROVER_STAGE_DURATION_SECONDS,
        Unit::Seconds,
        "Wall time of one deposit-prover subprocess (label `example`)."
    );
    describe_counter!(
        PROVER_STAGE_FAILURES_TOTAL,
        "deposit-prover subprocesses that did not exit 0 (label `example`)."
    );
    describe_counter!(
        ETH_GET_LOGS_TOTAL,
        "eth_getLogs calls by outcome (label `outcome`: ok, retry, error)."
    );
    describe_counter!(
        ETH_SCANNED_BLOCKS_TOTAL,
        "Ethereum blocks covered by eth_getLogs windows."
    );
    describe_gauge!(
        ETH_SAFE_HEAD_BLOCK,
        "Ethereum head minus confirmations at the last scan."
    );
    describe_gauge!(
        ETH_SCAN_FROM_BLOCK,
        "First block of the last eth_getLogs scan."
    );
    describe_gauge!(
        ETH_DEPOSIT_COUNTER,
        "AckiNackiBridge.depositCounter() on Ethereum, as last polled."
    );
    describe_gauge!(TARGET_DEPOSIT_ID, "depositId the loop is working on.");
    describe_gauge!(
        LAST_FINALIZED_DEPOSIT_ID,
        "Highest depositId finalized on AN by this process."
    );
    describe_gauge!(
        ATTEMPTS_SINCE_PROGRESS,
        "Consecutive non-success ticks on the current target."
    );
    describe_gauge!(
        PARKED_DEPOSITS,
        "Deposits parked by --skip-after-attempts that still need finalize-one."
    );
    describe_gauge!(
        BACKOFF_SECONDS,
        Unit::Seconds,
        "Sleep applied after the last tick."
    );
    describe_gauge!(
        LAST_TICK_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time of the last completed tick."
    );
    describe_gauge!(
        LAST_FINALIZED_TIMESTAMP_SECONDS,
        Unit::Seconds,
        "Unix time of the last finalized or already_finalized outcome."
    );
    describe_gauge!(
        SCAN_DONE_THROUGH_BLOCK,
        "Log-scan cursor persisted in state.json (scan_done_through_block)."
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

/// Record one `deposit_relayer_stage_duration_seconds` sample.
pub fn record_stage(stage: &'static str, elapsed: Duration) {
    histogram!(STAGE_DURATION_SECONDS, "stage" => stage).record(elapsed.as_secs_f64());
}

/// Count one tick outcome.
pub fn record_tick_outcome(outcome: &'static str) {
    counter!(TICKS_TOTAL, "outcome" => outcome).increment(1);
}

/// Set the gauges that mirror `state.json` and the current target.
pub fn set_state_gauges(
    target: u64,
    attempts_since_progress: u32,
    parked: usize,
    scan_done_through_block: Option<u64>,
) {
    gauge!(TARGET_DEPOSIT_ID).set(target as f64);
    gauge!(ATTEMPTS_SINCE_PROGRESS).set(f64::from(attempts_since_progress));
    gauge!(PARKED_DEPOSITS).set(parked as f64);
    if let Some(block) = scan_done_through_block {
        gauge!(SCAN_DONE_THROUGH_BLOCK).set(block as f64);
    }
}

/// Stopwatch for one stage; `finish` records the sample.
pub struct StageTimer {
    stage: &'static str,
    started: Instant,
}

impl StageTimer {
    pub fn start(stage: &'static str) -> Self {
        Self {
            stage,
            started: Instant::now(),
        }
    }

    pub fn finish(self) {
        record_stage(self.stage, self.started.elapsed());
    }
}

/// Stamp the static process gauges once at daemon start.
pub fn set_build_info() {
    gauge!(BUILD_INFO, "version" => env!("CARGO_PKG_VERSION")).set(1.0);
    gauge!(START_TIMESTAMP_SECONDS).set(unix_now());
}
