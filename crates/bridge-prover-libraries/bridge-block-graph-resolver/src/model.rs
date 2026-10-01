use std::{fmt, str::FromStr};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

macro_rules! hex_id {
    ($name:ident, $len:expr, $description:literal) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; $len]);

        impl $name {
            pub const ZERO: Self = Self([0; $len]);

            pub const fn from_bytes(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }

            pub const fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            pub fn is_zero(&self) -> bool {
                self.0 == [0; $len]
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = anyhow::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let bytes = hex::decode(value)
                    .map_err(|e| anyhow::anyhow!("invalid {} hex: {e}", $description))?;
                let bytes: [u8; $len] = bytes.try_into().map_err(|v: Vec<u8>| {
                    anyhow::anyhow!(
                        "invalid {} length: expected {} bytes, got {}",
                        $description,
                        $len,
                        v.len()
                    )
                })?;
                Ok(Self(bytes))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(D::Error::custom)
            }
        }
    };
}

hex_id!(BlockId, 32, "block id");
hex_id!(ThreadId, 34, "thread id");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockNode {
    pub block_id: BlockId,
    pub thread_id: ThreadId,
    pub height: u64,
    pub refs: Vec<BlockId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockEdge {
    /// Newer block that commits this edge.
    pub from: BlockId,
    /// Referenced older block.
    pub to: BlockId,
    /// `0` is the parent; `1+` are cross-thread references.
    pub ref_index: u32,
}

impl Ord for BlockEdge {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.from, self.ref_index, self.to).cmp(&(other.from, other.ref_index, other.to))
    }
}

impl PartialOrd for BlockEdge {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_display_and_serde_as_lowercase_hex() {
        let block: BlockId = "AA".repeat(32).parse().unwrap();
        assert_eq!(block.to_string(), "aa".repeat(32));
        assert_eq!(
            serde_json::to_string(&block).unwrap(),
            format!("\"{}\"", "aa".repeat(32))
        );
        assert_eq!(
            serde_json::from_str::<BlockId>(&serde_json::to_string(&block).unwrap()).unwrap(),
            block
        );

        let thread: ThreadId = "01".repeat(34).parse().unwrap();
        assert_eq!(thread.to_string(), "01".repeat(34));
        assert_eq!(
            serde_json::from_str::<ThreadId>(&format!("\"{}\"", thread)).unwrap(),
            thread
        );
    }

    #[test]
    fn ids_reject_bad_hex_and_length() {
        assert!("zz".repeat(32).parse::<BlockId>().is_err());
        assert!("00".repeat(31).parse::<BlockId>().is_err());
        assert!("00".repeat(33).parse::<ThreadId>().is_err());
    }
}
