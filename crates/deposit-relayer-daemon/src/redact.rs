//! URLs cut down to `scheme://host` wherever text leaves the process: RPC
//! URLs carry API keys, and transport errors, tool output and the daemon's
//! own log all quote them.

use std::io;

/// Every URL in `text` cut down to `scheme://host` (the port kept, the
/// userinfo, path, query and fragment dropped).
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let after = &rest[i + 3..];
        let url_end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>' | ','))
            .unwrap_or(after.len());
        let url = &after[..url_end];
        let host_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let host = url[..host_end].rsplit('@').next().unwrap_or("");
        out.push_str(&rest[..i]);
        out.push_str("://");
        out.push_str(host);
        rest = &after[url_end..];
    }
    out.push_str(rest);
    out
}

/// A `tracing` writer that redacts every URL in what it writes: the log sink
/// is the one place every message, field and error text passes through.
pub struct Redacting<M>(pub M);

pub struct RedactingWriter<W>(W);

impl<'a, M> tracing_subscriber::fmt::MakeWriter<'a> for Redacting<M>
where
    M: tracing_subscriber::fmt::MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter(self.0.make_writer())
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        RedactingWriter(self.0.make_writer_for(meta))
    }
}

impl<W: io::Write> io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // The fmt layer hands over one formatted event per call.
        self.0
            .write_all(redact_urls(&String::from_utf8_lossy(buf)).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_cut_to_their_host() {
        let t = "$ fetch --rpc-url https://eth.example/v2/SECRET?x=1 --tx 0xab\n  RPC URL: http://u:p@h.local:8545/path\nerror sending request for url (https://rpc.example/KEY): timeout\nv6 http://[::1]:8545/KEY end\n";
        assert_eq!(
            redact_urls(t),
            "$ fetch --rpc-url https://eth.example --tx 0xab\n  RPC URL: http://h.local:8545\nerror sending request for url (https://rpc.example): timeout\nv6 http://[::1]:8545 end\n"
        );
    }

    #[test]
    fn the_log_sink_redacts_what_it_writes() {
        use std::io::Write;
        let mut sink = Vec::new();
        {
            let mut w = RedactingWriter(&mut sink);
            w.write_all(b"call failed: https://rpc.example/v2/KEY (timeout)\n")
                .unwrap();
        }
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            "call failed: https://rpc.example (timeout)\n"
        );
    }
}
