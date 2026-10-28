//! Prover ↔ contract parity of the chain anchor (bridge-audit#122).
//!
//! The prover picks `prevMaxLevelLayerHash` with
//! `BridgeState::prev_max_level_layer_hash_for`; the contract requires
//! `expectedPrevAnchor(numLayers)`. Both take the latest hash of layer
//! `min(numLayers, t)`, so they agree only while their `t` agrees — and the
//! contract starts from empty windows while the prover starts from the seed.
//!
//! This test bootstraps a `BridgeState` from a seed block, appends the
//! scenario's bundles and compares every anchor, for `n` in `1..=10`, with
//! `contracts/ethereum/test/fixtures/anchor_parity.json`.
//! `AckiNackiBridgeAnchorParity.t.sol` replays the same bundles on the
//! contract and compares `expectedPrevAnchor(n)` with the same file, so a
//! change on either side that breaks parity fails one of the two.
//!
//! It lives in the relayer, which submits the prover's anchor, so that
//! `make relayer-test` (part of `make pre-push`) runs it. After an intended
//! change of the prover's pick, regenerate the file from
//! `crates/bridge-prover-libraries` with `ANCHOR_PARITY_WRITE=1 cargo test
//! --locked -p bridge-relayer-daemon --test anchor_parity` and check that the
//! forge test still passes.

use std::{collections::BTreeMap, path::PathBuf};

use bridge_prover_lib::{
    bootstrap::{BootstrapSeed, SEED_SCHEMA_VERSION},
    bridge_state::{BridgeState, MAX_LAYERS},
    AnchorMode,
};
use serde::{Deserialize, Serialize};

const W: u64 = 128;
const W2: u64 = W * W;
const W3: u64 = W2 * W;
const W4: u64 = W3 * W;
const L1_STRIDE: u64 = AnchorMode::L1.stride();

struct Scenario {
    name: &'static str,
    anchor_level: u8,
    seed_seq_no: u64,
    /// The seed block carries layers `1..=seed_layers`.
    seed_layers: u8,
    /// `(seq_no, num_layers)` of each bundle after the seed.
    bundles: &'static [(u64, u8)],
}

const SCENARIOS: &[Scenario] = &[
    // The case of bridge-audit#122: the seed sits on a W³ boundary, the
    // contract only learns layer 3 at the next one.
    Scenario {
        name: "l2_seed_on_w3_boundary",
        anchor_level: 2,
        seed_seq_no: 5 * W3,
        seed_layers: 3,
        bundles: &[
            (5 * W3 + W2, 2),
            (5 * W3 + 2 * W2, 2),
            (6 * W3, 3),
            (6 * W3 + W2, 2),
        ],
    },
    Scenario {
        name: "l2_seed_on_w4_boundary",
        anchor_level: 2,
        seed_seq_no: 2 * W4,
        seed_layers: 4,
        bundles: &[
            (2 * W4 + W2, 2),
            (2 * W4 + W3, 3),
            (3 * W4, 4),
            (3 * W4 + W2, 2),
        ],
    },
    Scenario {
        name: "l2_seed_on_w2_boundary",
        anchor_level: 2,
        seed_seq_no: 5 * W3 + 3 * W2,
        seed_layers: 2,
        bundles: &[(5 * W3 + 4 * W2, 2), (6 * W3, 3), (6 * W3 + W2, 2)],
    },
    // The first bundle after the seed is already a W³ boundary: the contract
    // checks it against the genesis anchor.
    Scenario {
        name: "l2_first_bundle_on_w3_boundary",
        anchor_level: 2,
        seed_seq_no: 6 * W3 - W2,
        seed_layers: 2,
        bundles: &[(6 * W3, 3), (6 * W3 + W2, 2)],
    },
    // L1 row of bridge-audit#122: the seed sits on a W² boundary.
    Scenario {
        name: "l1_seed_on_w2_boundary",
        anchor_level: 1,
        seed_seq_no: 3 * W2,
        seed_layers: 2,
        bundles: &[
            (3 * W2 + L1_STRIDE, 1),
            (3 * W2 + 2 * L1_STRIDE, 1),
            (4 * W2, 2),
            (4 * W2 + L1_STRIDE, 1),
        ],
    },
    Scenario {
        name: "l1_seed_off_w2_boundary",
        anchor_level: 1,
        seed_seq_no: 3 * W2 + L1_STRIDE,
        seed_layers: 1,
        bundles: &[
            (3 * W2 + 2 * L1_STRIDE, 1),
            (4 * W2, 2),
            (4 * W2 + L1_STRIDE, 1),
        ],
    },
];

/// One scenario as both test suites read it. Hashes are `0x`-prefixed
/// 32-byte big-endian hex — the `uint256` the contract stores, i.e. the
/// prover's little-endian bytes reversed.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ScenarioFixture {
    anchor_level: u8,
    seed_seq_no: u64,
    /// Every layer root of the seed block, layer `i + 1` at index `i`.
    seed_layer_hashes: Vec<String>,
    /// The contract's constructor `genesisPrevMaxLevelLayerHash`: the seed's
    /// layer-`anchor_level` root.
    genesis_anchor: String,
    /// `prev_max_level_layer_hash_for(n)` after the seed, `n = index + 1`.
    /// Below `anchor_level` the contract has no counterpart: it knows the
    /// seed only through `genesis_anchor`, and no bundle under that level
    /// carries fewer than `anchor_level` layers.
    seed_anchors: Vec<String>,
    bundle_count: usize,
    bundles: Vec<BundleFixture>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct BundleFixture {
    seq_no: u64,
    /// Layer roots of the bundle's key block, layer `i + 1` at index `i`.
    layer_hashes: Vec<String>,
    /// The anchor the prover proves this bundle from:
    /// `prev_max_level_layer_hash_for(layer_hashes.len())` before it.
    prev_anchor: String,
    /// `prev_max_level_layer_hash_for(n)` after the bundle, `n = index + 1`.
    anchors_after: Vec<String>,
}

/// A distinct, canonical-field root per block and layer: `0x<step><layer>`.
fn root(step: u64, layer: u8) -> [u8; 32] {
    let mut le = [0u8; 32];
    le[..8].copy_from_slice(&((step << 8) | u64::from(layer)).to_le_bytes());
    le
}

fn be_hex(le: [u8; 32]) -> String {
    let mut be = le;
    be.reverse();
    format!("0x{}", hex::encode(be))
}

fn anchors(state: &BridgeState) -> Vec<String> {
    (1..=MAX_LAYERS)
        .map(|n| be_hex(state.prev_max_level_layer_hash_for(n)))
        .collect()
}

fn layers(step: u64, num_layers: u8) -> Vec<([u8; 32], u8)> {
    (1..=num_layers)
        .map(|layer| (root(step, layer), layer))
        .collect()
}

fn run(scenario: &Scenario) -> ScenarioFixture {
    let seed_layers = layers(1, scenario.seed_layers);
    let seed = BootstrapSeed {
        schema_version: SEED_SCHEMA_VERSION,
        layer_hashes: seed_layers.clone(),
        block_height: scenario.seed_seq_no,
        block_seq_no: scenario.seed_seq_no,
        bk_set_commitment: [7u8; 32],
        anchor_level: scenario.anchor_level,
        last_bk_set_update_seq_no: 0,
    };
    let mut state = BridgeState::new(W as usize);
    seed.apply(&mut state).unwrap();
    let seed_anchors = anchors(&state);

    let mut bundles = Vec::with_capacity(scenario.bundles.len());
    for (i, &(seq_no, num_layers)) in scenario.bundles.iter().enumerate() {
        let per_layer = layers(i as u64 + 2, num_layers);
        let prev_anchor = be_hex(state.prev_max_level_layer_hash_for(usize::from(num_layers)));
        state.append_bundle(&per_layer, seq_no, seq_no).unwrap();
        bundles.push(BundleFixture {
            seq_no,
            layer_hashes: per_layer.iter().map(|(h, _)| be_hex(*h)).collect(),
            prev_anchor,
            anchors_after: anchors(&state),
        });
    }

    ScenarioFixture {
        anchor_level: scenario.anchor_level,
        seed_seq_no: scenario.seed_seq_no,
        seed_layer_hashes: seed_layers.iter().map(|(h, _)| be_hex(*h)).collect(),
        genesis_anchor: be_hex(root(1, scenario.anchor_level)),
        seed_anchors,
        bundle_count: bundles.len(),
        bundles,
    }
}

/// The crate is a symlinked member of the prover workspace, so the manifest
/// directory is canonicalized before walking up to the repository root.
fn fixture_path() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))
        .unwrap()
        .join("../../contracts/ethereum/test/fixtures/anchor_parity.json")
}

#[test]
fn prover_anchors_match_the_contract_fixture() {
    let computed: BTreeMap<String, ScenarioFixture> = SCENARIOS
        .iter()
        .map(|s| (s.name.to_string(), run(s)))
        .collect();
    let path = fixture_path();

    if std::env::var_os("ANCHOR_PARITY_WRITE").is_some() {
        let json = serde_json::to_string_pretty(&computed).unwrap() + "\n";
        std::fs::write(&path, json).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        return;
    }

    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}. Generate it with ANCHOR_PARITY_WRITE=1 cargo test --locked -p \
             bridge-relayer-daemon --test anchor_parity",
            path.display()
        )
    });
    let stored: BTreeMap<String, ScenarioFixture> = serde_json::from_str(&data).unwrap();
    assert_eq!(
        stored.keys().collect::<Vec<_>>(),
        computed.keys().collect::<Vec<_>>(),
        "scenario set differs from {}",
        path.display()
    );
    let hint = "the contract test checks expectedPrevAnchor against this file";
    for (name, expected) in &stored {
        let got = &computed[name];
        assert_eq!(
            got.seed_anchors, expected.seed_anchors,
            "{name} after the seed: {hint}"
        );
        for (i, (g, e)) in got.bundles.iter().zip(&expected.bundles).enumerate() {
            assert_eq!(
                g.prev_anchor, e.prev_anchor,
                "{name} bundle {i} prev_anchor: {hint}"
            );
            assert_eq!(
                g.anchors_after, e.anchors_after,
                "{name} bundle {i} anchors_after: {hint}"
            );
        }
        assert_eq!(got, expected, "{name}: {hint}");
    }
}

/// The probe from bridge-audit#122: an L2 seed on a W³ boundary. After the
/// first bundle the contract holds layers 1 and 2 only, so `t = 2` and a
/// three-layer block is anchored at the latest layer-2 root.
#[test]
fn a_seed_of_order_3_under_anchor_level_2_keeps_t_at_2() {
    let scenario = &SCENARIOS[0];
    assert_eq!(scenario.name, "l2_seed_on_w3_boundary");
    let fixture = run(scenario);
    let first = &fixture.bundles[0];
    assert_eq!(fixture.seed_anchors[2], fixture.genesis_anchor);
    assert_eq!(first.anchors_after[2], first.layer_hashes[1]);
}
