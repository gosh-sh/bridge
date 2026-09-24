use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde_with::serde_as;

use crate::bls::Signature;
use crate::bls::SignerIndex;

// ---------------------------------------------------------------------------
// BlockIdentifier — 32 bytes, serde_with::Bytes (bincode: u64 len + 32 bytes)
// ---------------------------------------------------------------------------
#[serde_as]
#[derive(Copy, Clone, Hash, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct BlockIdentifier(#[serde_as(as = "serde_with::Bytes")] pub [u8; 32]);

impl Default for BlockIdentifier {
    fn default() -> Self {
        Self([0u8; 32])
    }
}

impl fmt::Debug for BlockIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlockId({})", hex::encode(self.0))
    }
}

impl fmt::Display for BlockIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl FromStr for BlockIdentifier {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s, &mut bytes)?;
        Ok(Self(bytes))
    }
}

// ---------------------------------------------------------------------------
// BlockSeqNo — newtype around u32
// ---------------------------------------------------------------------------
#[derive(
    Copy, Clone, Eq, Hash, PartialEq, Serialize, Deserialize, Default, PartialOrd, Ord, Debug,
)]
pub struct BlockSeqNo(pub u32);

impl From<u32> for BlockSeqNo {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

// ---------------------------------------------------------------------------
// AckiNackiEnvelopeHash — [u8; 32] transparent serde
// ---------------------------------------------------------------------------
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(transparent)]
pub struct AckiNackiEnvelopeHash(pub [u8; 32]);

impl fmt::Debug for AckiNackiEnvelopeHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EnvHash({})", hex::encode(self.0))
    }
}

// ---------------------------------------------------------------------------
// AttestationTargetType
// ---------------------------------------------------------------------------
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[repr(u8)]
pub enum AttestationTargetType {
    Primary,
    Fallback,
}

// ---------------------------------------------------------------------------
// AttestationData
// ---------------------------------------------------------------------------
#[derive(Eq, PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct AttestationData {
    pub parent_block_id: BlockIdentifier,
    pub block_id: BlockIdentifier,
    pub block_seq_no: BlockSeqNo,
    pub envelope_hash: AckiNackiEnvelopeHash,
    pub target_type: AttestationTargetType,
}

// ---------------------------------------------------------------------------
// Envelope<TData> — sorted EnvelopeSerDe pattern
// ---------------------------------------------------------------------------
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Envelope<TData> {
    pub aggregated_signature: Signature,
    pub signature_occurrences: HashMap<SignerIndex, u16>,
    pub data: TData,
}

#[derive(Serialize, Deserialize)]
struct EnvelopeSerDe<TData> {
    pub aggregated_signature: Signature,
    pub signature_occurrences: Vec<(SignerIndex, u16)>,
    pub data: TData,
}

impl<TData: Serialize + Clone> Serialize for Envelope<TData> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut occurrences: Vec<(SignerIndex, u16)> =
            self.signature_occurrences.clone().into_iter().collect();
        occurrences.sort_by(|(a, _), (b, _)| a.cmp(b));
        let serde = EnvelopeSerDe {
            aggregated_signature: self.aggregated_signature.clone(),
            signature_occurrences: occurrences,
            data: self.data.clone(),
        };
        serde.serialize(serializer)
    }
}

impl<'de, TData: Deserialize<'de> + Clone> Deserialize<'de> for Envelope<TData> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let serde = EnvelopeSerDe::<TData>::deserialize(deserializer)?;
        Ok(Self {
            aggregated_signature: serde.aggregated_signature,
            signature_occurrences: HashMap::from_iter(serde.signature_occurrences),
            data: serde.data,
        })
    }
}

impl<TData: Serialize + Clone> Envelope<TData> {
    /// Create an envelope with the first signer's signature.
    pub fn sealed(
        data: TData,
        secret: &crate::bls::Secret,
        signer_index: SignerIndex,
    ) -> anyhow::Result<Self> {
        let signature = crate::bls::sign(secret, &data)?;
        let mut occurrences = HashMap::new();
        occurrences.insert(signer_index, 1);
        Ok(Self {
            aggregated_signature: signature,
            signature_occurrences: occurrences,
            data,
        })
    }

    /// Add another signer's signature (aggregate).
    pub fn add_signature(
        &mut self,
        signer_index: SignerIndex,
        secret: &crate::bls::Secret,
    ) -> anyhow::Result<()> {
        let new_sig = crate::bls::sign(secret, &self.data)?;
        self.aggregated_signature = crate::bls::merge(&self.aggregated_signature, &new_sig)?;
        let count = self.signature_occurrences.entry(signer_index).or_insert(0);
        *count += 1;
        Ok(())
    }
}
