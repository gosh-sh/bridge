//! Which bundle stride a relayer command runs at.
//!
//! Check B lets `last_seen` move four bundles between two reads, and the stall
//! warning scales with the stride, so a wrong stride halts the relayer on an
//! ordinary lost receipt: at L1's 1024 the bound is 4096, and one L2 bundle
//! is 16384 seq_nos. There is no default. The bridge reports its own stride
//! (`bundleStride()`); a bridge deployed before that field cannot, and its
//! anchor level has to be configured instead.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BundleStrideError {
    #[error(
        "the bridge has bundleStride={bridge}, but the configured anchor level gives stride \
         {configured}"
    )]
    Mismatch { bridge: u64, configured: u64 },
    #[error(
        "the bridge cannot report bundleStride() ({reason}) and no anchor level is configured"
    )]
    Unknown { reason: String },
    #[error(
        "the bridge cannot report bundleStride() ({reason}): it was deployed before the \
         bundle-aware rotation gate, or the RPC failed"
    )]
    PreBundleBridge { reason: String },
}

/// The stride `daemon`, `daemon-bridge` and `smoke-fixture` run at: the
/// bridge's own `bundleStride()`, or, for a bridge deployed before that field
/// (or reporting 0), the stride of the configured anchor level. When both are
/// known they must agree.
pub fn stride_for_submitting(
    bridge: Result<u64, String>,
    configured: Option<u64>,
) -> Result<u64, BundleStrideError> {
    match (bridge, configured) {
        (Ok(bridge), Some(configured)) if bridge != 0 && bridge != configured => {
            Err(BundleStrideError::Mismatch {
                bridge,
                configured,
            })
        },
        (Ok(bridge), _) if bridge != 0 => Ok(bridge),
        (_, Some(configured)) => Ok(configured),
        (Ok(_), None) => Err(BundleStrideError::Unknown {
            reason: "it reports 0".to_string(),
        }),
        (Err(reason), None) => Err(BundleStrideError::Unknown {
            reason,
        }),
    }
}

/// `daemon-live` applies rotations, so the bridge must report the stride of
/// the daemon's anchor level. A bridge deployed before `bundleStride()` still
/// applies the old rotation gate (`VerifyBlockLagBehindRotation`), which the
/// daemon does not follow: its rotations would revert until the daemon
/// aborts, so it is refused at startup instead.
pub fn check_stride_for_rotations(
    bridge: Result<u64, String>,
    daemon_stride: u64,
) -> Result<(), BundleStrideError> {
    match bridge {
        Ok(bridge) if bridge == daemon_stride => Ok(()),
        Ok(bridge) => Err(BundleStrideError::Mismatch {
            bridge,
            configured: daemon_stride,
        }),
        Err(reason) => Err(BundleStrideError::PreBundleBridge {
            reason,
        }),
    }
}

#[cfg(test)]
mod tests {
    use bridge_prover_lib::{BUNDLE_STRIDE_L1, BUNDLE_STRIDE_L2};

    use super::*;

    const OLD_BRIDGE: &str = "error code 3: execution reverted, data: \"0x\"";

    #[test]
    fn submitting_takes_the_bridge_stride() {
        assert_eq!(
            stride_for_submitting(Ok(BUNDLE_STRIDE_L2), None),
            Ok(BUNDLE_STRIDE_L2)
        );
        assert_eq!(
            stride_for_submitting(Ok(BUNDLE_STRIDE_L2), Some(BUNDLE_STRIDE_L2)),
            Ok(BUNDLE_STRIDE_L2)
        );
    }

    #[test]
    fn submitting_to_an_old_bridge_needs_the_anchor_level() {
        assert_eq!(
            stride_for_submitting(Err(OLD_BRIDGE.to_string()), Some(BUNDLE_STRIDE_L2)),
            Ok(BUNDLE_STRIDE_L2)
        );
        assert!(matches!(
            stride_for_submitting(Err(OLD_BRIDGE.to_string()), None),
            Err(BundleStrideError::Unknown { .. })
        ));
        assert_eq!(
            stride_for_submitting(Ok(0), Some(BUNDLE_STRIDE_L1)),
            Ok(BUNDLE_STRIDE_L1)
        );
        assert!(matches!(
            stride_for_submitting(Ok(0), None),
            Err(BundleStrideError::Unknown { .. })
        ));
    }

    #[test]
    fn submitting_refuses_a_level_the_bridge_contradicts() {
        assert_eq!(
            stride_for_submitting(Ok(BUNDLE_STRIDE_L2), Some(BUNDLE_STRIDE_L1)),
            Err(BundleStrideError::Mismatch {
                bridge: BUNDLE_STRIDE_L2,
                configured: BUNDLE_STRIDE_L1,
            })
        );
    }

    #[test]
    fn rotations_need_a_bundle_aware_bridge_at_the_daemon_level() {
        assert_eq!(
            check_stride_for_rotations(Ok(BUNDLE_STRIDE_L2), BUNDLE_STRIDE_L2),
            Ok(())
        );
        assert!(matches!(
            check_stride_for_rotations(Ok(BUNDLE_STRIDE_L1), BUNDLE_STRIDE_L2),
            Err(BundleStrideError::Mismatch { .. })
        ));
        assert!(matches!(
            check_stride_for_rotations(Err(OLD_BRIDGE.to_string()), BUNDLE_STRIDE_L2),
            Err(BundleStrideError::PreBundleBridge { .. })
        ));
    }
}
