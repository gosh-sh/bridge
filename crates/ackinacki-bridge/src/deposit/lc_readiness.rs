//! Item 10: can the light client, alone, anchor this deposit's block?
//!
//! `isAcceptedBlockHash` does not say who anchored a block, and
//! `disableOwnerAnchors()` leaves the owner's earlier anchors in place,
//! so an accepted block proves nothing by itself. Three checks, in order:
//! the light client follows the deposit's chain; the bridge accepts the
//! light client's head under its real hash (the byte order a deposit
//! proof uses); and ancestry has anchored a block between checkpoints
//! after the owner was switched off, recently.
//!
//! The light client reports block hashes as its stored keys, and which
//! byte order those keys are in has changed between its versions. Every
//! hash it reports is therefore looked up on the EVM chain in both
//! orders, and only the real block hash found there is put to the bridge.

use alloy_primitives::B256;
use serde_json::json;

use crate::deposit::{
    an::{AnRead, ExtDir},
    evm::{BlockTag, EvmRead, Header},
    identity::{pi_form, BRIDGE_ABI, LIGHT_CLIENT_ABI},
    ui::Ui,
};

/// One beacon-chain epoch: 32 slots of 12 s.
pub const EPOCH_S: u64 = 32 * 12;
/// How far the light client's head may trail the finalized EVM head.
/// Build constants, not flags, so they cannot be loosened by a user.
/// Initial values; calibrate on the stand once owner anchors are off.
pub const HEAD_MAX_LAG_S: u64 = 4 * EPOCH_S;
/// How far the checkpoint of the newest ancestry may trail the light
/// client's head before ancestry counts as stopped.
pub const ANCESTRY_MAX_LAG_S: u64 = 2 * EPOCH_S;
/// External address `AncestryAccepted` is emitted to.
pub const ANCESTRY_ACCEPTED_DST: &str =
    ":00000000000000000000000000000000000000000000000000000000000002c0";

/// Why the light client cannot anchor this deposit, named after the check
/// that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcFailure {
    /// Owner anchors are off and the bridge has no light client.
    NoLightClient,
    /// Check 1: the light client follows chain `lc`, not the deposit's.
    Network {
        /// The chain the light client follows.
        lc: u64,
        /// The deposit's chain.
        deposit: u64,
    },
    /// Check 2: the head is no block on the deposit's chain in either
    /// byte order.
    HeadNotFound {
        /// The head as the light client reports it.
        head: B256,
    },
    /// Check 2: the bridge does not accept the head's real block hash.
    KeyForm {
        /// The head's real block hash.
        block: B256,
    },
    /// Check 3: no successful `disableOwnerAnchors` in the bridge's history.
    NoFlip,
    /// Check 3: no `AncestryAccepted` with new hashes since the switch-off.
    NoAncestryAfterFlip,
    /// Check 3: the ancestry's checkpoint, or its parent, is no block on
    /// the deposit's chain.
    CheckpointNotFound {
        /// The hash that was looked up.
        checkpoint: B256,
    },
    /// Check 3: the block before the checkpoint is not newer than the
    /// switch-off, so the owner may have anchored it.
    ParentNotAfterFlip {
        /// The parent block's time.
        parent_ts: u64,
        /// The switch-off's time.
        t_flip: u64,
    },
    /// Check 3: the bridge does not accept the block before the checkpoint.
    ParentNotAccepted {
        /// The parent block's hash.
        parent: B256,
    },
    /// Check 3: the head trails the finalized EVM head too far.
    HeadStale {
        /// Seconds behind.
        lag_s: u64,
    },
    /// Check 3: the newest ancestry's checkpoint trails the head too far.
    AncestryStale {
        /// Seconds behind.
        lag_s: u64,
    },
}

impl std::fmt::Display for LcFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use LcFailure::*;
        match self {
            NoLightClient => write!(
                f,
                "owner anchors are off and the bridge names no light client"
            ),
            Network {
                lc,
                deposit,
            } => write!(
                f,
                "light-client check 1 (network): it follows chain {lc}, the deposit is on chain \
                 {deposit}; nobody can anchor this block"
            ),
            HeadNotFound {
                head,
            } => write!(
                f,
                "light-client check 2 (key form): its head {head} is not a block on this chain in \
                 either byte order"
            ),
            KeyForm {
                block,
            } => write!(
                f,
                "light-client check 2 (key form): the bridge does not accept its head block \
                 {block} under the hash a deposit proof uses"
            ),
            NoFlip => write!(
                f,
                "light-client check 3 (ancestry): no successful disableOwnerAnchors in the \
                 bridge's history, so fresh anchors may still be the owner's"
            ),
            NoAncestryAfterFlip => write!(
                f,
                "light-client check 3 (ancestry): no AncestryAccepted with new hashes since owner \
                 anchors were switched off"
            ),
            CheckpointNotFound {
                checkpoint,
            } => write!(
                f,
                "light-client check 3 (ancestry): checkpoint {checkpoint} is not a block on this \
                 chain"
            ),
            ParentNotAfterFlip {
                parent_ts,
                t_flip,
            } => write!(
                f,
                "light-client check 3 (ancestry): the block before the checkpoint ({parent_ts}) \
                 is not newer than the switch-off ({t_flip})"
            ),
            ParentNotAccepted {
                parent,
            } => write!(
                f,
                "light-client check 3 (ancestry): the bridge did not accept {parent}; the light \
                 client's anchor did not reach it"
            ),
            HeadStale {
                lag_s,
            } => write!(
                f,
                "light-client check 3 (freshness): its head is {lag_s} s behind the finalized \
                 chain (limit {HEAD_MAX_LAG_S} s)"
            ),
            AncestryStale {
                lag_s,
            } => write!(
                f,
                "light-client check 3 (freshness): its last ancestry is {lag_s} s behind its head \
                 (limit {ANCESTRY_MAX_LAG_S} s); ancestry has stopped"
            ),
        }
    }
}

/// One `AncestryAccepted` event of the light client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestry {
    /// The checkpoint the walk started from, as the light client reports it.
    pub checkpoint: B256,
    /// How many new block hashes the walk anchored.
    pub hashes_added: u64,
    /// When the event was emitted, in seconds.
    pub created_at: u64,
}

/// What [`observe`] read; `None` for what it could not find or did not
/// get to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LcObs {
    /// `getConfig().l1ChainId`.
    pub lc_chain_id: Option<u64>,
    /// The real block the light client's head names.
    pub head: Option<Header>,
    /// `getHead().executionBlockHash` as the light client reports it.
    pub head_raw: B256,
    /// The bridge's `isAcceptedBlockHash` for the head's real hash.
    pub bridge_accepts_head: Option<bool>,
    /// The time of the newest successful `disableOwnerAnchors`.
    pub t_flip: Option<u64>,
    /// The newest ancestry with new hashes after the switch-off.
    pub ancestry: Option<Ancestry>,
    /// The real block that ancestry's checkpoint names.
    pub checkpoint: Option<Header>,
    /// The checkpoint's parent.
    pub parent: Option<Header>,
    /// The bridge's `isAcceptedBlockHash` for the parent.
    pub bridge_accepts_parent: Option<bool>,
    /// The finalized EVM head.
    pub finalized_evm: Option<Header>,
}

/// The newest event that added hashes after `t_flip`, by event time.
pub fn newest_ancestry_after(events: &[Ancestry], t_flip: u64) -> Option<Ancestry> {
    events
        .iter()
        .filter(|e| e.hashes_added > 0 && e.created_at > t_flip)
        .max_by_key(|e| e.created_at)
        .cloned()
}

/// The two freshness bounds of check 3: the head against the finalized EVM
/// head, then the checkpoint against the head. Both bounds are inclusive.
pub fn freshness(head: &Header, checkpoint: &Header, finalized: &Header) -> Result<(), LcFailure> {
    let head_lag = finalized.timestamp.saturating_sub(head.timestamp);
    if head_lag > HEAD_MAX_LAG_S {
        return Err(LcFailure::HeadStale {
            lag_s: head_lag,
        });
    }
    let anc_lag = head.timestamp.saturating_sub(checkpoint.timestamp);
    if anc_lag > ANCESTRY_MAX_LAG_S {
        return Err(LcFailure::AncestryStale {
            lag_s: anc_lag,
        });
    }
    Ok(())
}

/// The three checks in order; the first that fails is the verdict.
pub fn judge(o: &LcObs, chain_id: u64) -> Result<(), LcFailure> {
    let lc = o.lc_chain_id.unwrap_or(0);
    if lc != chain_id {
        return Err(LcFailure::Network {
            lc,
            deposit: chain_id,
        });
    }
    let head = o.head.as_ref().ok_or(LcFailure::HeadNotFound {
        head: o.head_raw,
    })?;
    if o.bridge_accepts_head != Some(true) {
        return Err(LcFailure::KeyForm {
            block: head.hash,
        });
    }
    let t_flip = o.t_flip.ok_or(LcFailure::NoFlip)?;
    let anc = o.ancestry.as_ref().ok_or(LcFailure::NoAncestryAfterFlip)?;
    let c = o.checkpoint.as_ref().ok_or(LcFailure::CheckpointNotFound {
        checkpoint: anc.checkpoint,
    })?;
    let p = o.parent.as_ref().ok_or(LcFailure::CheckpointNotFound {
        checkpoint: c.parent_hash,
    })?;
    if p.timestamp <= t_flip {
        return Err(LcFailure::ParentNotAfterFlip {
            parent_ts: p.timestamp,
            t_flip,
        });
    }
    if o.bridge_accepts_parent != Some(true) {
        return Err(LcFailure::ParentNotAccepted {
            parent: p.hash,
        });
    }
    let fin = o.finalized_evm.as_ref().ok_or(LcFailure::HeadStale {
        lag_s: u64::MAX,
    })?;
    freshness(head, c, fin)
}

/// Who the anchor wait expects to accept the deposit's block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorPlan {
    /// Owner anchors are on; `lc_ready` when the light client passed the
    /// three checks as well.
    Owner {
        /// The light client can anchor the block too.
        lc_ready: bool,
    },
    /// Owner anchors are off; only the light client can anchor the block.
    LightClient,
}

impl AnchorPlan {
    /// The status line of the anchor wait for `block`.
    pub fn status_line(&self, block: B256) -> String {
        match self {
            AnchorPlan::Owner {
                lc_ready: false,
            } => format!("waiting for the bridge owner to accept block {block} (a manual step)"),
            AnchorPlan::Owner {
                lc_ready: true,
            } => {
                format!("waiting for the bridge owner or the light client to accept block {block}")
            },
            AnchorPlan::LightClient => {
                format!(
                    "waiting for the light client: block {block} must fall under a proven \
                     checkpoint"
                )
            },
        }
    }
}

/// With owner anchors on, the deposit proceeds whatever the light client's
/// state, which only changes the status text. With them off, it proceeds
/// only if the bridge names a light client and that light client passed.
pub fn plan(
    owner_enabled: bool,
    light_client: Option<[u8; 32]>,
    readiness: Result<(), LcFailure>,
) -> Result<AnchorPlan, LcFailure> {
    if owner_enabled {
        return Ok(AnchorPlan::Owner {
            lc_ready: light_client.is_some() && readiness.is_ok(),
        });
    }
    light_client.ok_or(LcFailure::NoLightClient)?;
    readiness.map(|_| AnchorPlan::LightClient)
}

/// The SDK renders `uint` outputs as strings, hex with `0x` or decimal;
/// `U256::from_str` reads both.
fn uint(v: &serde_json::Value) -> Option<alloy_primitives::U256> {
    match v {
        serde_json::Value::String(s) => s.parse().ok(),
        serde_json::Value::Number(n) => n.as_u64().map(alloy_primitives::U256::from),
        _ => None,
    }
}

/// The real block a hash from the light client names: `h` itself, or `h`
/// with each 16-byte half reversed.
async fn find_block(evm: &dyn EvmRead, h: B256) -> Option<Header> {
    if let Ok(Some(b)) = evm.header_by_hash(h).await {
        return Some(b);
    }
    let other = B256::from(pi_form(h.0));
    if other == h {
        return None;
    }
    evm.header_by_hash(other).await.ok().flatten()
}

/// The bridge's `isAcceptedBlockHash(chain_id, h)`; `h` goes in the byte
/// order a deposit proof uses.
async fn accepted(an: &dyn AnRead, bridge: [u8; 32], chain_id: u64, h: B256) -> Option<bool> {
    let v = an
        .run_getter(
            bridge,
            BRIDGE_ABI,
            "isAcceptedBlockHash",
            json!({ "chainId": chain_id.to_string(), "blockHash": format!("{h:#x}") }),
        )
        .await
        .ok()?;
    v["value0"].as_bool()
}

/// The time of the newest `disableOwnerAnchors` whose transaction did not
/// abort. A read that fails gives `None`: the switch-off stays unknown,
/// never an older one.
async fn t_flip(an: &dyn AnRead, bridge: [u8; 32]) -> Option<u64> {
    let mut before = None;
    loop {
        let page = an
            .ext_messages(bridge, ExtDir::In, before.clone())
            .await
            .ok()?;
        for m in &page.items {
            let Some(body) = &m.body else { continue };
            if an.decode(BRIDGE_ABI, body, false).map(|(n, _)| n)
                != Some("disableOwnerAnchors".into())
            {
                continue;
            }
            let full = an.message(&m.hash).await.ok()??;
            if let Some(tx) = full.dst_tx.filter(|t| !t.aborted) {
                return Some(tx.now);
            }
        }
        before = Some(page.cursor?);
    }
}

/// The light client's `AncestryAccepted` events, newest first, down to the
/// first page that reaches `not_before`: the list is newest first, so
/// nothing past that page is newer.
async fn ancestry_events(an: &dyn AnRead, lc: [u8; 32], not_before: u64) -> Vec<Ancestry> {
    let mut out = Vec::new();
    let mut before = None;
    loop {
        let Ok(page) = an.ext_messages(lc, ExtDir::Out, before.clone()).await else {
            break;
        };
        let mut older = false;
        for m in &page.items {
            older |= m.created_at <= not_before;
            if !m.dst.ends_with(ANCESTRY_ACCEPTED_DST) {
                continue;
            }
            let Some((name, v)) = m
                .body
                .as_deref()
                .and_then(|b| an.decode(LIGHT_CLIENT_ABI, b, false))
            else {
                continue;
            };
            if name != "AncestryAccepted" {
                continue;
            }
            let Some(c) = uint(&v["checkpointHash"]).map(B256::from) else {
                continue;
            };
            let added = uint(&v["hashesAdded"])
                .map(|u| u.saturating_to::<u64>())
                .unwrap_or(0);
            out.push(Ancestry {
                checkpoint: c,
                hashes_added: added,
                created_at: m.created_at,
            });
        }
        match (older, page.cursor) {
            (false, Some(c)) => before = Some(c),
            _ => break,
        }
    }
    out
}

/// Reads what [`judge`] decides on, check by check, and stops at the first
/// check that fails. A read that fails leaves its field `None`.
pub async fn observe(
    an: &dyn AnRead,
    evm: &dyn EvmRead,
    bridge: [u8; 32],
    lc: [u8; 32],
    chain_id: u64,
    ui: &dyn Ui,
) -> LcObs {
    ui.status("checking whether the light client can anchor this deposit");
    let mut o = LcObs::default();
    let cfg = an
        .run_getter(lc, LIGHT_CLIENT_ABI, "getConfig", json!({}))
        .await
        .ok();
    o.lc_chain_id = cfg
        .and_then(|c| uint(&c["l1ChainId"]))
        .map(|u| u.saturating_to::<u64>());
    if o.lc_chain_id != Some(chain_id) {
        return o;
    }
    let head = an
        .run_getter(lc, LIGHT_CLIENT_ABI, "getHead", json!({}))
        .await
        .ok();
    o.head_raw = head
        .and_then(|h| uint(&h["executionBlockHash"]))
        .map(B256::from)
        .unwrap_or_default();
    o.head = find_block(evm, o.head_raw).await;
    let Some(h) = o.head.clone() else { return o };
    o.bridge_accepts_head = accepted(an, bridge, chain_id, h.hash).await;
    if o.bridge_accepts_head != Some(true) {
        return o;
    }
    o.t_flip = t_flip(an, bridge).await;
    let Some(tf) = o.t_flip else { return o };
    o.ancestry = newest_ancestry_after(&ancestry_events(an, lc, tf).await, tf);
    let Some(a) = o.ancestry.clone() else {
        return o;
    };
    o.checkpoint = find_block(evm, a.checkpoint).await;
    let Some(c) = o.checkpoint.clone() else {
        return o;
    };
    o.parent = evm.header_by_hash(c.parent_hash).await.ok().flatten();
    let Some(p) = o.parent.clone() else { return o };
    if p.timestamp <= tf {
        return o;
    }
    o.bridge_accepts_parent = accepted(an, bridge, chain_id, p.hash).await;
    if o.bridge_accepts_parent != Some(true) {
        return o;
    }
    o.finalized_evm = evm.header(BlockTag::Finalized).await.ok().flatten();
    o
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;

    use super::*;
    use crate::deposit::{evm::Header, identity::pi_form};

    const T_FLIP: u64 = 1_786_000_000;

    fn hdr(n: u64, tag: u8, ts: u64, parent: u8) -> Header {
        Header {
            number: n,
            hash: B256::repeat_byte(tag),
            parent_hash: B256::repeat_byte(parent),
            timestamp: ts,
        }
    }

    /// Everything healthy: the head is a real block the bridge accepts in
    /// normal byte order, ancestry ran after the flip, its checkpoint's
    /// parent is newer than the flip and accepted, and both are fresh.
    fn ready() -> LcObs {
        let head = hdr(1000, 0xaa, T_FLIP + 5000, 0xa9);
        let ckpt = hdr(968, 0xcc, T_FLIP + 5000 - 384, 0xcb);
        LcObs {
            lc_chain_id: Some(11_155_111),
            head: Some(head.clone()),
            head_raw: head.hash,
            bridge_accepts_head: Some(true),
            t_flip: Some(T_FLIP),
            ancestry: Some(Ancestry {
                checkpoint: ckpt.hash,
                hashes_added: 31,
                created_at: T_FLIP + 4800,
            }),
            checkpoint: Some(ckpt.clone()),
            parent: Some(hdr(967, 0xcb, T_FLIP + 5000 - 396, 0xca)),
            bridge_accepts_parent: Some(true),
            finalized_evm: Some(hdr(1010, 0xff, T_FLIP + 5000 + 120, 0xfe)),
        }
    }

    #[test]
    fn a_ready_light_client_passes() {
        assert_eq!(judge(&ready(), 11_155_111), Ok(()));
    }

    #[test]
    fn an_l2_deposit_fails_check_1() {
        assert!(matches!(
            judge(&ready(), 8453),
            Err(LcFailure::Network {
                lc: 11_155_111,
                deposit: 8453
            })
        ));
    }

    #[test]
    fn the_shellnet_state_of_2026_09_28_fails_check_2() {
        // The head is only found in the reversed-halves form, and the bridge
        // does not accept the real block hash.
        let mut o = ready();
        o.head_raw = B256::from(pi_form(o.head.as_ref().unwrap().hash.0));
        o.bridge_accepts_head = Some(false);
        assert!(matches!(
            judge(&o, 11_155_111),
            Err(LcFailure::KeyForm { .. })
        ));
    }

    #[test]
    fn an_owner_anchor_from_before_the_flip_does_not_count() {
        let mut o = ready();
        o.parent.as_mut().unwrap().timestamp = T_FLIP - 1;
        assert!(matches!(
            judge(&o, 11_155_111),
            Err(LcFailure::ParentNotAfterFlip { .. })
        ));
    }

    #[test]
    fn no_flip_in_history_is_a_refusal() {
        let mut o = ready();
        o.t_flip = None;
        assert_eq!(judge(&o, 11_155_111), Err(LcFailure::NoFlip));
    }

    #[test]
    fn a_bounced_anchor_is_a_refusal() {
        let mut o = ready();
        o.bridge_accepts_parent = Some(false);
        assert!(matches!(
            judge(&o, 11_155_111),
            Err(LcFailure::ParentNotAccepted { .. })
        ));
    }

    #[test]
    fn an_old_ancestry_with_a_fresh_head_is_a_stopped_ancestry() {
        let mut o = ready();
        let c = o.checkpoint.as_mut().unwrap();
        c.timestamp = o.head.as_ref().unwrap().timestamp - 769;
        assert!(matches!(
            judge(&o, 11_155_111),
            Err(LcFailure::AncestryStale {
                lag_s: 769
            })
        ));
    }

    #[test]
    fn a_head_far_behind_finalized_is_stale() {
        let mut o = ready();
        o.finalized_evm.as_mut().unwrap().timestamp = o.head.as_ref().unwrap().timestamp + 1537;
        assert!(matches!(
            judge(&o, 11_155_111),
            Err(LcFailure::HeadStale {
                lag_s: 1537
            })
        ));
    }

    #[test]
    fn the_newest_ancestry_is_chosen_by_time_not_by_list_order() {
        let evs = vec![
            Ancestry {
                checkpoint: B256::repeat_byte(3),
                hashes_added: 5,
                created_at: T_FLIP + 300,
            },
            Ancestry {
                checkpoint: B256::repeat_byte(1),
                hashes_added: 5,
                created_at: T_FLIP + 900,
            },
            Ancestry {
                checkpoint: B256::repeat_byte(2),
                hashes_added: 0,
                created_at: T_FLIP + 1200,
            },
            Ancestry {
                checkpoint: B256::repeat_byte(4),
                hashes_added: 9,
                created_at: T_FLIP - 10,
            },
        ];
        assert_eq!(
            newest_ancestry_after(&evs, T_FLIP).unwrap().checkpoint,
            B256::repeat_byte(1)
        );
    }

    #[test]
    fn the_plan_follows_owner_anchors() {
        assert_eq!(
            plan(true, Some([1; 32]), Err(LcFailure::NoFlip)),
            Ok(AnchorPlan::Owner {
                lc_ready: false
            })
        );
        assert_eq!(
            plan(true, Some([1; 32]), Ok(())),
            Ok(AnchorPlan::Owner {
                lc_ready: true
            })
        );
        assert_eq!(
            plan(false, Some([1; 32]), Ok(())),
            Ok(AnchorPlan::LightClient)
        );
        assert_eq!(
            plan(false, Some([1; 32]), Err(LcFailure::NoFlip)),
            Err(LcFailure::NoFlip)
        );
        assert_eq!(plan(false, None, Ok(())), Err(LcFailure::NoLightClient));
    }

    #[test]
    fn both_freshness_bounds_are_inclusive() {
        let mut o = ready();
        let head_ts = o.head.as_ref().unwrap().timestamp;
        o.checkpoint.as_mut().unwrap().timestamp = head_ts - ANCESTRY_MAX_LAG_S;
        o.finalized_evm.as_mut().unwrap().timestamp = head_ts + HEAD_MAX_LAG_S;
        assert_eq!(judge(&o, 11_155_111), Ok(()));
        assert_eq!(
            (EPOCH_S, HEAD_MAX_LAG_S, ANCESTRY_MAX_LAG_S),
            (384, 1536, 768)
        );
    }

    #[test]
    fn every_failure_names_its_check() {
        let b = B256::repeat_byte(1);
        for (f, check) in [
            (
                LcFailure::Network {
                    lc: 1,
                    deposit: 8453,
                },
                "check 1",
            ),
            (
                LcFailure::HeadNotFound {
                    head: b,
                },
                "check 2",
            ),
            (
                LcFailure::KeyForm {
                    block: b,
                },
                "check 2",
            ),
            (LcFailure::NoFlip, "check 3"),
            (LcFailure::NoAncestryAfterFlip, "check 3"),
            (
                LcFailure::CheckpointNotFound {
                    checkpoint: b,
                },
                "check 3",
            ),
            (
                LcFailure::ParentNotAfterFlip {
                    parent_ts: 1,
                    t_flip: 2,
                },
                "check 3",
            ),
            (
                LcFailure::ParentNotAccepted {
                    parent: b,
                },
                "check 3",
            ),
            (
                LcFailure::HeadStale {
                    lag_s: 1537,
                },
                "check 3",
            ),
            (
                LcFailure::AncestryStale {
                    lag_s: 769,
                },
                "check 3",
            ),
        ] {
            assert!(f.to_string().contains(check), "{f}");
        }
        assert!(!LcFailure::NoLightClient.to_string().contains("check"));
    }

    #[test]
    fn the_status_line_says_who_is_awaited() {
        let b = B256::repeat_byte(0xab);
        let owner = AnchorPlan::Owner {
            lc_ready: false,
        }
        .status_line(b);
        assert!(
            owner.contains("bridge owner")
                && owner.contains("manual")
                && owner.contains(&format!("{b}"))
        );
        let either = AnchorPlan::Owner {
            lc_ready: true,
        }
        .status_line(b);
        assert!(
            either.contains("bridge owner or the light client"),
            "{either}"
        );
        let lc = AnchorPlan::LightClient.status_line(b);
        assert!(lc.contains("light client") && !lc.contains("owner"), "{lc}");
    }

    /// `_piForm` of `EthBeaconLightClient`, transcribed word for word:
    /// `(_rev16(h >> 128) << 128) | _rev16(h & (2^128 - 1))`, where `_rev16`
    /// reverses the low 16 bytes of its argument.
    fn contract_pi_form(h: B256) -> B256 {
        use alloy_primitives::U256;
        fn rev16(mut v: U256) -> U256 {
            let mut r = U256::ZERO;
            for _ in 0..16 {
                r = (r << 8) | (v & U256::from(0xff));
                v >>= 8;
            }
            r
        }
        let h = U256::from_be_bytes(h.0);
        let low = (U256::from(1) << 128) - U256::from(1);
        B256::from((rev16(h >> 128) << 128) | rev16(h & low))
    }

    #[test]
    fn pi_form_is_the_light_clients_key_packing() {
        let h = seq_hash(0x10);
        assert_ne!(B256::from(pi_form(h.0)), h, "a hash whose two forms differ");
        assert_eq!(B256::from(pi_form(h.0)), contract_pi_form(h));
    }

    #[test]
    fn the_ancestry_event_goes_to_external_address_704() {
        assert_eq!(ANCESTRY_ACCEPTED_DST, format!(":{:064x}", 704));
    }

    // ---- observe, on scripted chains ----

    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    use crate::deposit::{
        an::{AccountInfo, AnRead, ExtDir, MsgView, Page, TxListItem, TxRef, TxView},
        testkit::{msg, FakeAn, FakeEvm},
        ui::RecordingUi,
    };

    const BRIDGE: [u8; 32] = [0x1a; 32];
    const LC: [u8; 32] = [0x20; 32];
    const SEPOLIA: u64 = 11_155_111;

    /// A block hash whose two byte orders differ: bytes `tag, tag+1, …`.
    fn seq_hash(tag: u8) -> B256 {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = tag.wrapping_add(i as u8);
        }
        B256::from(b)
    }

    fn pi(h: B256) -> B256 {
        B256::from(pi_form(h.0))
    }

    /// The light client's head H, the checkpoint C of its newest ancestry
    /// and C's parent P, one epoch apart, all after the switch-off.
    struct Blocks {
        head: Header,
        checkpoint: Header,
        parent: Header,
    }

    fn blocks() -> Blocks {
        let head = Header {
            number: 1000,
            hash: seq_hash(0x10),
            parent_hash: seq_hash(0x0f),
            timestamp: T_FLIP + 5000,
        };
        let parent = Header {
            number: 967,
            hash: seq_hash(0x50),
            parent_hash: seq_hash(0x4f),
            timestamp: T_FLIP + 5000 - 396,
        };
        let checkpoint = Header {
            number: 968,
            hash: seq_hash(0x30),
            parent_hash: parent.hash,
            timestamp: T_FLIP + 5000 - 384,
        };
        Blocks {
            head,
            checkpoint,
            parent,
        }
    }

    /// An inbound `disableOwnerAnchors` whose transaction ran at `now`.
    fn switch_off(an: &FakeAn, hash: &str, now: u64, aborted: bool) {
        let bridge = format!("0:{}", hex::encode(BRIDGE));
        let m = msg(hash, "ExtIn", "", &bridge, Some("B-disable"));
        an.ext_in
            .lock()
            .unwrap()
            .entry(BRIDGE)
            .or_default()
            .push(m.clone());
        let mut full = m;
        full.dst_tx = Some(TxRef {
            hash: format!("{hash}-tx"),
            aborted,
            account: bridge,
            now,
        });
        an.messages.lock().unwrap().insert(hash.into(), full);
    }

    /// An `AncestryAccepted` event the light client emitted at `at`.
    fn ancestry(an: &FakeAn, hash: &str, checkpoint_raw: B256, added: u64, at: u64) {
        let body = format!("B-{hash}");
        let mut m = msg(
            hash,
            "ExtOut",
            &format!("0:{}", hex::encode(LC)),
            ANCESTRY_ACCEPTED_DST,
            Some(&body),
        );
        m.created_at = at;
        an.ext_out.lock().unwrap().entry(LC).or_default().push(m);
        an.bodies.lock().unwrap().insert(
            body,
            (
                "AncestryAccepted".into(),
                json!({ "checkpointHash": format!("{checkpoint_raw:#x}"), "hashesAdded": added.to_string() }),
            ),
        );
    }

    /// A light client that is ready, reporting its head and checkpoint the
    /// way `EthBeaconLightClient` 1.4.1 does: as stored keys, each 16-byte
    /// half reversed. The bridge accepts H and P in Ethereum order.
    fn ready_chains() -> (FakeEvm, FakeAn, Blocks) {
        let b = blocks();
        let evm = FakeEvm::sepolia();
        for h in [&b.head, &b.checkpoint, &b.parent] {
            evm.by_hash.lock().unwrap().insert(h.hash, h.clone());
        }
        evm.finalized.set([Some(Header {
            number: 1010,
            hash: seq_hash(0x70),
            parent_hash: seq_hash(0x6f),
            timestamp: b.head.timestamp + 120,
        })]);
        let an = FakeAn::default();
        // The SDK renders a uint256 as 0x-prefixed hex.
        an.getter(LC, "getConfig", vec![
            json!({ "l1ChainId": format!("0x{SEPOLIA:064x}") }),
        ]);
        an.getter(LC, "getHead", vec![
            json!({ "executionBlockHash": format!("{:#x}", pi(b.head.hash)) }),
        ]);
        an.accepted.lock().unwrap().insert(b.head.hash, 0);
        an.accepted.lock().unwrap().insert(b.parent.hash, 0);
        an.bodies.lock().unwrap().insert(
            "B-disable".into(),
            ("disableOwnerAnchors".into(), json!({})),
        );
        switch_off(&an, "flip", T_FLIP, false);
        ancestry(&an, "anc", pi(b.checkpoint.hash), 31, T_FLIP + 4800);
        (evm, an, b)
    }

    async fn observed(evm: &FakeEvm, an: &dyn AnRead) -> LcObs {
        observe(an, evm, BRIDGE, LC, SEPOLIA, &RecordingUi::new(true)).await
    }

    #[tokio::test]
    async fn a_light_client_that_stores_keys_in_pi_form_is_ready() {
        let (evm, an, b) = ready_chains();
        let o = observed(&evm, &an).await;
        assert_eq!(o.head_raw, pi(b.head.hash));
        assert_eq!(o.head.as_ref(), Some(&b.head));
        assert_eq!(o.checkpoint.as_ref(), Some(&b.checkpoint));
        assert_eq!(o.parent.as_ref(), Some(&b.parent));
        assert_eq!(o.t_flip, Some(T_FLIP));
        assert_eq!(judge(&o, SEPOLIA), Ok(()));
    }

    #[tokio::test]
    async fn a_head_reported_in_ethereum_order_is_found_as_well() {
        let (evm, an, b) = ready_chains();
        an.getter(LC, "getHead", vec![
            json!({ "executionBlockHash": format!("{:#x}", b.head.hash) }),
        ]);
        ancestry(&an, "anc2", b.checkpoint.hash, 31, T_FLIP + 4900);
        let o = observed(&evm, &an).await;
        assert_eq!(o.head_raw, b.head.hash);
        assert_eq!(o.head.as_ref(), Some(&b.head));
        assert_eq!(o.checkpoint.as_ref(), Some(&b.checkpoint));
        assert_eq!(judge(&o, SEPOLIA), Ok(()));
    }

    #[tokio::test]
    async fn a_bridge_that_accepts_only_the_stored_key_fails_check_2() {
        // The light client before 1.4.1 sent the bridge its stored key
        // itself, a word no deposit ever looks up.
        let (evm, an, b) = ready_chains();
        an.accepted.lock().unwrap().remove(&b.head.hash);
        an.accepted.lock().unwrap().insert(pi(b.head.hash), 0);
        let o = observed(&evm, &an).await;
        assert_eq!(o.bridge_accepts_head, Some(false));
        assert_eq!(
            judge(&o, SEPOLIA),
            Err(LcFailure::KeyForm {
                block: b.head.hash
            })
        );
        assert_eq!(o.t_flip, None, "stops at the first failed check");
    }

    #[tokio::test]
    async fn a_head_that_is_no_block_in_either_order_fails_check_2() {
        let (evm, an, _) = ready_chains();
        let nowhere = seq_hash(0x90);
        an.getter(LC, "getHead", vec![
            json!({ "executionBlockHash": format!("{nowhere:#x}") }),
        ]);
        let o = observed(&evm, &an).await;
        assert_eq!(
            judge(&o, SEPOLIA),
            Err(LcFailure::HeadNotFound {
                head: nowhere
            })
        );
        assert_eq!(
            o.bridge_accepts_head, None,
            "stops at the first failed check"
        );
    }

    #[tokio::test]
    async fn a_light_client_on_another_chain_is_not_read_further() {
        let (evm, an, _) = ready_chains();
        an.getter(LC, "getConfig", vec![json!({ "l1ChainId": "1" })]);
        let o = observed(&evm, &an).await;
        assert_eq!(
            judge(&o, SEPOLIA),
            Err(LcFailure::Network {
                lc: 1,
                deposit: SEPOLIA
            })
        );
        assert_eq!((o.head.clone(), o.head_raw), (None, B256::ZERO));
    }

    #[tokio::test]
    async fn an_aborted_switch_off_does_not_count() {
        let (evm, an, _) = ready_chains();
        // Newest first: a later switch-off whose transaction aborted, then
        // the real one.
        an.ext_in.lock().unwrap().get_mut(&BRIDGE).unwrap().clear();
        switch_off(&an, "flip-late", T_FLIP + 100, true);
        switch_off(&an, "flip", T_FLIP, false);
        let o = observed(&evm, &an).await;
        assert_eq!(o.t_flip, Some(T_FLIP));
        assert_eq!(judge(&o, SEPOLIA), Ok(()));
    }

    #[tokio::test]
    async fn only_aborted_switch_offs_mean_no_flip() {
        let (evm, an, _) = ready_chains();
        an.ext_in.lock().unwrap().get_mut(&BRIDGE).unwrap().clear();
        switch_off(&an, "flip", T_FLIP, true);
        let o = observed(&evm, &an).await;
        assert_eq!(judge(&o, SEPOLIA), Err(LcFailure::NoFlip));
    }

    #[tokio::test]
    async fn ancestry_from_before_the_switch_off_does_not_count() {
        let (evm, an, b) = ready_chains();
        an.ext_out.lock().unwrap().get_mut(&LC).unwrap().clear();
        ancestry(&an, "old", pi(b.checkpoint.hash), 31, T_FLIP);
        ancestry(&an, "empty", pi(b.checkpoint.hash), 0, T_FLIP + 4800);
        let o = observed(&evm, &an).await;
        assert_eq!(judge(&o, SEPOLIA), Err(LcFailure::NoAncestryAfterFlip));
    }

    #[tokio::test]
    async fn a_parent_the_bridge_did_not_accept_fails_check_3() {
        let (evm, an, b) = ready_chains();
        an.accepted.lock().unwrap().remove(&b.parent.hash);
        let o = observed(&evm, &an).await;
        assert_eq!(
            judge(&o, SEPOLIA),
            Err(LcFailure::ParentNotAccepted {
                parent: b.parent.hash
            })
        );
    }

    /// [`FakeAn`] with its external messages served `size` at a time,
    /// newest first, counting the pages read.
    struct Paged {
        inner: FakeAn,
        size: usize,
        pages: AtomicU32,
    }

    #[async_trait]
    impl AnRead for Paged {
        async fn rest_probe(&self, id: [u8; 32]) -> anyhow::Result<()> {
            self.inner.rest_probe(id).await
        }

        async fn account(&self, id: [u8; 32]) -> anyhow::Result<Option<AccountInfo>> {
            self.inner.account(id).await
        }

        async fn run_getter(
            &self,
            id: [u8; 32],
            abi: &str,
            f: &str,
            input: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner.run_getter(id, abi, f, input).await
        }

        async fn ext_messages(
            &self,
            id: [u8; 32],
            dir: ExtDir,
            before: Option<String>,
        ) -> anyhow::Result<Page<MsgView>> {
            self.pages.fetch_add(1, Ordering::SeqCst);
            let all = self.inner.ext_messages(id, dir, None).await?.items;
            let start: usize = before.map_or(0, |c| c.parse().unwrap());
            let end = (start + self.size).min(all.len());
            Ok(Page {
                items: all[start..end].to_vec(),
                cursor: (end < all.len()).then(|| end.to_string()),
            })
        }

        async fn transactions(
            &self,
            id: [u8; 32],
            after: Option<String>,
        ) -> anyhow::Result<Page<TxListItem>> {
            self.inner.transactions(id, after).await
        }

        async fn message(&self, h: &str) -> anyhow::Result<Option<MsgView>> {
            self.inner.message(h).await
        }

        async fn transaction(&self, h: &str) -> anyhow::Result<Option<TxView>> {
            self.inner.transaction(h).await
        }

        fn decode(
            &self,
            abi: &str,
            body: &str,
            internal: bool,
        ) -> Option<(String, serde_json::Value)> {
            self.inner.decode(abi, body, internal)
        }
    }

    #[tokio::test]
    async fn history_is_read_page_by_page_and_no_further_than_the_switch_off() {
        let (evm, an, b) = ready_chains();
        // The bridge: the switch-off under three newer inbound calls.
        let calls: Vec<MsgView> = (0..3)
            .map(|i| msg(&format!("fin{i}"), "ExtIn", "", "0:1a", Some("B-finalize")))
            .collect();
        an.ext_in
            .lock()
            .unwrap()
            .get_mut(&BRIDGE)
            .unwrap()
            .splice(0..0, calls);
        an.bodies
            .lock()
            .unwrap()
            .insert("B-finalize".into(), ("finalizeDeposit".into(), json!({})));
        // The light client: the newest ancestry under head updates, then
        // events older than the switch-off that must not be read.
        let mut out: Vec<MsgView> = (0..3)
            .map(|i| {
                let mut m = msg(&format!("head{i}"), "ExtOut", "0:20", ":01", Some("B-head"));
                m.created_at = T_FLIP + 4990 - i;
                m
            })
            .collect();
        out.extend(an.ext_out.lock().unwrap().remove(&LC).unwrap());
        for i in 0..6 {
            let mut m = msg(&format!("pre{i}"), "ExtOut", "0:20", ":01", Some("B-head"));
            m.created_at = T_FLIP - 1 - i;
            out.push(m);
        }
        an.ext_out.lock().unwrap().insert(LC, out);
        let paged = Paged {
            inner: an,
            size: 2,
            pages: AtomicU32::new(0),
        };
        let o = observed(&evm, &paged).await;
        assert_eq!(o.t_flip, Some(T_FLIP));
        assert_eq!(o.checkpoint.as_ref(), Some(&b.checkpoint));
        assert_eq!(judge(&o, SEPOLIA), Ok(()));
        // Bridge: 2 pages to reach the switch-off. Light client: the
        // ancestry is 4th and the first older event 5th, so 3 of its 5
        // pages.
        assert_eq!(paged.pages.load(Ordering::SeqCst), 2 + 3);
    }

    #[tokio::test]
    async fn the_finalized_head_is_read_for_freshness() {
        let (evm, an, b) = ready_chains();
        evm.finalized.set([Some(Header {
            number: 1200,
            hash: seq_hash(0x71),
            parent_hash: seq_hash(0x70),
            timestamp: b.head.timestamp + 1537,
        })]);
        let o = observed(&evm, &an).await;
        assert_eq!(
            judge(&o, SEPOLIA),
            Err(LcFailure::HeadStale {
                lag_s: 1537
            })
        );
    }

    /// Reads the shellnet light client and prints the form its head is
    /// stored in. The light client before 1.4.1 sent the bridge its stored
    /// key, and check 2 failed; 1.4.1 sends the Ethereum-order hash.
    #[tokio::test]
    #[ignore = "reads shellnet and Sepolia; run by hand"]
    async fn live_shellnet_light_client_shows_its_key_form() {
        let an =
            crate::deposit::an::LiveAn::connect("https://shellnet.ackinacki.org/graphql").unwrap();
        let evm =
            crate::deposit::evm::AlloyEvm::connect("https://ethereum-sepolia-rpc.publicnode.com")
                .unwrap();
        let ui = RecordingUi::new(true);
        let bridge = [0x1a; 32];
        let cfg = an
            .run_getter(
                bridge,
                crate::deposit::identity::BRIDGE_ABI,
                "getAnchorConfig",
                json!({}),
            )
            .await
            .unwrap();
        println!("getAnchorConfig: {cfg}");
        let (_, id) = cfg["lightClient"]
            .as_str()
            .unwrap()
            .split_once(':')
            .unwrap();
        let mut lc = [0u8; 32];
        hex::decode_to_slice(id, &mut lc).unwrap();
        let o = observe(&an, &evm, bridge, lc, SEPOLIA, &ui).await;
        assert_eq!(o.lc_chain_id, Some(SEPOLIA));
        let head = o
            .head
            .as_ref()
            .expect("the head is a Sepolia block in one of the two byte orders");
        let form = if o.head_raw == head.hash {
            "Ethereum byte order"
        } else {
            "stored-key form (each 16-byte half reversed)"
        };
        println!(
            "head: block {} {}, getHead returns it in {form} ({}); the bridge accepts {}: {:?}",
            head.number, head.hash, o.head_raw, head.hash, o.bridge_accepts_head
        );
        // A light client that sends the bridge its stored key leaves the
        // bridge accepting the raw form instead.
        if o.head_raw != head.hash {
            println!(
                "the bridge accepts the raw form {}: {:?}",
                o.head_raw,
                accepted(&an, bridge, SEPOLIA, o.head_raw).await
            );
        }
        let version = an
            .run_getter(lc, LIGHT_CLIENT_ABI, "getVersion", json!({}))
            .await;
        println!("light client getVersion: {version:?}");
        println!("observed: {o:?}");
        println!("verdict: {:?}", judge(&o, SEPOLIA));
    }
}
