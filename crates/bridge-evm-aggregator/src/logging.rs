//! How `aggregate-proof` logs: to stderr, which its callers forward into
//! their own logs.

use tracing_subscriber::EnvFilter;

/// The filter without `RUST_LOG`: this crate's and the tool's own lines at
/// `info` — the pk cache's notices (waiting for another keygen, a filesystem
/// without locks, leftovers removed, a slot loaded or generated) are for the
/// operator — and everyone else's warnings.
pub const DEFAULT_FILTER: &str = "warn,bridge_evm_aggregator=info,aggregate_proof=info";

/// `RUST_LOG`, or [`DEFAULT_FILTER`] without it.
pub fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caches_notices_pass_the_default_filter_and_dependencies_chatter_does_not() {
        #[derive(Clone, Default)]
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new(DEFAULT_FILTER))
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "bridge_evm_aggregator::cache", "a cache notice");
            tracing::info!(target: "aggregate_proof", "the tool's own");
            tracing::info!(target: "snark_verifier_sdk", "a dependency's chatter");
            tracing::warn!(target: "snark_verifier_sdk", "a dependency's warning");
        });
        let log = String::from_utf8_lossy(&sink.0.lock().unwrap()).into_owned();
        assert!(
            log.contains("a cache notice") && log.contains("the tool's own"),
            "{log}"
        );
        assert!(
            !log.contains("chatter") && log.contains("a dependency's warning"),
            "{log}"
        );
    }
}
