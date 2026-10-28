//! Ethereum execution-header RLP + keccak256 (same binding `submitAncestry`
//! and `proveBlock` use). The one header encoder of this crate is alloy's,
//! behind [`encode_header_rlp`].

use alloy::{primitives::Bytes, rpc::types::Header as RpcHeader};
use tiny_keccak::{Hasher, Keccak};

use crate::error::RelayerError;

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut h = Keccak::v256();
    h.update(data);
    let mut out = [0u8; 32];
    h.finalize(&mut out);
    out
}

/// First RLP item of an Ethereum block header is `parentHash` (32 bytes).
pub fn rlp_parent_hash(header: &[u8]) -> Result<[u8; 32], RelayerError> {
    if header.is_empty() {
        return Err(RelayerError::other("empty header RLP"));
    }
    let p = header[0];
    let mut off = if p <= 0xf7 {
        1usize
    } else {
        1 + (p.saturating_sub(0xf7) as usize)
    };
    if off >= header.len() || header[off] != 0xa0 {
        return Err(RelayerError::other(
            "header RLP parentHash is not a 32-byte string",
        ));
    }
    off += 1;
    if off + 32 > header.len() {
        return Err(RelayerError::other("truncated parentHash"));
    }
    header[off..off + 32]
        .try_into()
        .map_err(|_| RelayerError::other("parentHash"))
}

/// The RLP of a block header, checked against the hash the node returned with
/// it: a header field the encoder does not know (a fork newer than alloy's
/// `Header`) fails here instead of as a confusing on-chain `ERR_BAD_ANCESTRY`
/// or a refused `proveBlock`.
pub fn encode_header_rlp(header: &RpcHeader) -> Result<Bytes, RelayerError> {
    let rlp = alloy::rlp::encode(&header.inner);
    if alloy::primitives::keccak256(&rlp) != header.hash {
        return Err(RelayerError::eth(format!(
            "the header of block {} does not encode to its hash {}: the node returns fields this \
             encoder does not know",
            header.inner.number, header.hash
        )));
    }
    Ok(rlp.into())
}

/// `n` minimal linked header RLPs, oldest first — the order `submitAncestry`
/// takes. Not real Ethereum headers.
pub fn dummy_linked_chain(n: usize) -> Vec<Vec<u8>> {
    let mut root = vec![0xf8, 33, 0xa0];
    root.extend(std::iter::repeat_n(0x01, 32));
    let mut out = vec![root];
    for _ in 1..n {
        let prev = keccak256(out.last().expect("non-empty"));
        let mut next = vec![0xf8, 33, 0xa0];
        next.extend_from_slice(&prev);
        out.push(next);
    }
    out
}

/// Check an ancestry payload in the shape `submitAncestry` takes: oldest
/// first, 3 to 33 headers, each one naming its predecessor as parent. Returns
/// the two end hashes, both of which the contract requires to be proven
/// anchors — `(older, newer)`.
pub fn link_headers(rlps: &[Vec<u8>]) -> Result<([u8; 32], [u8; 32]), RelayerError> {
    if rlps.len() < 3 {
        return Err(RelayerError::other(
            "need two anchors + ≥1 block between them",
        ));
    }
    if rlps.len() > 33 {
        return Err(RelayerError::other(
            "ancestry longer than two anchors + one epoch",
        ));
    }
    let older = keccak256(&rlps[0]);
    let mut prev = older;
    for rlp in &rlps[1..] {
        if rlp_parent_hash(rlp)? != prev {
            return Err(RelayerError::other("parent_hash break in header RLP chain"));
        }
        prev = keccak256(rlp);
    }
    Ok((older, prev))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keccak_empty() {
        let h = keccak256(b"");
        assert_eq!(
            hex::encode(h),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    #[test]
    fn keccak_abc() {
        let h = keccak256(b"abc");
        assert_eq!(
            hex::encode(h),
            "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
        );
    }

    #[test]
    fn rlp_parent_of_minimal_list() {
        // list of one 32-byte string 0x11..
        let mut rlp = vec![0xc0 + 33, 0xa0];
        rlp.extend(std::iter::repeat_n(0x11, 32));
        assert_eq!(rlp_parent_hash(&rlp).unwrap(), [0x11; 32]);
    }

    #[test]
    fn link_headers_accepts_oldest_first_span() {
        let chain = dummy_linked_chain(3);
        let (older, newer) = link_headers(&chain).unwrap();
        assert_eq!(older, keccak256(&chain[0]));
        assert_eq!(newer, keccak256(&chain[2]));
    }

    #[test]
    fn link_headers_refuses_newest_first_span() {
        let mut chain = dummy_linked_chain(3);
        chain.reverse();
        assert!(link_headers(&chain).is_err());
    }

    #[test]
    fn link_headers_refuses_a_span_without_room_between_the_anchors() {
        assert!(link_headers(&dummy_linked_chain(2)).is_err());
    }

    #[test]
    fn link_headers_refuses_more_than_two_anchors_and_an_epoch() {
        assert!(link_headers(&dummy_linked_chain(34)).is_err());
        assert!(link_headers(&dummy_linked_chain(33)).is_ok());
    }

    fn header(json: &str) -> RpcHeader {
        serde_json::from_str(json).expect("fixture json")
    }

    #[test]
    fn mainnet_block_1_rlp_keccak_matches_hash() {
        let h = header(include_str!("fixtures/mainnet_block_1.json"));
        let rlp = encode_header_rlp(&h).unwrap();
        assert_eq!(keccak256(&rlp), h.hash.0);
        let parent = rlp_parent_hash(&rlp).unwrap();
        assert_eq!(
            hex::encode(parent),
            "d4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3"
        );
    }

    #[test]
    fn mainnet_shanghai_rlp_keccak_matches_hash() {
        let h = header(include_str!("fixtures/mainnet_dencun.json"));
        assert_eq!(keccak256(&encode_header_rlp(&h).unwrap()), h.hash.0);
    }

    #[test]
    fn mainnet_prague_rlp_keccak_matches_hash() {
        let h = header(include_str!("fixtures/mainnet_prague.json"));
        assert_eq!(keccak256(&encode_header_rlp(&h).unwrap()), h.hash.0);
    }

    #[test]
    fn sepolia_amsterdam_rlp_keccak_matches_hash() {
        let h = header(include_str!("fixtures/sepolia_amsterdam.json"));
        assert_eq!(keccak256(&encode_header_rlp(&h).unwrap()), h.hash.0);
    }

    #[test]
    fn encode_rejects_a_header_without_a_field_the_node_hashed() {
        let mut v: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/sepolia_amsterdam.json"))
                .expect("fixture json");
        v["requestsHash"] = serde_json::Value::Null;
        let h: RpcHeader = serde_json::from_value(v).expect("header");
        let err = encode_header_rlp(&h).unwrap_err().to_string();
        assert!(err.contains("does not encode to its hash"), "got {err}");
    }

    #[test]
    fn encode_rejects_when_node_hash_does_not_match_rlp() {
        let mut h = header(include_str!("fixtures/mainnet_block_1.json"));
        h.hash = alloy::primitives::B256::repeat_byte(0xab);
        let err = encode_header_rlp(&h).unwrap_err().to_string();
        assert!(err.contains("does not encode to its hash"), "got {err}");
    }
}
