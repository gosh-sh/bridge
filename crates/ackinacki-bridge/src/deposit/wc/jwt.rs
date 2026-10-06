//! Relay authentication: a short-lived EdDSA JWT whose issuer is the
//! client's ed25519 key as a `did:key`.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};

/// The `did:key` form of an ed25519 public key: multicodec `0xed 0x01`
/// followed by the key, base58btc-encoded with the `z` multibase prefix.
pub fn did_key(public: &[u8; 32]) -> String {
    let mut v = vec![0xed, 0x01];
    v.extend_from_slice(public);
    format!("did:key:z{}", bs58::encode(v).into_string())
}

/// The JWT header.
#[derive(serde::Serialize)]
struct Header {
    alg: &'static str,
    typ: &'static str,
}

/// The JWT claims, serialized in this field order.
#[derive(serde::Serialize)]
struct Claims<'a> {
    iss: String,
    sub: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

/// Builds the relay authorization JWT, valid for `ttl_s` seconds from `iat`.
pub fn relay_jwt(sk: &SigningKey, sub_hex: &str, aud: &str, iat: u64, ttl_s: u64) -> String {
    let h = B64U.encode(
        serde_json::to_vec(&Header {
            alg: "EdDSA",
            typ: "JWT",
        })
        .expect("static header"),
    );
    let c = Claims {
        iss: did_key(&sk.verifying_key().to_bytes()),
        sub: sub_hex,
        aud,
        iat,
        exp: iat + ttl_s,
    };
    let p = B64U.encode(serde_json::to_vec(&c).expect("plain claims"));
    let signing_input = format!("{h}.{p}");
    let sig = sk.sign(signing_input.as_bytes());
    format!("{signing_input}.{}", B64U.encode(sig.to_bytes()))
}

/// The relay WebSocket URL carrying the JWT, the project id and the user agent.
pub fn relay_url(base: &str, project_id: &str, auth: &str) -> String {
    let ua = format!(
        "wc-2/rust-ackinacki-bridge-{}/cli",
        env!("CARGO_PKG_VERSION")
    );
    format!(
        "{}/?auth={auth}&projectId={project_id}&ua={}",
        base.trim_end_matches('/'),
        ua.replace('/', "%2F")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_and_jwt_match_an_independent_implementation() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        assert_eq!(
            hex::encode(sk.verifying_key().to_bytes()),
            "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12"
        );
        assert_eq!(
            did_key(&sk.verifying_key().to_bytes()),
            "did:key:z6MkghLt1e8m1fmANsdJJco3aCLV8Xnigr5UWwC3u5iZFPd3"
        );
        assert_eq!(
            relay_jwt(&sk, &"00".repeat(32), "wss://relay.walletconnect.org", 1_700_000_000, 86_400),
            "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJkaWQ6a2V5Ono2TWtnaEx0MWU4bTFmbUFOc2RKSmNvM2FDTFY4WG5pZ3I1VVd3QzN1NWlaRlBkMyIsInN1YiI6IjAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAiLCJhdWQiOiJ3c3M6Ly9yZWxheS53YWxsZXRjb25uZWN0Lm9yZyIsImlhdCI6MTcwMDAwMDAwMCwiZXhwIjoxNzAwMDg2NDAwfQ.2qGfGHgkki-_SWPIjl2t9tGmEkbtaZwPj0NFkVo7vwIijfgaUH8ZkiN72XiiSMN5gnR_XKopnEeCq0g-pIZeBg"
        );
    }

    #[test]
    fn the_url_carries_auth_and_project() {
        let u = relay_url("wss://relay.walletconnect.org", "p1", "j.w.t");
        assert!(u.starts_with("wss://relay.walletconnect.org/?"));
        assert!(u.contains("auth=j.w.t") && u.contains("projectId=p1") && u.contains("ua="));
    }
}
