use std::fmt;

use serde::Deserialize;
use serde::Serialize;
use serde_with::serde_as;

pub const DST: [u8; 43] = *b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";

pub type SignerIndex = u16;

// ---------------------------------------------------------------------------
// PubKey
// ---------------------------------------------------------------------------
#[serde_as]
#[derive(Hash, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct PubKey(pub(crate) gosh_blst::min_pk::PublicKey);

impl PubKey {
    /// 48-byte compressed G1 representation.
    pub fn to_bytes(&self) -> [u8; 48] {
        self.0.to_bytes()
    }

    pub fn from_bytes(bytes: &[u8; 48]) -> anyhow::Result<Self> {
        let pk = gosh_blst::min_pk::PublicKey::from_bytes(bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse pubkey: {e:?}"))?;
        Ok(Self(pk))
    }
}

impl fmt::Debug for PubKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hexed = hex::encode(self.0.to_bytes());
        write!(f, "PubKey({}...{})", &hexed[..4], &hexed[92..])
    }
}

// ---------------------------------------------------------------------------
// Secret
// ---------------------------------------------------------------------------
#[serde_as]
#[derive(Deserialize, Serialize, Clone)]
pub struct Secret(pub(crate) gosh_blst::min_pk::SecretKey);

impl Secret {
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn from_bytes(bytes: &[u8; 32]) -> anyhow::Result<Self> {
        let sk = gosh_blst::min_pk::SecretKey::from_bytes(bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse secret: {e:?}"))?;
        Ok(Self(sk))
    }
}

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------
#[serde_as]
#[derive(Hash, Deserialize, Serialize, Clone, PartialEq, Eq, Debug)]
pub struct Signature(pub(crate) gosh_blst::min_pk::Signature);

// ---------------------------------------------------------------------------
// BLS operations
// ---------------------------------------------------------------------------

/// Sign data by bincode-serializing it, then BLS-signing the bytes.
/// Matches acki-nacki's `GoshBLS::sign`.
pub fn sign<TData: Serialize>(secret: &Secret, data: &TData) -> anyhow::Result<Signature> {
    let buffer = bincode::serialize(data)?;
    Ok(Signature(secret.0.sign(&buffer, &DST, &[])))
}

/// Merge two BLS signatures (aggregate).
pub fn merge(one: &Signature, another: &Signature) -> anyhow::Result<Signature> {
    let mut agg = gosh_blst::min_pk::AggregateSignature::from_signature(&one.0);
    gosh_blst::min_pk::AggregateSignature::add_signature(&mut agg, &another.0, false)
        .map_err(|e| anyhow::anyhow!("BLS merge failed: {e:?}"))?;
    Ok(Signature(agg.to_signature()))
}

/// Verify an aggregated BLS signature against pubkeys with occurrence counts.
/// Matches acki-nacki's `GoshBLS::verify`.
pub fn verify<TData: Serialize>(
    signature: &Signature,
    pubkeys_occurrences: &[(PubKey, usize)],
    data: &TData,
) -> anyhow::Result<bool> {
    let mut flattened: Vec<&gosh_blst::min_pk::PublicKey> = vec![];
    for (pk, count) in pubkeys_occurrences {
        for _ in 0..*count {
            flattened.push(&pk.0);
        }
    }
    let agg_pk = gosh_blst::min_pk::AggregatePublicKey::aggregate(&flattened, false)
        .map_err(|e| anyhow::anyhow!("Pubkey aggregation failed: {e:?}"))?;
    let buffer = bincode::serialize(data)?;
    let result =
        signature
            .0
            .verify(false, &buffer, &DST, &[], &agg_pk.to_public_key(), false);
    Ok(result == gosh_blst::BLST_ERROR::BLST_SUCCESS)
}

/// Generate a BLS keypair using gosh_blst.
pub fn gen_keypair() -> (Secret, PubKey) {
    let (pk, sk) = gosh_blst::gen_bls_key_pair();
    (Secret(sk), PubKey(pk))
}
