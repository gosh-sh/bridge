//! WalletConnect v2 envelope crypto: X25519 key agreement, HKDF-SHA256
//! to a symmetric key, ChaCha20-Poly1305 with a 12-byte nonce. A topic
//! is sha256 of its symmetric key. Type 0 envelopes carry nonce and
//! ciphertext; type 1 also carries the sender's public key.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit, Nonce};
use hkdf::Hkdf;
use rand::RngCore as _;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

/// An X25519 key pair.
pub struct KeyPair {
    /// The private half.
    pub secret: StaticSecret,
    /// The public half, as sent on the wire.
    pub public: [u8; 32],
}

impl KeyPair {
    /// The pair for a given secret.
    pub fn from_secret(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret).to_bytes();
        KeyPair {
            secret,
            public,
        }
    }

    /// A fresh random pair.
    pub fn generate() -> Self {
        Self::from_secret(random_bytes())
    }
}

/// `N` bytes from the thread-local CSPRNG.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::thread_rng().fill_bytes(&mut out);
    out
}

/// X25519 agreement, then HKDF-SHA256 with no salt and no info, 32 bytes.
pub fn derive_sym_key(own: &StaticSecret, peer_public: &[u8; 32]) -> [u8; 32] {
    let shared = own.diffie_hellman(&PublicKey::from(*peer_public));
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(None, shared.as_bytes())
        .expand(&[], &mut out)
        .expect("32 bytes is a valid HKDF output length");
    out
}

/// The relay topic of a symmetric key: hex of its sha256.
pub fn topic_of(sym: &[u8; 32]) -> String {
    hex::encode(Sha256::digest(sym))
}

/// ChaCha20-Poly1305 encryption of `plaintext` under `sym` and `iv`.
fn seal(sym: &[u8; 32], iv: [u8; 12], plaintext: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(sym.into())
        .encrypt(Nonce::from_slice(&iv), plaintext)
        .expect("ChaCha20-Poly1305 encryption does not fail for in-memory input")
}

/// A type 0 envelope: base64 of `0x00 || iv || ciphertext`.
pub fn seal_type0(sym: &[u8; 32], iv: [u8; 12], plaintext: &[u8]) -> String {
    let mut v = vec![0u8];
    v.extend_from_slice(&iv);
    v.extend(seal(sym, iv, plaintext));
    B64.encode(v)
}

/// A type 1 envelope: base64 of `0x01 || sender public || iv || ciphertext`.
pub fn seal_type1(
    sym: &[u8; 32],
    sender_public: [u8; 32],
    iv: [u8; 12],
    plaintext: &[u8],
) -> String {
    let mut v = vec![1u8];
    v.extend_from_slice(&sender_public);
    v.extend_from_slice(&iv);
    v.extend(seal(sym, iv, plaintext));
    B64.encode(v)
}

/// A parsed, still sealed envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Envelope {
    /// Nonce and ciphertext only.
    Type0 {
        /// The ChaCha20-Poly1305 nonce.
        iv: [u8; 12],
        /// Ciphertext with the authentication tag.
        sealed: Vec<u8>,
    },
    /// Also carries the sender's public key.
    Type1 {
        /// The sender's X25519 public key.
        sender_public: [u8; 32],
        /// The ChaCha20-Poly1305 nonce.
        iv: [u8; 12],
        /// Ciphertext with the authentication tag.
        sealed: Vec<u8>,
    },
}

/// Parses a base64 envelope of type 0 or 1; anything else is an error.
pub fn parse(b64: &str) -> Result<Envelope, String> {
    let raw = B64
        .decode(b64)
        .map_err(|e| format!("envelope is not base64: {e}"))?;
    match raw.split_first() {
        Some((0, rest)) if rest.len() > 12 => Ok(Envelope::Type0 {
            iv: rest[..12].try_into().unwrap(),
            sealed: rest[12..].to_vec(),
        }),
        Some((1, rest)) if rest.len() > 44 => Ok(Envelope::Type1 {
            sender_public: rest[..32].try_into().unwrap(),
            iv: rest[32..44].try_into().unwrap(),
            sealed: rest[44..].to_vec(),
        }),
        Some((t, _)) => Err(format!(
            "unsupported envelope type {t} or truncated envelope"
        )),
        None => Err("empty envelope".into()),
    }
}

/// Decrypts and authenticates an envelope under `sym`.
pub fn open(sym: &[u8; 32], env: &Envelope) -> Result<Vec<u8>, String> {
    let (iv, sealed) = match env {
        Envelope::Type0 {
            iv,
            sealed,
        }
        | Envelope::Type1 {
            iv,
            sealed,
            ..
        } => (iv, sealed),
    };
    ChaCha20Poly1305::new(sym.into())
        .decrypt(Nonce::from_slice(iv), sealed.as_slice())
        .map_err(|_| "envelope does not decrypt with this key".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> KeyPair {
        KeyPair::from_secret(core::array::from_fn(|i| (i + 1) as u8))
    }
    fn b() -> KeyPair {
        KeyPair::from_secret(core::array::from_fn(|i| (i + 33) as u8))
    }
    const PLAIN: &[u8] = br#"{"id":1,"jsonrpc":"2.0","result":true}"#;

    #[test]
    fn keys_and_topic_match_an_independent_implementation() {
        assert_eq!(
            hex::encode(a().public),
            "07a37cbc142093c8b755dc1b10e86cb426374ad16aa853ed0bdfc0b2b86d1c7c"
        );
        assert_eq!(
            hex::encode(b().public),
            "5869aff450549732cbaaed5e5df9b30a6da31cb0e5742bad5ad4a1a768f1a67b"
        );
        let sym = derive_sym_key(&a().secret, &b().public);
        assert_eq!(sym, derive_sym_key(&b().secret, &a().public));
        assert_eq!(
            hex::encode(sym),
            "0f77e74c4faf2feee58fc2c41be0d0f32d5bd32150bdb548ffa65beb2b1ca573"
        );
        assert_eq!(
            topic_of(&sym),
            "c9c71dfb61298046cee15333ff7bd4431d01ac59814cc2783e1e4ba57c033d13"
        );
    }

    #[test]
    fn envelopes_match_an_independent_implementation() {
        let sym = derive_sym_key(&a().secret, &b().public);
        let env0 = seal_type0(&sym, [7; 12], PLAIN);
        assert_eq!(
            env0,
            "AAcHBwcHBwcHBwcHB9dJrSWUzoPaoMwD8HSIVed3iH1L9eUDjObVjIAEDdf/\
             EpDtLIl8cd9TXsLfOIAPz48WzknGCw=="
        );
        let env1 = seal_type1(&sym, b().public, [7; 12], PLAIN);
        assert_eq!(env1, "AVhpr/RQVJcyy6rtXl35swptoxyw5XQrrVrUoado8aZ7BwcHBwcHBwcHBwcH10mtJZTOg9qgzAPwdIhV53eIfUv15QOM5tWMgAQN1/8SkO0siXxx31Newt84gA/PjxbOScYL");
        assert_eq!(open(&sym, &parse(&env0).unwrap()).unwrap(), PLAIN);
        let Envelope::Type1 {
            sender_public, ..
        } = parse(&env1).unwrap()
        else {
            panic!()
        };
        assert_eq!(sender_public, b().public);
        let pairing = [0x11u8; 32];
        assert_eq!(
            seal_type0(&pairing, [7; 12], PLAIN),
            "AAcHBwcHBwcHBwcHB1r5+GbgahGMiYXI73BrveDH5OcTiTooyQc3jOUg2kQ2GwB8LG8M1u7uFyObwuKDNu0isCCvHA=="
        );
    }

    #[test]
    fn a_tampered_or_foreign_envelope_does_not_open() {
        let sym = derive_sym_key(&a().secret, &b().public);
        let mut raw = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            seal_type0(&sym, [7; 12], PLAIN),
        )
        .unwrap();
        *raw.last_mut().unwrap() ^= 1;
        let bad = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
        assert!(open(&sym, &parse(&bad).unwrap()).is_err());
        assert!(open(
            &[0u8; 32],
            &parse(&seal_type0(&sym, [7; 12], PLAIN)).unwrap()
        )
        .is_err());
        assert!(
            parse("AgAA").is_err(),
            "type 2 is not a type this client speaks"
        );
    }
}
