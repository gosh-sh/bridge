//! Epoch ancestry for 31/32 non-checkpoint deposits.
//!
//! A light-client `submitUpdate` anchors one execution `block_hash` (the
//! checkpoint). Every other block in that epoch is covered iff it sits on the
//! execution **parent-hash chain** ending at that checkpoint (at most 31
//! parents). The Halo2 step circuit does not prove this; the relayer (and
//! later `submitAncestry` on `EthBeaconLightClient`) does.

/// Execution payload parent link (`eth/v2/beacon/blocks` body).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecLink {
    pub block_hash: [u8; 32],
    pub parent_hash: [u8; 32],
}

/// `links[0].block_hash == checkpoint`. Each next link is the parent:
/// `links[i].parent_hash == links[i + 1].block_hash`. Depth ≤ 32.
pub fn covers_deposit(
    checkpoint: [u8; 32],
    deposit: [u8; 32],
    links: &[ExecLink],
) -> Result<(), String> {
    if links.is_empty() {
        return Err("empty ancestry chain".into());
    }
    if links[0].block_hash != checkpoint {
        return Err("chain does not start at the light-client checkpoint".into());
    }
    if links.len() > 32 {
        return Err(format!("ancestry longer than one epoch ({})", links.len()));
    }
    for w in links.windows(2) {
        if w[0].parent_hash != w[1].block_hash {
            return Err("parent_hash break in ancestry chain".into());
        }
    }
    if links.iter().any(|l| l.block_hash == deposit) {
        return Ok(());
    }
    Err("deposit block_hash is not on the checkpoint parent chain".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn chain() -> Vec<ExecLink> {
        // checkpoint C ← B ← A (oldest)
        vec![
            ExecLink {
                block_hash: h(3),
                parent_hash: h(2),
            },
            ExecLink {
                block_hash: h(2),
                parent_hash: h(1),
            },
            ExecLink {
                block_hash: h(1),
                parent_hash: h(0),
            },
        ]
    }

    #[test]
    fn checkpoint_covers_itself() {
        covers_deposit(h(3), h(3), &chain()).unwrap();
    }

    #[test]
    fn epoch_interior_is_covered() {
        covers_deposit(h(3), h(1), &chain()).unwrap();
    }

    #[test]
    fn off_chain_rejected() {
        assert!(covers_deposit(h(3), h(9), &chain()).is_err());
    }

    #[test]
    fn broken_parent_rejected() {
        let mut c = chain();
        c[1].block_hash = h(8);
        assert!(covers_deposit(h(3), h(1), &c).is_err());
    }

    #[test]
    fn empty_chain_rejected() {
        assert!(covers_deposit(h(3), h(3), &[]).is_err());
    }

    #[test]
    fn wrong_checkpoint_start_rejected() {
        assert!(covers_deposit(h(9), h(3), &chain()).is_err());
    }

    #[test]
    fn longer_than_epoch_rejected() {
        let links: Vec<ExecLink> = (0u8..=32)
            .rev()
            .map(|i| ExecLink {
                block_hash: h(i),
                parent_hash: h(i.saturating_sub(1)),
            })
            .collect();
        assert_eq!(links.len(), 33);
        assert!(covers_deposit(h(32), h(0), &links).is_err());
    }
}
