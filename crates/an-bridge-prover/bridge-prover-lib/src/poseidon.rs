// Re-export from bridge-poseidon (single source of truth).
pub use bridge_poseidon::{
    compute_bk_set_poseidon, decompose_pubkey_x_to_limbs, poseidon_hash_bytes, poseidon_hash_fr,
    poseidon_hash_fr_to_bytes, LIMB_BITS, MAX_SIGNERS, NUM_LIMBS, PADDING_SIGNER_INDEX,
    POSEIDON_R_F, POSEIDON_R_P, POSEIDON_RATE, POSEIDON_T,
};
