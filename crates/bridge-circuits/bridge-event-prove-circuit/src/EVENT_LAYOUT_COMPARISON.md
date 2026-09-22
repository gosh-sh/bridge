# `WithdrawalInitiated` vs `VoucherGenerated` — cell layout & circuit handling

Empirical findings from real BOCs (`acki-nacki/tests/exchange/withdrawals.txt`,
`acki-nacki/tests/dex/vouchers.txt`) and how they shape
`bridge_event_prove_circuit.rs`.

Both circuits share the outer pipeline: SHA-256 cell chain → `ext_msg_leaf` →
events Merkle proof → `block_leaf` → block Merkle proof → dense chain →
`final_root`. They differ only in the inner cell DAG and field extraction.

---

## 1. Why we hash the ExtOut wrapper, not just the body

`compute_ext_message_leaf_hash` in `acki-nacki/node/src/types/history_proof.rs`
takes `ext_message_hash = msg_cell.repr_hash()` where `msg_cell` is the
**root** of the serialized outbound `Message` DAG. That `repr_hash` transitively
binds every child cell via the parent's child-hashes section, so the circuit
must hash starting at the ExtOut wrapper (`entries[0]`), not the body.

`entries[0]` is the wrapper; the event body is `entries[1]`.

---

## 2. Cell DAG shape

### 2.1 `VoucherGenerated` — 2 cells

| idx | role           | refs | content                                     |
|-----|----------------|------|---------------------------------------------|
| C0  | ExtOut wrapper | 1→C1 | ExtOut header + child_hashes[0]=sha256(C1)  |
| C1  | event body     | 0    | 6-byte ABI prefix + scalars inline          |

Body inline: `[0..6)` ABI prefix, `[6..38)` `sk_u_commit` (LE Fr),
`[38..70)` `voucher_nominal` (BE), `[70..74)` `token_type` (BE).

### 2.2 `WithdrawalInitiated` — 4 cells

| idx | role           | refs   | size (B) | content                                              |
|-----|----------------|--------|----------|------------------------------------------------------|
| C0  | ExtOut wrapper | 1→C1   | —        | ExtOut header + child_hashes[0]=sha256(C1)           |
| C1  | event body     | 2→C3,C2| 126      | scalars inline, then child_hashes for C2, C3         |
| C2  | recipient cell | 0      | 22 *     | `00 28` + 20 recipient bytes                         |
| C3  | sender cell    | 0      | 36       | `00 43` + 34 B encoding `std_addr$10` + wc + acc_id  |

\* Fixed at 22 in the circuit (`RECIPIENT_CELL_LEN = 2 + RECIPIENT_LEN_FIXED`,
where `RECIPIENT_LEN_FIXED = 20`). Solidity caps recipient ≤ 64 bytes; a
variable-length path is deferred (see §5).

#### Body byte layout (126 B, matches constants in `bridge_event_prove_circuit.rs`)

```
[0..2)    d1 + d2                                         (header)
[2..6)    ABI event id = 0x3c838959                        (EVENT_ABI_PREFIX_*)
[6..38)   dstChainId  (uint256, BE)        ← PRIVATE       (EVENT_DST_CHAIN_ID_*)
[38..54)  amount      (uint128, BE)        ← PRIVATE       (EVENT_AMOUNT_*)
[54..58)  tokenId     (uint32,  BE)        ← PUBLIC        (EVENT_TOKEN_ID_*)
[58..62)  child_depths (2 × u16 BE)
[62..94)  child_hashes[0] = sha256(C2 recipient)
[94..126) child_hashes[1] = sha256(C3 sender)
```

Why both ref cells exist: under TVM Solidity ABI v2, `bytes` (recipient) and
`address` (sender, 267 bits) overflow the parent and are emitted as refs.

#### Sender cell C3 (36 B, fixed)

```
data[0]   = 0x00          d1: refs=0
data[1]   = 0x43          d2: bit_len=267, last byte partial
data[2..36] payload:
    bits[0..2)   = 0b10        std_addr$10 tag
    bits[2..3)   = 0           no anycast
    bits[3..11)  = workchain   (0 in all fixtures)
    bits[11..267)= account_id  (NOT byte-aligned)
```

`account_id` is left implicit — the circuit binds the whole cell via SHA-256
and never extracts it as an Fr.

---

## 3. Differences that affect the circuit

| Aspect                        | Voucher (dark-dex)         | Withdrawal (bridge, v2)                                                          |
|-------------------------------|----------------------------|----------------------------------------------------------------------------------|
| Cells in flattened BOC        | 2                          | 4                                                                                |
| Body `refs_count`             | 0                          | 2                                                                                |
| ABI prefix offset / length    | `[0..6)`                   | `[2..6)` (after d1+d2)                                                           |
| Leading scalar endianness     | LE Fr (`sk_u_commit`)      | BE u256 (`dstChainId`), BE u128 (`amount`), BE u80×2 (recipient α-split)         |
| Public event fields           | `voucher_nominal`, `token_type` | `tokenId`, `amount`, `recipientHi`, `recipientLo`, `dstChainId`             |
| Private event fields          | `sk_u` (secret)            | recipient cell raw bytes, sender cell raw bytes                                  |
| Recipient/sender              | inline                     | each its own ref cell (C2, C3)                                                   |
| SHA-256 chain links           | 1                          | 3 (wrapper→body, body→recipient, body→sender)                                    |
| Identity binding              | `sk_u` ↔ `sk_u_commit`     | `dappFr` + `accFr` (destination) + `senderAccFr` (source) public                 |
| Replay binding                | (n/a)                      | `nullifier = Poseidon(block_id, tokenId, amount, recipHi, recipLo, senderAcc)`   |
| Anchor binding                | (n/a)                      | multi-layer-hash-choice (see §4.5)                                               |

There is no per-event Poseidon "final commitment" on the bridge side — every
event field is already on-chain on Acki Nacki, so there is no secret to seal.
The proof is instead bound to a specific destination by exposing `dapp_fr`
and `acc_fr` as public instances.

---

## 4. In-circuit handling (matches `bridge_event_prove_circuit.rs`)

### 4.1 Witness assignment & SHA-256

All four `entries[i].cell_repr_data` arrays are assigned as
`Vec<AssignedValue<Fr>>` byte witnesses. `Sha256Chip::digest_bytes` performs
the 8-bit range check on every input byte. Hashes computed for all 4 cells.

### 4.2 Child-hash equality (3 links, 96 byte-equalities)

Offsets come from `entries[i].childs_repr_hashes_offset`:

```
wrapper_bytes[ch0   .. ch0+32] == body_hash
body_bytes  [bch0   .. bch0+32] == recipient_hash
body_bytes  [bch0+32.. bch0+64] == sender_hash
```

### 4.3 Field extraction & constraints

From body (`entries[1].cell_repr_data`):
- `tokenId` ← BE-pack `body[54..58)` — slot `PUB_TOKEN_ID`.
- `amount` ← BE-pack `body[38..54)` — slot `PUB_AMOUNT`.
- `dstChainId` ← BE-pack `body[6..38)` — slot `PUB_DST_CHAIN_ID`.
- ABI event id: `body[2..6)` constrained to `0x3c838959` (4 byte equalities).
- `d1` refs_count constrained for **all 4 cells** (1, 2, 0, 0): low 3 bits of
  the byte are decomposed and compared against the constants.

From recipient cell (`entries[2].cell_repr_data`):
- `recipientHi` ← BE-pack `recipient[2..12)` — slot `PUB_RECIPIENT_HI`.
- `recipientLo` ← BE-pack `recipient[12..22)` — slot `PUB_RECIPIENT_LO`.

Recipient split convention is **α** (10/10 BE bytes). ETH-side reassembly:

```solidity
address recipient = address(uint160(
    (uint256(recipientHi) << 80) | uint256(recipientLo)
));
```

TODO(circuit4-v2) — alternative splits if ETH side prefers different on-chain
typing (the in-circuit code is one `bytes_to_fr` per half — switching
convention is a one-line code change per slot):
- **β** = `uint128(bytes[0..16]) + uint32(bytes[16..20])` — 4 leading zero
  bytes in `recipientHi`, wasteful.
- **γ** = `uint80(bytes[0..10]) + uint80(bytes[10..20])` — identical to α.

### 4.4 Poseidon chain & Merkle proofs

`poseidon_hash_96_circuit` chunks 3 × 32-byte inputs at 31-byte boundaries
into 4 Frs (with the algebraic-linking trick that re-uses `hi_a`, `low_b`,
`hi_b`, `low_c` across boundaries, range-checked at 248/8/240/16/232/24 bits):

```
ext_msg_leaf  = Poseidon96(dapp_id, account_id, repr_hash(C0))
ext_out_root  = padded events Merkle proof from ext_msg_leaf
                (depth ≤ MAX_EVENTS_TREE_DEPTH = 8; depth witnessed,
                 range-checked at 4 bits with depth ≤ MAX)
block_leaf    = Poseidon96(block_id, envelope_hash, ext_out_root)
root_1        = block Merkle proof from block_leaf (fixed depth)
final_root    = verify_chain_of_dense_proofs(root_1, dense_chain,
                num_active ≤ MAX_CHAIN_LEN, range-checked at 4 bits)
```

`repr_hash_fr` is the LE-pack of `wrapper_hash` (32 bytes → Fr).

### 4.5 Multi-layer-hash choice

The verifier supplies `NUM_LAYER_HASHES = MAX_LAYERS * W` candidate layer
hashes (W = 8 by default via `w-8` feature, or 128 via `w-128`). The prover
witnesses a private `hash_choice_index`, range-checks `(NUM_LAYER_HASHES - 1)
- index` to `HASH_IDX_BITS = ceil(log2(NUM_LAYER_HASHES))` bits, and
constrains `gate.select_from_idx(layer_hashes, index) == final_root`.

Result: the verifier learns the proof anchors to *some* known layer hash but
not *which* one.

### 4.6 Public instance layout (column 0, v2)

`NUM_LEADING_PUBLIC_INPUTS = 9` leading slots + `NUM_LAYER_HASHES` candidate
hashes. Slot indices are exported as `PUB_*` constants.

```
[0]   token_id       BE u32  from body[54..58)
[1]   amount         BE u128 from body[38..54)
[2]   recipientHi    BE u80  from recipient[2..12)   (split α — see §4.3)
[3]   recipientLo    BE u80  from recipient[12..22)
[4]   dstChainId     BE u256 from body[6..38)
[5]   senderAccFr    Fr-encoding of sender account_id, algebraically
                     decoded from sender cell bits [11..267) — see §4.7
[6]   dappFr         bytes_to_fr(account_dapp_id)    (destination dApp)
[7]   accFr          bytes_to_fr(account_id)         (destination account)
[8]   nullifier      Poseidon(block_id_fr, tokenId, amount,
                              recipientHi, recipientLo, senderAccFr)
[9 .. 9+NUM_LAYER_HASHES] layer hash candidates
```

Total: `TOTAL_PUBLIC_INPUTS = 9 + NUM_LAYER_HASHES` Fr values.

### 4.7 senderAccFr (slot 5) — algebraic decode

`senderAccFr` is **not** loaded as an orchestrator witness. It is derived
in-circuit from `entries[3].cell_repr_data` (bytes [3..36], the std_addr$10
payload after `d1`+`d2`) by extracting bits [11..267) per §2.2:

```
for j in 3..36:
    sender_bytes[j] = high3[j] * 32 + low5[j]   (range_check 3 / 5 bits)
for i in 0..32:
    account_id_byte[i] = low5[3+i] * 8 + high3[4+i]
senderAccFr = sum_i (account_id_byte[i] * 256^i)
```

This matches the native `bytes_to_fr(account_id_bytes)` convention
(`Fr::from_raw` on 4 LE u64 limbs). Because the sender cell bytes are
already SHA-256-bound to the event body, the resulting `senderAccFr` is
uniquely determined by the source-chain BOC — no orchestrator-supplied
witness, no malleability surface.

Routing bits (std_addr$10 tag / anycast / workchain) are NOT constrained
here — see §5 "constrain d2 and tighten entries[3] bit-prefix".

**No `senderDappFr` public input.** The TVM address type (`std_addr$10` =
`MsgAddrStd { anycast, workchain_id, address }` in
`tvm-sdk/tvm_block/src/messages.rs`) has no dApp-id field; `dapp_id` lives
in `ShardAccount` state metadata
(`tvm-sdk/tvm_block/src/accounts.rs:ShardAccount.dapp_id`). Binding the
sender's dApp-id would therefore require a separate `ShardAccount`-state
Merkle proof — out of scope for v2. If the destination side ever needs
this, the right architectural fix is for the AN bridge contract dev to add
a `senderDappId` field directly to the `WithdrawalInitiated` event so it
appears in the body BOC and can be parsed/bound the same way as
`tokenId` / `dstChainId`.

### 4.8 Nullifier (slot 8)

Single-sponge call (`hash_fix_len_array` over 6 Fr inputs, RATE=2 → 3 absorb
rounds + squeeze). Native counterpart is `test_helpers::nullifier_native`,
which delegates to `bridge_poseidon::poseidon_hash_fr` for the same params
(T=3, R_F=8, R_P=57). MockProver checks instance equality, so test
`test_nullifier_recomputes_natively` proves the two paths agree.

The nullifier binds the destination chain side's burn record to:
- the source block (`block_id_fr`, already a witnessed Fr) — block-level
  anonymity is preserved because the verifier still doesn't learn `block_id`
  directly; only the nullifier is public;
- every settled withdrawal field (token, amount, recipient, sender), so
  replay protection is over the full tuple, not just a single field.

---

## 5. Out-of-scope / TODO

- **Variable-length recipient up to `MAX_RECIPIENT_LEN = 64`** (constant
  exists in code, unused). Requires `digest_bytes_var_len` on the SHA-256
  chip or a precomputed-prefix trick, plus a `d2 == recipient_len << 1`
  constraint and zero-padding assertion. Punted until needed.
- Constraining `d2` and tightening `entries[3]` bit-prefix (`std_addr$10`
  tag + anycast=0 + wc=0) if a routing attack model warrants it.
- Real-prover K sweep, mirroring `dark_dex_circuit_new::test_k_sweep_benchmark`.
- §5.7 **`senderDappFr` is not exposed.** Resolved in v2 +
  sender-binding (this revision): the TVM address type carries no dApp-id
  field, and routing this through a separate `ShardAccount`-state Merkle
  proof is substantially more work than this crate's scope. The path
  forward, if the destination side ever needs the source dApp-id, is for
  the AN bridge contract dev to add a `senderDappId` field directly to
  the `WithdrawalInitiated` event — then it appears in the body BOC and
  can be parsed/bound the same way as `tokenId` / `dstChainId`.
  `senderAccFr` itself is now algebraically decoded from the sender cell
  payload (see §4.7) — no orchestrator-supplied witness.
