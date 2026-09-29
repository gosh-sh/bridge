//! The deposit proof's twelve public inputs: 12 × 32 bytes, each a BN254
//! field element in little-endian order, in the order the circuit
//! exposes them. The same layout the relayer decodes; the fixture test
//! holds this copy to the prover's own output.

use alloy_primitives::{Address, B256, U256};

/// How many public inputs the circuit exposes.
pub const NUM_PUBLIC_INPUTS: usize = 12;
/// The encoded size of all public inputs.
pub const PUBLIC_INPUT_BYTES: usize = NUM_PUBLIC_INPUTS * 32;

/// The twelve public inputs, in circuit order; 256-bit values are split
/// into high and low 128-bit halves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepositPublicInputs {
    /// The deposit id from the `Deposit` event.
    pub deposit_id: U256,
    /// The depositor's EVM address.
    pub sender: U256,
    /// The amount in micro-USDC.
    pub amount: U256,
    /// The bridge contract address.
    pub contract: U256,
    /// The EVM chain id.
    pub chain_id: U256,
    /// High half of the dapp id.
    pub dapp_hi: U256,
    /// Low half of the dapp id.
    pub dapp_lo: U256,
    /// High half of the Acki Nacki account.
    pub account_hi: U256,
    /// Low half of the Acki Nacki account.
    pub account_lo: U256,
    /// High half of the block hash.
    pub block_hash_hi: U256,
    /// Low half of the block hash.
    pub block_hash_lo: U256,
    /// The promise commitment; zero for a plain proof.
    pub promise_commit: U256,
}

impl DepositPublicInputs {
    /// Parses exactly [`PUBLIC_INPUT_BYTES`] bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != PUBLIC_INPUT_BYTES {
            return Err(format!(
                "public inputs are {} bytes, expected {PUBLIC_INPUT_BYTES}",
                bytes.len()
            ));
        }
        let fr = |i: usize| {
            let mut le = [0u8; 32];
            le.copy_from_slice(&bytes[i * 32..(i + 1) * 32]);
            U256::from_le_bytes(le)
        };
        Ok(Self {
            deposit_id: fr(0),
            sender: fr(1),
            amount: fr(2),
            contract: fr(3),
            chain_id: fr(4),
            dapp_hi: fr(5),
            dapp_lo: fr(6),
            account_hi: fr(7),
            account_lo: fr(8),
            block_hash_hi: fr(9),
            block_hash_lo: fr(10),
            promise_commit: fr(11),
        })
    }

    /// The inverse of [`Self::decode`].
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PUBLIC_INPUT_BYTES);
        for fr in [
            self.deposit_id,
            self.sender,
            self.amount,
            self.contract,
            self.chain_id,
            self.dapp_hi,
            self.dapp_lo,
            self.account_hi,
            self.account_lo,
            self.block_hash_hi,
            self.block_hash_lo,
            self.promise_commit,
        ] {
            out.extend_from_slice(&fr.to_le_bytes::<32>());
        }
        out
    }

    /// The public inputs an honest prover would produce for `e`, with a
    /// zero promise commitment; lets driver tests fake a prover.
    pub fn from_expected(e: &ExpectedInputs) -> Self {
        let lo_mask = (U256::from(1u8) << 128) - U256::from(1u8);
        let split = |v: U256| (v >> 128, v & lo_mask);
        let (dapp_hi, dapp_lo) = split(U256::from_be_bytes(e.dapp_id));
        let (account_hi, account_lo) = split(U256::from_be_bytes(e.account_id));
        let (block_hash_hi, block_hash_lo) = split(U256::from_be_bytes(e.block_hash.0));
        Self {
            deposit_id: e.deposit_id,
            sender: U256::from_be_slice(e.sender.as_slice()),
            amount: U256::from(e.amount),
            contract: U256::from_be_slice(e.contract.as_slice()),
            chain_id: U256::from(e.chain_id),
            dapp_hi,
            dapp_lo,
            account_hi,
            account_lo,
            block_hash_hi,
            block_hash_lo,
            promise_commit: U256::ZERO,
        }
    }

    /// The Acki Nacki account id, halves joined.
    pub fn an_account(&self) -> U256 {
        (self.account_hi << 128) | self.account_lo
    }

    /// The dapp id, halves joined.
    pub fn dapp_id(&self) -> U256 {
        (self.dapp_hi << 128) | self.dapp_lo
    }

    /// The block hash, halves joined.
    pub fn block_hash(&self) -> B256 {
        B256::from((self.block_hash_hi << 128) | self.block_hash_lo)
    }
}

/// What the deposit says the public inputs must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedInputs {
    /// The deposit id from the `Deposit` event.
    pub deposit_id: U256,
    /// The depositor's EVM address.
    pub sender: Address,
    /// The amount in micro-USDC.
    pub amount: u64,
    /// The bridge contract address.
    pub contract: Address,
    /// The EVM chain id.
    pub chain_id: u64,
    /// The dapp id, big-endian.
    pub dapp_id: [u8; 32],
    /// The Acki Nacki account id, big-endian.
    pub account_id: [u8; 32],
    /// The hash of the block that holds the deposit.
    pub block_hash: B256,
}

/// The first public input that disagrees with the deposit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiMismatch {
    /// The field's name, as the contract's event names it.
    pub field: &'static str,
    /// The value in the proof, hex.
    pub got: String,
    /// The value the deposit has, hex.
    pub want: String,
}

impl std::fmt::Display for PiMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "public input {} is {}, the deposit has {}",
            self.field, self.got, self.want
        )
    }
}

/// An address as the field element the circuit exposes.
fn addr_as_u256(a: Address) -> U256 {
    U256::from_be_slice(a.as_slice())
}

/// Checks the eight deposit-bound inputs against `want`; the first
/// disagreement is named.
pub fn verify(pi: &DepositPublicInputs, want: &ExpectedInputs) -> Result<(), PiMismatch> {
    let checks: [(&'static str, U256, U256); 8] = [
        ("depositId", pi.deposit_id, want.deposit_id),
        ("sender", pi.sender, addr_as_u256(want.sender)),
        ("amount", pi.amount, U256::from(want.amount)),
        ("contract", pi.contract, addr_as_u256(want.contract)),
        ("chainId", pi.chain_id, U256::from(want.chain_id)),
        ("dappId", pi.dapp_id(), U256::from_be_bytes(want.dapp_id)),
        (
            "anAccount",
            pi.an_account(),
            U256::from_be_bytes(want.account_id),
        ),
        (
            "blockHash",
            U256::from_be_bytes(pi.block_hash().0),
            U256::from_be_bytes(want.block_hash.0),
        ),
    ];
    for (field, got, exp) in checks {
        if got != exp {
            return Err(PiMismatch {
                field,
                got: format!("{got:#x}"),
                want: format!("{exp:#x}"),
            });
        }
    }
    Ok(())
}

/// The `proof_00` fixture and what it must decode to, for this module's
/// tests and for the prover and driver tests that fake a prover.
#[cfg(test)]
pub mod tests_support {
    use alloy_primitives::{keccak256, Address, U256};

    use super::ExpectedInputs;

    /// The fixture's public inputs, as the prover wrote them.
    pub const PI: &[u8] = include_bytes!(
        "../../../../deposit-prover/fixtures/deposit_10proofs/proof_00/public_inputs.bin"
    );
    /// The witness the fixture proof was built from.
    const INPUT: &str =
        include_str!("../../../../deposit-prover/fixtures/deposit_10proofs/proof_00/input.json");

    /// A fresh copy of the fixture's public inputs.
    pub fn fixture_pi() -> Vec<u8> {
        PI.to_vec()
    }

    /// The expected values come from the witness, not from this module:
    /// the event fields as the fetcher recorded them, and the block hash as
    /// keccak of the header it proved against.
    pub fn expected_from_witness() -> ExpectedInputs {
        let v: serde_json::Value = serde_json::from_str(INPUT).unwrap();
        let ev = &v["event_data"];
        let bytes = |x: &serde_json::Value| -> Vec<u8> {
            x.as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_u64().unwrap() as u8)
                .collect()
        };
        let header = bytes(&v["receipt_proof"]["block_header_rlp"]);
        let mut account = [0u8; 32];
        account.copy_from_slice(&bytes(&ev["an_account"]));
        ExpectedInputs {
            deposit_id: U256::from(ev["deposit_id"].as_u64().unwrap()),
            sender: Address::from_slice(&bytes(&ev["sender"])),
            amount: U256::from_be_slice(&bytes(&ev["amount"])).to::<u64>(),
            contract: Address::from_slice(&bytes(&ev["contract_address"])),
            chain_id: ev["chain_id"].as_u64().unwrap(),
            dapp_id: [0u8; 32],
            account_id: account,
            block_hash: keccak256(&header),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256};

    use super::{tests_support::*, *};

    #[test]
    fn the_fixture_decodes_to_its_own_witness() {
        let pi = DepositPublicInputs::decode(PI).unwrap();
        verify(&pi, &expected_from_witness()).unwrap();
        assert_eq!(pi.amount, U256::from(1_000_000u64));
        assert_eq!(pi.chain_id, U256::from(11_155_111u64));
    }

    #[test]
    fn encode_is_the_inverse_of_decode() {
        let pi = DepositPublicInputs::decode(PI).unwrap();
        assert_eq!(pi.encode(), PI);
        let e = expected_from_witness();
        verify(&DepositPublicInputs::from_expected(&e), &e).unwrap();
    }

    #[test]
    fn a_wrong_length_is_refused() {
        assert!(DepositPublicInputs::decode(&PI[..383]).is_err());
        let mut long = PI.to_vec();
        long.push(0);
        assert!(DepositPublicInputs::decode(&long).is_err());
    }

    #[test]
    fn each_field_is_named_when_it_disagrees() {
        let pi = DepositPublicInputs::decode(PI).unwrap();
        let base = expected_from_witness();
        let cases: Vec<(&str, ExpectedInputs)> = vec![
            ("depositId", ExpectedInputs {
                deposit_id: U256::from(1),
                ..base.clone()
            }),
            ("sender", ExpectedInputs {
                sender: Address::ZERO,
                ..base.clone()
            }),
            ("amount", ExpectedInputs {
                amount: 2,
                ..base.clone()
            }),
            ("contract", ExpectedInputs {
                contract: Address::ZERO,
                ..base.clone()
            }),
            ("chainId", ExpectedInputs {
                chain_id: 1,
                ..base.clone()
            }),
            ("dappId", ExpectedInputs {
                dapp_id: [1u8; 32],
                ..base.clone()
            }),
            ("anAccount", ExpectedInputs {
                account_id: [1u8; 32],
                ..base.clone()
            }),
            ("blockHash", ExpectedInputs {
                block_hash: B256::ZERO,
                ..base.clone()
            }),
        ];
        for (field, want) in cases {
            assert_eq!(verify(&pi, &want).unwrap_err().field, field);
        }
    }
}
