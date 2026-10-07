//! Capturing what a test logs, whatever runs beside it.
//!
//! `tracing` asks once per callsite whether any subscriber wants it, and
//! caches the answer. While a single dispatcher is registered in the
//! process, the answer comes from the default subscriber of the thread that
//! reaches the callsite first. A test that installed a subscriber for its
//! own thread is then often that single dispatcher, and when a test beside
//! it, with no subscriber, reaches a shared `warn!` first, the callsite
//! caches "never": the capturing test reads back nothing. With a second
//! dispatcher registered for the life of the process, the answer covers
//! every live subscriber, and each event asks the one of its thread.

use std::sync::OnceLock;

/// The dispatcher that keeps every test from being the only one.
static SECOND: OnceLock<tracing::Dispatch> = OnceLock::new();

/// [`tracing::subscriber::with_default`] for a test that reads back what
/// `f` logged.
pub fn with_default<S, R>(subscriber: S, f: impl FnOnce() -> R) -> R
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    SECOND.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    tracing::subscriber::with_default(subscriber, || {
        // Registering `subscriber` asked every callsite again, but a thread
        // that was answering one at that moment for itself alone may have
        // stored its answer after that: ask once more.
        tracing::callsite::rebuild_interest_cache();
        f()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    /// Everything `subscriber` writes, and the subscriber.
    fn capturing() -> (Arc<Mutex<Vec<u8>>>, impl tracing::Subscriber + Send + Sync) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let out = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || Sink(out.clone()))
            .with_ansi(false)
            .finish();
        (buf, subscriber)
    }

    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_line_is_captured_although_a_thread_without_a_subscriber_reached_it_first() {
        // A callsite of its own, first reached here: by a thread that has no
        // subscriber, while the capturing one is installed.
        fn emit() {
            tracing::warn!("seen by the capture");
        }
        let (buf, subscriber) = capturing();
        super::with_default(subscriber, || {
            std::thread::spawn(emit).join().unwrap();
            emit();
        });
        let said = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(said.contains("seen by the capture"), "{said:?}");
    }
}
