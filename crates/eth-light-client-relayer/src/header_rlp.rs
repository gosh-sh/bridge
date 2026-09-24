//! Ethereum execution-header RLP + keccak256 (same binding `submitAncestry`
//! uses).

use rlp::RlpStream;
use serde_json::Value;
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

/// Ethereum JSON quantities are hex without a guaranteed even digit count.
fn decode_qty(s: &str) -> Result<Vec<u8>, hex::FromHexError> {
    let h = s.trim_start_matches("0x");
    if h.len() % 2 == 1 {
        hex::decode(format!("0{h}"))
    } else {
        hex::decode(h)
    }
}

/// Encode `eth_getBlockByHash` JSON (`result` object) to canonical header RLP.
pub fn encode_header_rlp(block: &Value) -> Result<Vec<u8>, RelayerError> {
    fn hx(v: &Value, k: &str) -> Result<Vec<u8>, RelayerError> {
        let s = v
            .get(k)
            .and_then(|x| x.as_str())
            .ok_or_else(|| RelayerError::other(format!("block missing {k}")))?;
        decode_qty(s).map_err(|e| RelayerError::other(format!("{k}: {e}")))
    }
    fn hx_opt(v: &Value, k: &str) -> Result<Option<Vec<u8>>, RelayerError> {
        match v.get(k).and_then(|x| x.as_str()) {
            None | Some("") | Some("0x") => Ok(None),
            Some(s) => Ok(Some(
                decode_qty(s).map_err(|e| RelayerError::other(format!("{k}: {e}")))?,
            )),
        }
    }
    fn uint_bytes(v: &Value, k: &str) -> Result<Vec<u8>, RelayerError> {
        let mut b = hx(v, k)?;
        while b.first() == Some(&0) {
            b.remove(0);
        }
        Ok(b)
    }

    let parent = hx(block, "parentHash")?;
    let uncles = hx(block, "sha3Uncles")?;
    let miner = hx(block, "miner")?;
    let state = hx(block, "stateRoot")?;
    let txs = hx(block, "transactionsRoot")?;
    let rec = hx(block, "receiptsRoot")?;
    let bloom = hx(block, "logsBloom")?;
    let extra = hx_opt(block, "extraData")?.unwrap_or_default();
    let mix = hx(block, "mixHash")?;
    let nonce = hx(block, "nonce")?;

    let optionals = [
        hx_opt(block, "baseFeePerGas")?,
        hx_opt(block, "withdrawalsRoot")?,
        hx_opt(block, "blobGasUsed")?,
        hx_opt(block, "excessBlobGas")?,
        hx_opt(block, "parentBeaconBlockRoot")?,
        hx_opt(block, "requestsHash")?,
    ];
    let n_opt = optionals.iter().filter(|o| o.is_some()).count();
    let mut s = RlpStream::new_list(15 + n_opt);
    s.append(&parent.as_slice());
    s.append(&uncles.as_slice());
    s.append(&miner.as_slice());
    s.append(&state.as_slice());
    s.append(&txs.as_slice());
    s.append(&rec.as_slice());
    s.append(&bloom.as_slice());
    s.append(&uint_bytes(block, "difficulty")?.as_slice());
    s.append(&uint_bytes(block, "number")?.as_slice());
    s.append(&uint_bytes(block, "gasLimit")?.as_slice());
    s.append(&uint_bytes(block, "gasUsed")?.as_slice());
    s.append(&uint_bytes(block, "timestamp")?.as_slice());
    s.append(&extra.as_slice());
    s.append(&mix.as_slice());
    s.append(&nonce.as_slice());
    for b in optionals.into_iter().flatten() {
        if matches!(b.len(), 20 | 32 | 256) {
            s.append(&b.as_slice());
        } else {
            let mut u = b;
            while u.first() == Some(&0) {
                u.remove(0);
            }
            s.append(&u.as_slice());
        }
    }
    let rlp: Vec<u8> = s.out().into();
    // Runtime fork-gate: the node returns `hash` in the same JSON. If we
    // rebuilt the RLP wrong (a new header field we do not encode), keccak
    // diverges. Fail here instead of as a confusing on-chain ERR_BAD_ANCESTRY.
    if let Some(hash_s) = block.get("hash").and_then(|x| x.as_str()) {
        let want =
            decode_qty(hash_s).map_err(|e| RelayerError::other(format!("block.hash: {e}")))?;
        let got = keccak256(&rlp);
        if want.as_slice() != got.as_slice() {
            return Err(RelayerError::other(format!(
                "header RLP keccak {} != node hash {hash_s} — encoder is behind the current fork",
                hex::encode(got)
            )));
        }
    }
    Ok(rlp)
}

/// Two minimal linked header RLPs for tests (not real Ethereum headers).
/// `keccak(child)` has `parentHash == keccak(parent)`.
pub fn dummy_linked_headers() -> (Vec<u8>, Vec<u8>) {
    let mut parent = vec![0xf8, 33, 0xa0];
    parent.extend(std::iter::repeat_n(0x01, 32));
    let p_hash = keccak256(&parent);
    let mut child = vec![0xf8, 33, 0xa0];
    child.extend_from_slice(&p_hash);
    (child, parent)
}

/// Walk `checkpoint` toward genesis, at most `max` headers (checkpoint first).
pub fn link_headers(rlps: &[Vec<u8>]) -> Result<[u8; 32], RelayerError> {
    if rlps.len() < 2 {
        return Err(RelayerError::other("need checkpoint + ≥1 parent"));
    }
    if rlps.len() > 32 {
        return Err(RelayerError::other("ancestry longer than one epoch"));
    }
    let checkpoint = keccak256(&rlps[0]);
    let mut want = rlp_parent_hash(&rlps[0])?;
    for rlp in &rlps[1..] {
        let h = keccak256(rlp);
        if h != want {
            return Err(RelayerError::other("parent_hash break in header RLP chain"));
        }
        want = rlp_parent_hash(rlp)?;
    }
    Ok(checkpoint)
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
    fn linked_headers_roundtrip() {
        let parent_of_a = [0x22; 32];
        let mut a_body = vec![0xa0];
        a_body.extend_from_slice(&parent_of_a);
        // not a real header — link_headers uses keccak of whole RLP, so craft
        // two lists where keccak(child) is planted as parent of parent list.
        // Use real keccak chain: header P then header C with parentHash = keccak(P).
        let mut p = vec![0xf8, 33, 0xa0];
        p.extend(std::iter::repeat_n(0x01, 32));
        let p_hash = keccak256(&p);
        let mut c = vec![0xf8, 33, 0xa0];
        c.extend_from_slice(&p_hash);
        let ckpt = link_headers(&[c.clone(), p.clone()]).unwrap();
        assert_eq!(ckpt, keccak256(&c));
    }

    #[test]
    fn mainnet_block_1_rlp_keccak_matches_hash() {
        let v: Value = serde_json::from_str(include_str!("fixtures/mainnet_block_1.json"))
            .expect("fixture json");
        let rlp = encode_header_rlp(&v).unwrap();
        let got = keccak256(&rlp);
        let want = hex::decode(v["hash"].as_str().unwrap().trim_start_matches("0x")).unwrap();
        assert_eq!(hex::encode(got), hex::encode(&want));
        let parent = rlp_parent_hash(&rlp).unwrap();
        assert_eq!(
            hex::encode(parent),
            "d4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3"
        );
    }

    #[test]
    fn mainnet_shanghai_rlp_keccak_matches_hash() {
        let v: Value = serde_json::from_str(include_str!("fixtures/mainnet_dencun.json"))
            .expect("fixture json");
        let rlp = encode_header_rlp(&v).unwrap();
        let got = keccak256(&rlp);
        let want = hex::decode(v["hash"].as_str().unwrap().trim_start_matches("0x")).unwrap();
        assert_eq!(hex::encode(got), hex::encode(&want));
    }

    #[test]
    fn mainnet_prague_rlp_keccak_matches_hash() {
        let v: Value = serde_json::from_str(include_str!("fixtures/mainnet_prague.json"))
            .expect("fixture json");
        let rlp = encode_header_rlp(&v).unwrap();
        let got = keccak256(&rlp);
        let want = hex::decode(v["hash"].as_str().unwrap().trim_start_matches("0x")).unwrap();
        assert_eq!(hex::encode(got), hex::encode(&want));
    }

    #[test]
    fn encode_rejects_when_node_hash_does_not_match_rlp() {
        let mut v: Value = serde_json::from_str(include_str!("fixtures/mainnet_block_1.json"))
            .expect("fixture json");
        v["hash"] = Value::String(format!("0x{}", "ab".repeat(32)));
        let err = encode_header_rlp(&v).unwrap_err().to_string();
        assert!(
            err.contains("encoder is behind the current fork"),
            "got {err}"
        );
    }
}
