//! Transient failures — an RPC timeout, a 429, a GraphQL hiccup, a relay
//! reconnect — are retried forever: backoff 1 s doubling to 60 s, ±20 %
//! jitter, every attempt reported. A read that failed is "unknown", never
//! "no", so nothing here turns an error into a decision.

use std::{future::Future, time::Duration};

use rand::Rng as _;

use crate::deposit::ui::Ui;

/// The pause before retry number `attempt` (counted from 0): 1, 2, 4, …, 32
/// seconds, then 60 seconds for every later attempt.
pub fn base_delay(attempt: u32) -> Duration {
    Duration::from_secs(if attempt >= 6 { 60 } else { 1u64 << attempt })
}

/// `base` scaled by `1 + 0.2 * unit`; `unit` is clamped to `[-1, 1]`, so the
/// result is within ±20 % of `base`.
pub fn jittered(base: Duration, unit: f64) -> Duration {
    base.mul_f64(1.0 + 0.2 * unit.clamp(-1.0, 1.0))
}

/// Retries `f` until it succeeds or `deadline` passes. The deadline covers
/// the calls themselves and the pauses between them. `None` means the read
/// never succeeded and there is no time left to wait.
pub async fn until<T, F, Fut>(
    ui: &dyn Ui,
    what: &str,
    deadline: Option<tokio::time::Instant>,
    mut f: F,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut attempt = 0u32;
    loop {
        let r = match deadline {
            Some(d) => tokio::time::timeout_at(d, f()).await.ok()?,
            None => f().await,
        };
        match r {
            Ok(v) => return Some(v),
            Err(e) => {
                attempt += 1;
                ui.retry(what, attempt, &format!("{e:#}"));
                let unit: f64 = rand::thread_rng().gen_range(-1.0..=1.0);
                let pause = jittered(base_delay(attempt - 1), unit);
                if deadline.is_some_and(|d| tokio::time::Instant::now() + pause >= d) {
                    return None;
                }
                tokio::time::sleep(pause).await;
            },
        }
    }
}

/// The longest a single optional read may take.
pub const ONE_READ: Duration = Duration::from_secs(60);

/// One attempt, no longer than `deadline` or [`ONE_READ`]. An error and a
/// timeout both give `None`: "not known", never "no".
pub async fn once<T, Fut>(deadline: Option<tokio::time::Instant>, fut: Fut) -> Option<T>
where
    Fut: Future<Output = anyhow::Result<T>>,
{
    let cap = tokio::time::Instant::now() + ONE_READ;
    let end = deadline.map_or(cap, |d| d.min(cap));
    tokio::time::timeout_at(end, fut)
        .await
        .ok()
        .and_then(Result::ok)
}

/// Retries `f` forever, reporting every failed attempt through `ui`.
pub async fn transient<T, F, Fut>(ui: &dyn Ui, what: &str, f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    match until(ui, what, None, f).await {
        Some(v) => v,
        None => unreachable!("no deadline, so `until` only returns a value"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::deposit::ui::{RecordingUi, UiEvent};

    #[test]
    fn delays_double_up_to_a_minute() {
        let got: Vec<u64> = (0..9).map(|a| base_delay(a).as_secs()).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let b = Duration::from_secs(10);
        assert_eq!(jittered(b, 1.0), Duration::from_secs(12));
        assert_eq!(jittered(b, -1.0), Duration::from_secs(8));
        assert_eq!(jittered(b, 0.0), b);
    }

    #[tokio::test(start_paused = true)]
    async fn until_gives_up_at_the_deadline_and_not_before() {
        let ui = RecordingUi::new(true);
        let t0 = tokio::time::Instant::now();
        let got: Option<()> = until(
            &ui,
            "reading",
            Some(t0 + Duration::from_secs(300)),
            || async { anyhow::bail!("503") },
        )
        .await;
        assert!(got.is_none());
        let waited = t0.elapsed();
        assert!(
            waited <= Duration::from_secs(300) && waited >= Duration::from_secs(200),
            "{waited:?}"
        );
        // A call that hangs is cut at the deadline too.
        let hang: Option<()> = until(
            &ui,
            "reading",
            Some(tokio::time::Instant::now() + Duration::from_secs(5)),
            || std::future::pending(),
        )
        .await;
        assert!(hang.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn once_is_one_attempt_cut_at_the_deadline_or_a_minute() {
        assert_eq!(once(None, async { Ok(7) }).await, Some(7));
        assert_eq!(
            once::<u8, _>(None, async { anyhow::bail!("503") }).await,
            None
        );
        let t0 = tokio::time::Instant::now();
        assert_eq!(once::<u8, _>(None, std::future::pending()).await, None);
        assert_eq!(
            t0.elapsed(),
            ONE_READ,
            "a hung read without a deadline still ends"
        );
        let t0 = tokio::time::Instant::now();
        assert_eq!(
            once::<u8, _>(Some(t0 + Duration::from_secs(5)), std::future::pending()).await,
            None
        );
        assert_eq!(t0.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn every_failed_attempt_is_reported_and_the_value_comes_back() {
        let ui = RecordingUi::new(true);
        let n = AtomicU32::new(0);
        let v = transient(&ui, "reading", || async {
            if n.fetch_add(1, Ordering::SeqCst) < 3 {
                anyhow::bail!("429 Too Many Requests")
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(v, 7);
        let retries = ui
            .events()
            .into_iter()
            .filter(|e| matches!(e, UiEvent::Retry(..)))
            .count();
        assert_eq!(retries, 3);
    }
}
