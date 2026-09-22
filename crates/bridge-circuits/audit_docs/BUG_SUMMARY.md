# Bug Summary — attestation-bls-checker-circuit

Резюме: ни одного подтверждённого soundness break. Один cross-circuit вопрос (BC-014) выходит за scope аудируемого crate'а, но потенциально блокирующий для production бриджа в целом.

---

## P1 — production-blocking (cross-circuit)

### BC-014: Circuit 3 vs Circuit 1A/1B — несовместимые Poseidon conventions

| Circuit | In-circuit | Off-circuit oracle | Padding |
|---------|-----------|---------------------|---------|
| Circuit 1A/1B | `compute_old_bk_set_commitment` (lib.rs) | `attestation::test_helpers::compute_bk_set_poseidon_instance` | **YES**, MAX_SIGNERS=300 |
| **Circuit 3** | `bk-set-update-checker-circuit/src/lib.rs:101::compute_bk_set_commitment` | `bk_set_update::test_helpers::compute_bk_set_poseidon_instance` | **NO**, real entries only |

Архитектурная причина: Circuit 1A/1B — single-VK design (load_witness для signer indices → нужно padding до фиксированного MAX_SIGNERS). Circuit 3 — multi-VK design (load_constant → padding не нужен).

Решение: не забыть добавить padding в Circuit 3 in-circuit.

---

## Upstream problems

### BC-015 (fuzzing): panic в `gosh-halo2-crypto-lib::deserialize_*` на malformed input

**Файлы:**
- `gosh-halo2-crypto-lib/bls-verification/src/lib.rs:336` — `deserialize_g2_signature`
- `gosh-halo2-crypto-lib/bls-verification/src/lib.rs:358` — `deserialize_g1_pubkey`

Обе функции используют `panic!()` если input не декодируется. Конструкторы `PrimaryAttestationBlsCheckerCircuit::new` и `FallbackAttestationBlsCheckerCircuit::new` вызывают их на каждом вызове. Malformed attestation (corrupted bytes от node / network glitch) → **DoS на relayer'е** (расширение BC-006).

**Решение:**
```rust
pub fn deserialize_g1_pubkey(pk_bytes: &[u8]) -> anyhow::Result<G1Affine> {
    if pk_bytes.len() != 48 {
        anyhow::bail!("BLS public key must be 48 bytes, got {}", pk_bytes.len());
    }
    let bytes: [u8; 48] = pk_bytes.try_into().unwrap();
    let opt_be = G1Affine::from_compressed_be(&bytes);
    if bool::from(opt_be.is_some()) { return Ok(opt_be.unwrap()); }
    let opt_le = G1Affine::from_compressed_le(&bytes);
    if bool::from(opt_le.is_some()) { return Ok(opt_le.unwrap()); }
    anyhow::bail!("Failed to deserialize 48-byte BLS pubkey as G1Affine")
}
```

Аналогично `deserialize_g2_signature`. Конструкторы circuit'ов соответственно становятся `Result<Self, Error>` (совместное расширение с BC-012).

### BC-016 (Phase 3 fuzzing): `halo2-axiom::ParamsKZG::read_custom` panic

**Файл:** upstream `halo2-axiom/src/poly/kzg/commitment.rs:189` — "attempt to shift left with overflow".

Не блокирует наш circuit-runtime (ParamsKZG парсится только offline, при setup'е). Стоит зарепортить upstream'у, не критично для bridge security.

## P2 — defense-in-depth и hygiene

### BC-002 (partial fix): parent_block_id и block_id equality в Fallback

После d1f08ce между att1 и att2 проверяется equality для `envelope_hash` (32 байта) и `block_seq_no` (4 байта). **Не проверяется** equality для `parent_block_id` (40 байт offset 0..40) и `block_id` (40 байт offset 40..80).

**Сценарий:** атаки с разными `block_id`/`parent_block_id` при совпадающем envelope_hash и block_seq_no требуют SHA-256 collision, что с одной стороны нереалистично, но как defense-in-depth реализовать легко и ничего не стоит по сложности.


### BC-001: hygiene: explicit byte range_check на attestation_data (self-documentation)

Байты `attestation_data` загружаются как witness без явного `range_check(byte, 8)` в самой схеме. Защита работает косвенно через `Sha256Chip::digest_bytes` (downstream в `hash_to_curve`). Однако: hidden invariant хрупок к рефакторингам и пропускается static analysis.

**Решение:**
```rust
// primary_circuit.rs:67-72 и fallback_circuit.rs:69-86
.map(|&b| {
    let cell = ctx.load_witness(F::from(b as u64));
    range.range_check(ctx, cell, 8);
    cell
})
```

### BC-005: hygiene: explicit range_check на last_seen witness

`last_seen_block_seqno` загружается как witness без явного `range_check(last_seen, 32)`. Но защита работает через `range_check(diff_minus_one, 32)`.

**Решение:**
```rust
// lib.rs:190
let last_seen = ctx.load_witness(F::from(last_seen_block_seqno as u64));
range.range_check(ctx, last_seen, 32);  // ← добавить
```
1 lookup. Schema становится self-contained.

### BC-003 + BC-009: hygiene: explicit constraints на padding-pattern в Poseidon BK set

В `compute_old_bk_set_commitment` (lib.rs:110-158):
- `is_real` bit witness, без ограничения что pattern монотонно убывает (real entries first).
- `signer_index` для padding entries без ограничения, что = `PADDING_SIGNER_INDEX` (0xFFFF).

Защита работает через Poseidon binding к public instance, но строго зависит от off-circuit функции, которая выполняет подстановку правильного посейдон хэша в публичные входы. Нужно больше внимания к рефакторингу дальнейшему без in-circuit защиты.

### BC-006: panic-prone parsers (off-circuit DoS)

`bridge_parsers::attestation_data_parser`:
- `parse_signature_bytes(att)` panics если `att.len() < 200`
- `parse_num_signers(att)` panics если `att.len() < 208`
- `parse_signer_entries(att)` capacity overflow если `num_signers ≈ u64::MAX`

Не security issue для самой схемы (panic = reject), но плохая robustness инфраструктуры relayer'а.


### BC-011: bk_set с size > MAX_SIGNERS silent truncation

`PrimaryAttestationBlsCheckerCircuit::new` (и Fallback): `Vec::resize(max_signers, ...)` молчаливо обрезает, если `bk_set.len() > 300`. Signers с большими indices теряются, последующее remapping (BC-012) panics.


### BC-012: panic в remapping `unwrap_or_else(|| panic!)`

При неизвестном `signer_index` от attestation, конструктор panics. Off-circuit DoS-вектор: malformed attestation crash'ит prover'а.

### BC-010: PADDING_SIGNER_INDEX = 0xFFFF collision

Спека утверждает, что  "real signer indices are always much smaller than 0xFFFF", но это не заложено в схему. Если bk_set legitimately содержит `0xFFFF`, поведение остаётся корректным, но это implicit invariant.

### BC-013: count в signer_entries без upper bound

Внутри `verify_bls_attestation_with_assigned_msghash` MSM использует `max_bits=17` для shifted scalars. При honest signer_entries (HashMap unique keys) каждый pubkey получает count ≤ 65535 < 2¹⁷. Опционально можно было бы добавить проверку upstream `verify_bls_attestation_with_assigned_msghash`.