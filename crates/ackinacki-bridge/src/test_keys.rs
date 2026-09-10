//! One pinned ed25519 key pair, shared by every test module that needs a
//! keys.json the SDK will actually accept.
//!
//! Not duplicated per module on purpose: `KeyPair::decode` derives the
//! public half from the secret and compares, so two copies that drift do
//! not fail visibly — they fail inside `encode_message`, in the one error
//! whose text this crate deliberately withholds (F18).
//!
//! Generated once with `tvm-cli genphrase --dump`. It has never held a
//! balance and never will: it exists so the tests can exercise the real
//! signing path offline.
#![cfg(test)]

/// A throwaway ed25519 public key for the fixtures. Never a real one:
/// its secret is in the line below it.
pub const PAIR_PUBLIC: &str = "e842bd24792748fffffe700face504a5abb0b7c82f61ed285f18dac26ec1bc7f";
/// Its secret half, in the source tree on purpose: the tests need a
/// pair that signs, and a real one may never be one.
pub const PAIR_SECRET: &str = "5ed412cc5c2a7abc91a11dc6bbb929cba13e4f1c4d5bc36e91ea17e21363f52e";
