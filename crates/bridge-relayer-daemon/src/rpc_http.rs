//! HTTP client for the Ethereum JSON-RPC endpoint.
//!
//! alloy's `ProviderBuilder::connect_http` builds a reqwest client without a
//! timeout. A request on a connection that has gone silent then waits until
//! the OS gives up on the connection, which can take many minutes, and a
//! daemon tick stuck on it does nothing else meanwhile. A request through the
//! client built here fails after a bound instead: the tick fails, and the
//! daemon retries after its backoff. A submit that fails this way is an
//! unconfirmed one ([`crate::is_unconfirmed_submit_failure`]), so it does not
//! count toward the abort threshold.

use std::time::Duration;

use alloy::transports::http::reqwest;

/// Overrides [`DEFAULT_RPC_REQUEST_TIMEOUT`], in whole seconds.
pub const ENV_RPC_REQUEST_TIMEOUT_SECS: &str = "BRIDGE_RPC_REQUEST_TIMEOUT_SECS";

/// Bound on one JSON-RPC request, from connecting to the last byte of the
/// response. The relayer's slowest call, one `eth_getLogs` span of the
/// startup scan, takes seconds.
pub const DEFAULT_RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// [`ENV_RPC_REQUEST_TIMEOUT_SECS`] when it is set and non-empty, else
/// [`DEFAULT_RPC_REQUEST_TIMEOUT`].
pub fn rpc_request_timeout_from_env() -> anyhow::Result<Duration> {
    parse_rpc_request_timeout(std::env::var(ENV_RPC_REQUEST_TIMEOUT_SECS).ok().as_deref())
}

/// The env value as a timeout. `0` is refused: every request would fail.
fn parse_rpc_request_timeout(raw: Option<&str>) -> anyhow::Result<Duration> {
    let raw = match raw.map(str::trim) {
        Some(raw) if !raw.is_empty() => raw,
        _ => return Ok(DEFAULT_RPC_REQUEST_TIMEOUT),
    };
    let secs: u64 = raw
        .parse()
        .map_err(|e| anyhow::anyhow!("{ENV_RPC_REQUEST_TIMEOUT_SECS}={raw:?}: {e}"))?;
    anyhow::ensure!(secs > 0, "{ENV_RPC_REQUEST_TIMEOUT_SECS} must be > 0");
    Ok(Duration::from_secs(secs))
}

/// HTTP client whose every request fails after `timeout`. Hand it to
/// `ProviderBuilder::connect_reqwest` where `connect_http` would build one
/// without a bound.
pub fn rpc_http_client(timeout: Duration) -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| anyhow::anyhow!("Ethereum RPC HTTP client: {e}"))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use alloy::{
        providers::{Provider, ProviderBuilder},
        transports::{RpcError, TransportErrorKind},
    };
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn timeout_is_the_default_unless_set_to_whole_seconds_above_zero() {
        for unset in [None, Some(""), Some("  ")] {
            assert_eq!(
                parse_rpc_request_timeout(unset).unwrap(),
                DEFAULT_RPC_REQUEST_TIMEOUT
            );
        }
        assert_eq!(
            parse_rpc_request_timeout(Some(" 15 ")).unwrap(),
            Duration::from_secs(15)
        );
        for bad in ["0", "1.5", "60s", "-1"] {
            assert!(parse_rpc_request_timeout(Some(bad)).is_err(), "{bad}");
        }
    }

    /// The case the bound exists for: the server takes the connection and
    /// never answers. Without the bound the call below would wait forever.
    #[tokio::test]
    async fn a_request_the_server_never_answers_fails_after_the_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut silent = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                silent.push(socket);
            }
        });
        let provider = ProviderBuilder::new().connect_reqwest(
            rpc_http_client(Duration::from_millis(200)).unwrap(),
            url.parse().unwrap(),
        );

        let started = Instant::now();
        let err = tokio::time::timeout(Duration::from_secs(10), provider.get_block_number())
            .await
            .expect("the request outlived its own timeout")
            .expect_err("a server that never answers cannot return a block number");

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let RpcError::Transport(TransportErrorKind::Custom(source)) = &err else {
            panic!("expected a transport error, got {err:?}");
        };
        let http = source
            .downcast_ref::<reqwest::Error>()
            .unwrap_or_else(|| panic!("expected a reqwest error, got {source:?}"));
        assert!(http.is_timeout(), "{http:?}");
    }
}
