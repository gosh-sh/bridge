//! Deposits the circuit cannot prove, found before any key or proof work.
//!
//! Without these checks an oversized receipt panics in
//! `mock_fulfill_keccak_promises` and an oversized log fails only the
//! local verify after a full prove, both indistinguishable from a passing
//! failure. A caller that sees [`UNPROVABLE_EXIT_CODE`] must not retry:
//! the same deposit gives the same answer every time.

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Mutex},
};

use axiom_eth::utils::{
    component::promise_loader::single::PromiseLoaderParams, eth_circuit::EthCircuitImpl,
};
use halo2_base::{gates::circuit::CircuitBuilderStage, halo2_proofs::halo2curves::bn256::Fr};

use crate::{
    circuit_v2::{
        receipt_max_byte_len, DepositEventCircuitV2, MAX_BLOCK_HEADER_BYTES, RECEIPT_PF_MAX_DEPTH,
        TX_PF_MAX_DEPTH,
    },
    mpt::reject_unprovable_enclosing_tx,
    prover::{get_default_params, CircuitConfig, FIXED_KECCAK_CAPACITY},
    rlp_utils::{rlp_list_items, RlpItem},
    types::DepositProofInput,
};

/// The exit code of the prover tools for a deposit the circuit cannot
/// prove. Callers match on it: the CLI ends the operation at its exit 35,
/// and the relayer parks the deposit at once.
pub const UNPROVABLE_EXIT_CODE: i32 = 3;

/// Why the circuit cannot prove a deposit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unprovable(pub String);

impl std::fmt::Display for Unprovable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unprovable {}

macro_rules! unprovable {
    ($($arg:tt)*) => {
        Unprovable(format!($($arg)*))
    };
}

/// Every check below: the enclosing transaction, the header, both MPT
/// paths, the receipt's logs, and last the keccak budget, which needs the
/// circuit's own witness generation.
pub fn check_provable(input: &DepositProofInput, config: &CircuitConfig) -> Result<(), Unprovable> {
    check_witness_bounds(input, config)?;
    check_keccak_budget(input, config)
}

/// [`check_provable`]; on a refusal, the reason goes to stderr and the
/// process exits with [`UNPROVABLE_EXIT_CODE`].
pub fn exit_if_unprovable(input: &DepositProofInput, config: &CircuitConfig) {
    if let Err(e) = check_provable(input, config) {
        eprintln!("unprovable: {e}");
        std::process::exit(UNPROVABLE_EXIT_CODE);
    }
}

/// The bounds that need nothing but the witness bytes.
pub fn check_witness_bounds(
    input: &DepositProofInput,
    config: &CircuitConfig,
) -> Result<(), Unprovable> {
    reject_unprovable_enclosing_tx(&input.tx_proof.tx_bytes).map_err(|e| unprovable!("{e:#}"))?;
    let header = input.receipt_proof.block_header_rlp.len();
    if header > MAX_BLOCK_HEADER_BYTES {
        return Err(unprovable!(
            "the block header is {header} bytes; the circuit reads at most \
             {MAX_BLOCK_HEADER_BYTES}"
        ));
    }
    let receipt_depth = input.receipt_proof.proof_nodes.len();
    if receipt_depth > RECEIPT_PF_MAX_DEPTH {
        return Err(unprovable!(
            "the receipt MPT path has {receipt_depth} nodes; the circuit takes at most \
             {RECEIPT_PF_MAX_DEPTH}"
        ));
    }
    let tx_depth = input.tx_proof.proof_nodes.len();
    if tx_depth > TX_PF_MAX_DEPTH {
        return Err(unprovable!(
            "the transaction MPT path has {tx_depth} nodes; the circuit takes at most \
             {TX_PF_MAX_DEPTH}"
        ));
    }
    check_receipt(&input.receipt_proof.receipt_rlp, config)
}

/// The receipt fits the circuit's receipt leaf: at most `max_log_num`
/// logs, each with at most `topic_num_bounds.1` topics and
/// `max_data_byte_len` data bytes. Every log counts, not only `Deposit`.
pub fn check_receipt(receipt_rlp: &[u8], config: &CircuitConfig) -> Result<(), Unprovable> {
    let max_len = receipt_max_byte_len(config.max_data_byte_len, config.max_log_num);
    if receipt_rlp.len() > max_len {
        return Err(unprovable!(
            "the receipt is {} bytes; the circuit's receipt leaf holds at most {max_len}",
            receipt_rlp.len()
        ));
    }
    // A typed receipt starts with its type byte; a legacy one is the list.
    let list = match receipt_rlp.first() {
        Some(&t) if t < 0x80 => &receipt_rlp[1..],
        _ => receipt_rlp,
    };
    let malformed = |e: anyhow::Error| unprovable!("the receipt is not well-formed RLP: {e:#}");
    let fields = rlp_list_items(list).map_err(malformed)?;
    let logs = fields
        .get(3)
        .ok_or_else(|| unprovable!("the receipt has {} fields, no logs", fields.len()))?;
    let logs = rlp_list_items(logs.span).map_err(malformed)?;
    if logs.len() > config.max_log_num {
        return Err(unprovable!(
            "the receipt has {} logs; the circuit parses at most {}",
            logs.len(),
            config.max_log_num
        ));
    }
    for (i, log) in logs.iter().enumerate() {
        let (topics, data) = log_parts(log).map_err(|e| unprovable!("log {i}: {e:#}"))?;
        if topics > config.topic_num_bounds.1 {
            return Err(unprovable!(
                "log {i} has {topics} topics; the circuit takes at most {}",
                config.topic_num_bounds.1
            ));
        }
        if data > config.max_data_byte_len {
            return Err(unprovable!(
                "log {i} has {data} data bytes; the circuit reads at most {} per log",
                config.max_data_byte_len
            ));
        }
    }
    Ok(())
}

/// A log's topic count and data length.
fn log_parts(log: &RlpItem<'_>) -> anyhow::Result<(usize, usize)> {
    let parts = rlp_list_items(log.span)?;
    let [_, topics, data] = parts.as_slice() else {
        anyhow::bail!("a log has {} fields, not 3", parts.len());
    };
    Ok((rlp_list_items(topics.span)?.len(), data.payload.len()))
}

/// The keccak preimages of the header, both MPT paths, the tx leaf and the
/// receipt fit [`FIXED_KECCAK_CAPACITY`] permutations. The count comes
/// from the circuit's own promise collection, so it cannot drift from
/// what proving would ask for. Witness generation only; no SRS, no key.
pub fn check_keccak_budget(
    input: &DepositProofInput,
    config: &CircuitConfig,
) -> Result<(), Unprovable> {
    let circuit = EthCircuitImpl::<Fr, _>::new_impl(
        CircuitBuilderStage::Mock,
        DepositEventCircuitV2::new(input.clone(), config),
        get_default_params(),
        PromiseLoaderParams::new_for_one_shard(FIXED_KECCAK_CAPACITY),
    );
    let over_budget = catching_panic(KECCAK_CAPACITY_ASSERT, || {
        circuit.mock_fulfill_keccak_promises(Some(FIXED_KECCAK_CAPACITY))
    });
    if over_budget {
        return Err(unprovable!(
            "the header, MPT paths, transaction and receipt need more than \
             {FIXED_KECCAK_CAPACITY} keccak permutations (about {} KB of hashed bytes), the \
             circuit's fixed budget",
            FIXED_KECCAK_CAPACITY * 136 / 1000
        ));
    }
    Ok(())
}

/// The assertion axiom-eth's `OutputKeccakShard::into_logical_results`
/// fails when the calls exceed the shard's capacity.
const KECCAK_CAPACITY_ASSERT: &str = "total_capacity <= self.capacity";

/// Runs `f`; true if it panicked with a message containing `marker`. That
/// panic is kept quiet; any other one is reported as usual and goes on.
fn catching_panic(marker: &'static str, f: impl FnOnce()) -> bool {
    let previous = Arc::new(Mutex::new(Some(panic::take_hook())));
    let hook = Arc::clone(&previous);
    panic::set_hook(Box::new(move |info| {
        let quiet = panic_text(info.payload()).is_some_and(|m| m.contains(marker));
        if !quiet {
            if let Some(h) = hook.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                h(info);
            }
        }
    }));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    drop(panic::take_hook());
    if let Some(h) = previous.lock().unwrap_or_else(|p| p.into_inner()).take() {
        panic::set_hook(h);
    }
    match result {
        Ok(()) => false,
        Err(p) if panic_text(p.as_ref()).is_some_and(|m| m.contains(marker)) => true,
        Err(p) => panic::resume_unwind(p),
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> Option<&str> {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CircuitConfig {
        CircuitConfig {
            degree: 18,
            max_data_byte_len: 2048,
            max_log_num: 20,
            topic_num_bounds: (0, 4),
        }
    }

    fn rlp_bytes(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        alloy_rlp::Encodable::encode(&b, &mut out);
        out
    }

    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = items.concat();
        let mut out = Vec::new();
        alloy_rlp::Header {
            list: true,
            payload_length: body.len(),
        }
        .encode(&mut out);
        out.extend_from_slice(&body);
        out
    }

    fn log(topics: usize, data: usize) -> Vec<u8> {
        rlp_list(&[
            rlp_bytes(&[0x11; 20]),
            rlp_list(&vec![rlp_bytes(&[0x22; 32]); topics]),
            rlp_bytes(&vec![0x33; data]),
        ])
    }

    fn receipt(logs: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0x02];
        out.extend(rlp_list(&[
            rlp_bytes(&[1]),
            rlp_bytes(&[0x5a, 0x08]),
            rlp_bytes(&[0; 256]),
            rlp_list(logs),
        ]));
        out
    }

    #[test]
    fn a_receipt_at_the_bounds_passes() {
        let logs = vec![log(4, 2048); 20];
        check_receipt(&receipt(&logs), &config()).unwrap();
    }

    #[test]
    fn one_log_more_than_the_circuit_parses_is_unprovable() {
        let logs = vec![log(1, 32); 21];
        let e = check_receipt(&receipt(&logs), &config()).unwrap_err();
        assert!(e.0.contains("21 logs"), "{e}");
    }

    #[test]
    fn any_log_over_the_data_bound_is_unprovable_not_only_deposit() {
        let logs = vec![log(3, 96), log(1, 2049)];
        let e = check_receipt(&receipt(&logs), &config()).unwrap_err();
        assert!(e.0.contains("log 1 has 2049 data bytes"), "{e}");
    }

    #[test]
    fn a_log_with_five_topics_is_unprovable() {
        let e = check_receipt(&receipt(&[log(5, 0)]), &config()).unwrap_err();
        assert!(e.0.contains("5 topics"), "{e}");
    }

    #[test]
    fn a_legacy_receipt_is_read_without_a_type_byte() {
        let typed = receipt(&[log(1, 32)]);
        check_receipt(&typed[1..], &config()).unwrap();
    }

    #[test]
    fn trailing_bytes_are_malformed() {
        let mut r = receipt(&[log(1, 32)]);
        r.push(0);
        let e = check_receipt(&r, &config()).unwrap_err();
        assert!(e.0.contains("not well-formed"), "{e}");
    }

    #[test]
    fn only_the_marked_panic_is_caught() {
        assert!(catching_panic("over budget", || panic!(
            "keccak over budget"
        )));
        assert!(!catching_panic("over budget", || {}));
        let other = panic::catch_unwind(|| catching_panic("over budget", || panic!("other")));
        assert!(other.is_err());
    }
}
