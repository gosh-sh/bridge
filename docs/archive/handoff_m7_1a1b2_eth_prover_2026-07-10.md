> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Handoff Summary — M7 ETH-side prover (1A/1B/2 + Circuit 4)

_Session handoff, 2026-07-10._

## Context / repo
- Repo: `/home/sergey/Pruvendo/gosh/acki-nacki-bridge` (dual remotes: GitLab `origin`, GitHub `github`).
- Heavy halo2 builds/runs go on **n14** (`ssh -p 22488 gosh@94.156.178.14`), workspace `/mnt/data/gosh/sergey-bridge/acki-nacki-bridge`. Use `cargo +nightly` + `export PATH="$HOME/bin:$PATH"` (for `solc`) + `CARGO_NET_OFFLINE=true`.
- Excluded crates (own `target/`, not in workspace): `crates/bridge-snark-utils`, `crates/bridge-evm-aggregator`, `crates/bridge-relayer-daemon`, `crates/an-bridge-prover/*`.

## Key architectural fact established this session
**"Alina's library" = `crates/an-bridge-prover/bridge-prover-lib`** does ALL real AN-block proving. Every module is `pub` (`gql_client`, `attestation_fetcher`, `bk_set_fetcher`, `real_chain_builder`, `block_id_tree`, `bridge_state`, `layer_prover`, `prover`, `verifier`, `keys`). `bridge-prover-daemon/src/main.rs` is **pure orchestration** over those pub fns (proves Blake2b for the AN VM). A prior doc claim that "1A/1B/2 live→Poseidon is blocked because the daemon is binary-only" was **WRONG and has been corrected** — the fetchers are reusable public API.

Proof flavours: AN side = Blake2b (`ZKHALO2VERIFYWITHVK` opcode); ETH aggregator side = **Poseidon** (`our_side_reprove`). Both available in the lib via `*_with_transcript(TranscriptKind)`.

## What was built this session (committed locally, NOT yet pushed)
1. **`docs/m7_eth_side_prover_status_2026-07-07.md`** — removed the false blocker; documented the new 1A/1B/2 exporter, the n14 live-run result, and the precise partner ask.
2. **`crates/bridge-snark-utils/src/bin/export_1a1b2_poseidon_snark.rs`** (NEW) — ETH-side re-prove of Circuits 1A/1B/2, mirror of `export_c4_poseidon_snark.rs`:
   - `--circuit primary|fallback|layer|auto` (auto classifies attestation evidence: `[PRIMARY]`→1A, `[PRIMARY,FALLBACK]`→1B).
   - 1A/1B: `gql_client::create_client` → `fetch_attestation_evidence` + `fetch_bk_set` (config fallback `--bk-set-config`) → `generate_{primary,fallback}_proof_with_transcript(Poseidon)` → native self-verify → `export_poseidon_snark`.
   - Circuit 2: replays daemon's `generate_layer_proof_for_key_block` over pub lib fns (`query_proof_block_by_seqno`, `block_id_tree`, `real_chain_builder::build_real_chain` over `--state daemon_state.json`) → `generate_layer_proof_with_transcript(Poseidon)`.
   - **`--check-bk-set`**: fast (~1s, no proving) pre-flight comparing `compute_bk_set_poseidon(loaded_set)` vs block's `block_merkle_tree_leaves[2]`.
   - Same SRS downsize workaround as C4 (shared K=21 SRS → downsize in-memory to circuit k; halo2-axiom asserts exact `params.n()`). Key file prefixes: `primary_*`, `fallback_*`, `layer_*`, `event_*` (VK `{prefix}_vk.bin`, config `{prefix}_config_params.json`).
   - **NOTE**: layer keys on n14 are named `layer_hashes_*` (old orchestrator naming) but the lib's `LayerHashesKeyManager` PREFIX is `"layer"` → `ensure_layer_keys` would keygen fresh `layer_*` keys (heavy). Not exercised yet.
3. **`crates/bridge-snark-utils/Cargo.toml`** — added `hex`, `tokio` deps + `[[bin]] export-1a1b2-poseidon-snark`.
- Local `cargo check` + `clippy` clean (warm 3.0G target). n14 `cargo +nightly build --release` clean.

## n14 live-run result (2026-07-08)
Ran `--circuit auto` vs live shellnet (`https://shellnet.ackinacki.org/graphql`, real finalized key blocks). Full path executed on real data: live PRIMARY evidence (5 signers) OK → primary VK/PK 3.7GB load + SRS downsize OK → Poseidon proof generated OK → **SELF_VERIFY FAIL** (tool correctly refuses to export invalid snark).

**Root cause pinpointed (not the code): stale BK set.** Via `--check-bk-set`:
```
block.leaves[2] (shellnet current)  = 5f434241f8bfd6b3f8364117de3d6583290be7adb3fe99596685e667bbdfa113
prover/bk_set.json                   = e62d8b5f… MISMATCH
an-bridge-prover/bk_set.json         = 93f04b1d… MISMATCH
```
Current shellnet BK set is genesis-only and **not exposed** by the API: `bkSetUpdates` empty (`edges:[]`), no `/v2/bk_set` REST (404), no BK/validators field in `BlockchainQuery`. Both cached `bk_set.json` on n14 are from the local `poseidon_dex` stand (`*.poseidon_dex_local.bak` siblings). Only these 2 configs exist on n14.

## THE BLOCKER (external / partner-side)
To finish real 1A/1B/2: need the **shellnet genesis BK-set** — 5 × 48-byte BLS pubkeys (index→pubkey) whose `compute_bk_set_poseidon` == `5f434241f8bfd6b3f8364117de3d6583290be7adb3fe99596685e667bbdfa113`. Then: drop as `bk_set.json` → `--check-bk-set` (must MATCH) → `--circuit auto` (expect SELF_VERIFY PASS) → `aggregate-proof --name PrimaryAggregatorVerifier` → EVM calldata (self-checks vs committed `contracts/ethereum/verifiers/*.bin`).

Orthogonal notes: Circuit 4 (withdrawal) needs NO BK set — that's why C4 M7 is already green on real shellnet data. This FAIL is proven to be the BK set, NOT the 16-leaf `block_id`/PR#3 circuit-shape change.

## M7 state (from prior sessions, already done)
- `aggregate-proof` bin (`crates/bridge-evm-aggregator/src/bin/aggregate_proof.rs`): Poseidon inner snark → EVM calldata, byte-identical self-check vs committed verifier `.bin`.
- C4 ETH-side prover `export-c4-poseidon-snark --fixture PrivateWitness.json` + relayer `crates/bridge-relayer-daemon/src/aggregator.rs` (`Circuit4ShplonkPipeline`, `prove-withdraw-shplonk` CLI, 56 tests green).
- C4 proven on real shellnet withdrawal witness on n14: our 10 PIs == partner byte-for-byte; Foundry `AckiNackiBridgeProductionWithdrawByProof` 3/3 green.
- All 4 aggregator `.bin` committed under `contracts/ethereum/verifiers/`.

## Pending todos
- **Push** this session's work (user asked "Давай пуш" earlier in the broader chat — the 1A/1B/2 exporter + doc are NOT pushed yet; confirm scope).
- Optional: draft partner message to Alina requesting shellnet genesis BK set.
- `q2`: delete dead Solidity (18 files + 3 bins) + NatSpec sweep.
- `agg`: delete orchestrator export-spike-artifacts + export-halo2-poseidon-snark, rewrite stale README.
- `redeploy`: shellnet withdraw-bridge redeploy with all 4 real verifiers (dry-run green; broadcast gated on genesis anchor + real accFr + deploy key).
- `node16leaf` / `regen-c2-wrap`: after andrew node release + Alina PR#9 merge + circuits PR#3 — support 16-leaf `block_id` in `AckiNackiBridge`, regen `LayerHashesAggregatorVerifier.bin` (+recheck 1A/1B/4 shapes) on n14.

## Live config (non-secret)
- Shellnet GraphQL: `https://shellnet.ackinacki.org/graphql`; thread_id (hex): `0000…0000` (68 chars). Every block carries `block_merkle_tree_leaves[8]`; `leaves[2]` = BK-set commitment.
- Sepolia withdraw bridge `0x58a1…043d`; deposit bridge `0x99c37fb7…4ce82`. Ursus operator: `ubuntu@ursus-tools.dev`.
- Secrets: `.secrets`.
