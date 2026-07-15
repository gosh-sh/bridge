//! TVM Bag-of-Cells flattening.
//!
//! Byte-for-byte port of `bridge-event-prove-circuit::boc_helper`. We keep
//! a local copy because the prover and circuits workspaces pin **different**
//! git sources for `tvm-sdk` (different URLs / branches / commits). Cargo
//! treats same-version crates from different git sources as *distinct*
//! types, so calling the upstream `serialize_cells_tree_root_first` with a
//! `Cell` constructed by *our* `tvm_types` fails to type-check
//! (`expected ... found a different tvm_types::cell::Cell`). The local copy
//! sidesteps that by living inside this crate and only ever seeing our
//! `tvm_types`.
//!
//! When the two workspaces converge on a single `tvm-sdk` pin, this module
//! can be deleted and callers switched to
//! `bridge_event_prove_circuit::boc_helper::{serialize_cells_tree_root_first,
//! BocFlattenData}`.

use std::collections::{HashSet, VecDeque};

use tvm_types::cell::{DEPTH_SIZE, SHA256_SIZE};
use tvm_types::{Cell, CellType, LevelMask};

/// Flat representation of a single cell from a serialized BOC tree.
/// Field-for-field equivalent of
/// `bridge-event-prove-circuit::boc_helper::BocFlattenData`.
#[derive(Debug, Clone)]
pub struct FlatCell {
    pub repr_hash: [u8; 32],
    pub refs_count: u8,
    pub childs_repr_hashes_offset: Option<Vec<u16>>,
    pub cell_repr_data: Vec<u8>,
}

/// Build the SHA-256 preimage (`cell_repr_data`) whose hash equals the cell's
/// `repr_hash`. Layout (ordinary / exotic non-big cells):
///   `d1 || d2 || data_or_prev_hash || child_depths || child_hashes`
pub fn build_cell_repr_data(cell: &Cell) -> tvm_types::Result<Vec<u8>> {
    let cell_type = cell.cell_type();

    if cell_type == CellType::Big {
        return Ok(cell.data().to_vec());
    }

    let bit_len = cell.bit_length();
    let refs_count = cell.references_count();
    let is_merkle = cell_type == CellType::MerkleProof || cell_type == CellType::MerkleUpdate;
    let is_pruned = cell_type == CellType::PrunedBranch;
    let mask = cell.level_mask().mask();

    let repr_i: usize = if is_pruned || mask == 0 {
        0
    } else {
        8 - mask.leading_zeros() as usize
    };

    let hash_level_mask = if is_pruned {
        cell.level_mask()
    } else {
        LevelMask::with_level(repr_i as u8)
    };
    let d1 = (hash_level_mask.mask() << 5)
        | ((cell_type != CellType::Ordinary) as u8 * 8)
        | refs_count as u8;

    let d2 = ((bit_len / 8) << 1) as u8 + (bit_len % 8 != 0) as u8;

    let data_part_len = if repr_i == 0 {
        (bit_len / 8) + usize::from(bit_len % 8 != 0)
    } else {
        SHA256_SIZE
    };
    let total = 2 + data_part_len + refs_count * (DEPTH_SIZE + SHA256_SIZE);
    let mut repr_data = Vec::with_capacity(total);

    repr_data.push(d1);
    repr_data.push(d2);

    if repr_i == 0 {
        let data_size = (bit_len / 8) + usize::from(bit_len % 8 != 0);
        repr_data.extend_from_slice(&cell.data()[..data_size]);
    } else {
        let prev_hash = cell.hash(repr_i - 1);
        repr_data.extend_from_slice(prev_hash.as_slice());
    }

    let child_level = repr_i + is_merkle as usize;
    for i in 0..refs_count {
        let child = cell.reference(i)?;
        repr_data.extend_from_slice(&child.depth(child_level).to_be_bytes());
    }

    for i in 0..refs_count {
        let child = cell.reference(i)?;
        repr_data.extend_from_slice(child.hash(child_level).as_slice());
    }

    Ok(repr_data)
}

/// BFS walk of the cell DAG rooted at `root`, dedup by `repr_hash`, children
/// sorted by ascending `refs_count`. Same traversal order
/// `bridge-event-prove-circuit::test_helpers::parse_withdrawal_boc` expects.
pub fn serialize_cells_tree_root_first(root: &Cell) -> tvm_types::Result<Vec<FlatCell>> {
    let mut visited = HashSet::new();
    let mut result: Vec<FlatCell> = Vec::new();
    let mut queue = VecDeque::new();

    visited.insert(root.repr_hash());
    queue.push_back(root.clone());

    while let Some(cell) = queue.pop_front() {
        let hash = cell.repr_hash();
        let repr_data = build_cell_repr_data(&cell)?;
        let refs_count = cell.references_count();

        let child_hashes_start = repr_data.len() - refs_count * SHA256_SIZE;
        let childs_repr_hashes_offset = if refs_count == 0 {
            None
        } else {
            Some(
                (0..refs_count)
                    .map(|i| (child_hashes_start + i * SHA256_SIZE) as u16)
                    .collect(),
            )
        };

        let mut repr_hash = [0u8; 32];
        repr_hash.copy_from_slice(hash.as_slice());

        result.push(FlatCell {
            repr_hash,
            refs_count: refs_count as u8,
            childs_repr_hashes_offset,
            cell_repr_data: repr_data,
        });

        let mut children = Vec::with_capacity(refs_count);
        for i in 0..refs_count {
            children.push(cell.reference(i)?);
        }
        children.sort_by_key(|c| c.references_count());

        for child in children {
            if visited.insert(child.repr_hash()) {
                queue.push_back(child);
            }
        }
    }

    Ok(result)
}
