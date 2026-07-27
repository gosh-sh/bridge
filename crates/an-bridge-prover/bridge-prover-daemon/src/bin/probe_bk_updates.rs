//! `probe_bk_updates` — one-off cadence study for the AN `bkSetUpdates`
//! stream. Called out in `docs/bk_set_update_no_circuit3_plan.md` §Phase 2.
//!
//! Purpose: sample a live chain's rotation cadence so we can size the
//! prover's drain loop (one-at-a-time vs batched) and estimate the daemon's
//! per-day rotation-proof budget. Read-only, no state file changes, no
//! circuit / IPC work.
//!
//! Usage:
//!
//! ```sh
//! BRIDGE_GQL_ENDPOINT=https://shellnet.ackinacki.org/graphql \
//!     cargo run --release -p bridge-prover-daemon --bin probe_bk_updates
//!
//! # or with flags:
//! cargo run --release -p bridge-prover-daemon --bin probe_bk_updates -- \
//!     --gql https://shellnet.ackinacki.org/graphql \
//!     --last 500 \
//!     --gap-threshold 100 \
//!     --json
//! ```
//!
//! Behavior:
//! - Walks the full `bkSetUpdates` stream in ascending order via the same
//!   500-event-page pagination the prover uses.
//! - With `--last N`, keeps only the last N events after the full walk.
//!   (Server-side `last: N` is *not* used — it doesn't tell us the seq_no
//!   of the earliest event, which we want for a full-history summary.)
//! - Prints one line per event with `(height, delta_from_prev, block_id)`.
//! - Prints a summary block: count, first/last height, gaps (min/median/p90/max),
//!   bursts (consecutive events with delta <= `--gap-threshold`).
//! - With `--json`, prints only a single JSON object with the summary +
//!   the per-event array (script-friendly).
//!
//! Deliberately keeps zero coupling to `bridge-prover-lib` — this is an
//! operational tool, not a code path the daemon depends on.

use std::collections::VecDeque;

use anyhow::{bail, Context};
use bridge_gql_fetcher::gql_client::{create_client, BkSetUpdateWithAttestations};
use serde::Serialize;

const PAGE_SIZE: u32 = 500;
/// Hard ceiling on pagination — same shape as `bk_set_at_height`.
const MAX_PAGES: usize = 10_000;

#[derive(Debug, Serialize)]
struct EventRow {
    height: Option<u64>,
    delta_from_prev: Option<u64>,
    block_id: String,
}

#[derive(Debug, Serialize)]
struct Summary {
    endpoint: String,
    total_events_walked: usize,
    events_reported: usize,
    first_height: Option<u64>,
    last_height: Option<u64>,
    gap_min: Option<u64>,
    gap_median: Option<u64>,
    gap_p90: Option<u64>,
    gap_max: Option<u64>,
    gap_mean: Option<f64>,
    bursts_count: usize,
    burst_threshold: u64,
    largest_burst_len: usize,
    events_with_missing_height: usize,
}

#[derive(Debug, Serialize)]
struct ProbeOutput {
    summary: Summary,
    events: Vec<EventRow>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut endpoint: Option<String> = None;
    let mut last_n: Option<usize> = None;
    let mut gap_threshold: u64 = 100;
    let mut json_out = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--gql" => endpoint = Some(args.next().context("--gql needs a URL")?),
            "--last" => {
                last_n = Some(
                    args.next()
                        .context("--last needs a value")?
                        .parse()
                        .context("--last must be usize")?,
                );
            }
            "--gap-threshold" => {
                gap_threshold = args
                    .next()
                    .context("--gap-threshold needs a value")?
                    .parse()
                    .context("--gap-threshold must be u64")?;
            }
            "--json" => json_out = true,
            "--help" | "-h" => {
                eprintln!(
                    "usage: probe_bk_updates [--gql URL] [--last N] \
                     [--gap-threshold SEQNO] [--json]\n\
                    \n\
                    Reads BRIDGE_GQL_ENDPOINT if --gql omitted.\n\
                    Walks full bkSetUpdates history in ascending order; \
                    --last N truncates to the last N after walking.\n\
                    Bursts = maximal runs of events whose gap to the \
                    previous event is <= --gap-threshold seq_no."
                );
                return Ok(());
            }
            other => bail!("unknown arg: {other}"),
        }
    }

    let endpoint = endpoint
        .or_else(|| std::env::var("BRIDGE_GQL_ENDPOINT").ok())
        .context("no endpoint — pass --gql or set BRIDGE_GQL_ENDPOINT")?;
    let client = create_client(&endpoint).context("create_client failed")?;

    // Paginated ascending walk. Server returns edges in ascending chain_order
    // (which coincides with ascending height on healthy chains); we sanity-check
    // that as we go and warn (not fail) on regressions so the probe stays useful
    // when the chain is misbehaving.
    let mut cursor: Option<String> = None;
    let mut all: Vec<BkSetUpdateWithAttestations> = Vec::new();
    let mut pages = 0usize;
    loop {
        let (page, next) = client
            .query_bk_set_updates_paged(u64::MAX, PAGE_SIZE, cursor.as_deref())
            .await
            .with_context(|| format!("page {} (cursor={:?})", pages, cursor))?;
        pages += 1;
        if !json_out {
            eprintln!(
                "page {}: {} events (cursor -> {:?})",
                pages,
                page.len(),
                next
            );
        }
        all.extend(page);
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
        if pages >= MAX_PAGES {
            bail!(
                "refusing to paginate past {} pages — probable server bug",
                MAX_PAGES
            );
        }
    }

    // Order check (warn only). This is a cadence probe, not a fetcher —
    // print a diagnostic on unordered results and continue.
    let missing_height = all.iter().filter(|u| u.height.is_none()).count();
    let mut ordering_warnings = 0usize;
    let mut hwm: u64 = 0;
    for u in &all {
        if let Some(h) = u.height {
            if h < hwm {
                ordering_warnings += 1;
            }
            hwm = hwm.max(h);
        }
    }
    if ordering_warnings > 0 && !json_out {
        eprintln!(
            "WARN: {} events out of ascending order in server response",
            ordering_warnings
        );
    }

    // Truncate to the last N if requested (post-walk, so first_height reflects
    // full-history context in stderr but summary reports the reported window).
    let events_walked = all.len();
    if let Some(n) = last_n {
        if all.len() > n {
            all.drain(0..all.len() - n);
        }
    }

    // Build per-event rows with per-step delta.
    let mut rows: Vec<EventRow> = Vec::with_capacity(all.len());
    let mut prev: Option<u64> = None;
    for u in &all {
        let delta = match (prev, u.height) {
            (Some(p), Some(h)) if h >= p => Some(h - p),
            _ => None,
        };
        rows.push(EventRow {
            height: u.height,
            delta_from_prev: delta,
            block_id: u.block_id.clone(),
        });
        if let Some(h) = u.height {
            prev = Some(h);
        }
    }

    // Gap statistics + burst detection.
    let gaps: Vec<u64> = rows.iter().filter_map(|r| r.delta_from_prev).collect();
    let (gap_min, gap_median, gap_p90, gap_max, gap_mean) = if gaps.is_empty() {
        (None, None, None, None, None)
    } else {
        let mut sorted = gaps.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let median = sorted[n / 2];
        let p90 = sorted[(n * 9) / 10];
        let sum: u64 = sorted.iter().sum();
        (
            Some(sorted[0]),
            Some(median),
            Some(p90),
            Some(sorted[n - 1]),
            Some(sum as f64 / n as f64),
        )
    };

    // Burst = maximal run of consecutive events whose gap-from-prev <= threshold.
    // (i.e. the arrivals cluster.) Run of length 1 = isolated event, not a burst.
    let mut bursts: Vec<usize> = Vec::new();
    let mut run: VecDeque<()> = VecDeque::new();
    for r in &rows {
        if r.delta_from_prev.map(|d| d <= gap_threshold).unwrap_or(false) {
            run.push_back(());
        } else {
            if run.len() >= 1 {
                // +1 because the first event of the burst is the "prev" of the
                // second, which is the one whose delta triggered.
                bursts.push(run.len() + 1);
            }
            run.clear();
        }
    }
    if run.len() >= 1 {
        bursts.push(run.len() + 1);
    }
    // Drop isolated pairs iff you want "true" bursts — but at threshold=100 seq_no,
    // even a 2-event cluster is worth flagging. Keep len>=2.
    let bursts: Vec<usize> = bursts.into_iter().filter(|&l| l >= 2).collect();
    let largest_burst_len = bursts.iter().copied().max().unwrap_or(0);

    let summary = Summary {
        endpoint: endpoint.clone(),
        total_events_walked: events_walked,
        events_reported: rows.len(),
        first_height: rows.first().and_then(|r| r.height),
        last_height: rows.last().and_then(|r| r.height),
        gap_min,
        gap_median,
        gap_p90,
        gap_max,
        gap_mean,
        bursts_count: bursts.len(),
        burst_threshold: gap_threshold,
        largest_burst_len,
        events_with_missing_height: missing_height,
    };

    if json_out {
        let out = ProbeOutput { summary, events: rows };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("=== bkSetUpdates cadence probe ===");
        println!("endpoint         : {}", summary.endpoint);
        println!("total walked     : {}", summary.total_events_walked);
        println!("events reported  : {}", summary.events_reported);
        println!(
            "first height     : {}",
            summary
                .first_height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "n/a".into())
        );
        println!(
            "last  height     : {}",
            summary
                .last_height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "n/a".into())
        );
        println!("missing height   : {}", summary.events_with_missing_height);
        println!("--- gap seqno stats ---");
        println!(
            "min / median / p90 / max : {} / {} / {} / {}",
            summary.gap_min.map(|g| g.to_string()).unwrap_or_else(|| "-".into()),
            summary
                .gap_median
                .map(|g| g.to_string())
                .unwrap_or_else(|| "-".into()),
            summary.gap_p90.map(|g| g.to_string()).unwrap_or_else(|| "-".into()),
            summary.gap_max.map(|g| g.to_string()).unwrap_or_else(|| "-".into()),
        );
        println!(
            "mean             : {}",
            summary
                .gap_mean
                .map(|g| format!("{:.1}", g))
                .unwrap_or_else(|| "-".into())
        );
        println!(
            "bursts (gap<={} seqno): {} clusters, largest={}",
            summary.burst_threshold, summary.bursts_count, summary.largest_burst_len,
        );
        println!("--- events ---");
        for r in &rows {
            println!(
                "  height={:>10}  delta={:>10}  block_id={}",
                r.height
                    .map(|h| h.to_string())
                    .unwrap_or_else(|| "n/a".into()),
                r.delta_from_prev
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "n/a".into()),
                r.block_id,
            );
        }
    }
    Ok(())
}
