# TD-20 — MPT padding witness malleability (QC-PROV-04)

PoC: `tests/td_20_padding_malleability.rs`, legacy `padding_mutation_poc.rs`.

## QC-PROV-04 summary

| Witness mutation | MockProver | Public inputs |
|------------------|------------|---------------|
| `max_key_byte_len=3`, corrupt key slot **1** (active RLC prefix) | **Reject** | — |
| `max_key_byte_len=3`, corrupt key slot **2** (trailing padding) | **Pass** | **Byte-identical** to baseline |
| Active key last byte flip | **Reject** | — |
| `max_key_byte_len=4` + slot 3 garbage | Reject or pass | If pass → PI identical |

## Verdict: **QC** (upstream axiom-eth padding slots)

1. Trailing `key_bytes` padding beyond `key_byte_len` is not zero-constrained — witness malleability only.
2. All **passing** padding mutations keep 12×32-byte public inputs **byte-identical** → **not BC** (no PI drift / false deposit).
3. Production uses `max_key_byte_len=3` (QC-PROV-03 closed); relayer binds event fields off-circuit.

**BC** would require prove pass + **different** PI — not observed.

Closes TD-09 gap note «padding malleability» at canonical depth.

## Commands

    cd deposit-prover && cargo test td_20 -- --nocapture
    cd deposit-prover && cargo test padding -- --nocapture
    cd deposit-prover && cargo test
