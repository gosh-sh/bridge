# Documentation — state and rewrite plan

> **Everything outside `docs/archive/` is current. `docs/archive/` is not.**

**As of 2026-08-18** the repository's prose was consolidated so that documentation can be rewritten
from scratch instead of patched. Read this file before adding any document.

## What exists now

| Path | Status |
|---|---|
| [`docs/ETH-contracts-spec.md`](docs/ETH-contracts-spec.md) | **The only verified document.** The Ethereum contract system as implemented, derived by reading the Solidity sources at commit `a69ba36`, every behavioural claim carrying a `file:line` citation. Safe to act on. |
| `docs/archive/` (66 files) | Everything else the repository had. **Staged for deletion.** Not maintained, not authoritative, and in several places contradicted by the code. Source material for the rewrite — nothing more. |
| `README.md`, `AGENTS.md`, `*/README.md`, `.cursor/skills/**` | Left in place, unchanged, by decision. See *Reference conventions* below. |

Everything under `docs/` other than the spec was moved into `docs/archive/`, with directory structure
preserved so a half-remembered path still finds its file: `docs/reviews/x.md` → `docs/archive/reviews/x.md`,
`crates/an-bridge-prover/docs/y.md` → `docs/archive/crates/an-bridge-prover/docs/y.md`.

## Reference conventions (transitional — delete with the archive)

* **Stale path in a doc or comment?** The rule is mechanical: `docs/<name>` → `docs/archive/<name>`.
  References in code, scripts, `Makefile` and `.gitignore` were repointed during the move.
* `README.md`, `AGENTS.md` and the agent skills under `.cursor/` were deliberately **not** edited, so
  their `docs/…` paths still read as they did before the move. Apply the rule above when following one.
* Inside `docs/archive/`, cross-references were left untouched. The files are being replaced, not
  maintained.
* `docs/ETH-contracts-spec.md` cites `src/…`, `test/…`, `script/…`, `verifiers/…` relative to
  **`contracts/ethereum/`** — it was written next to those sources and moved without rewriting its
  citations.

## The rewrite

**Goal:** delete `docs/archive/` and replace it with a small set of documents that are true, each
covering one genre for one audience. The archive exists so that step loses no knowledge.

**Rules for anything new:**

1. **Verified or nothing.** A document states the commit it was checked against and cites `file:line`
   for claims about code. If a claim cannot be verified from the source, it says so in the text rather
   than being asserted or quietly dropped.
2. **One genre per document.** Design-and-correctness, operations, and verification are three
   different documents about one subject; a new one of the first kind does not replace the other two.
   Most of the archive's damage came from ignoring this.
3. **Live next to what it describes** where that is a component (a crate's own docs), and under
   `docs/` where it spans components.
4. **No orphan status reports.** Point-in-time snapshots (`*_status_*.md`, handoffs, per-PR reviews)
   belong in the PR or the issue tracker, not in `docs/`.
5. When a document falls behind the code, re-verify it and bump the commit, or delete it. Do not let
   it sit and rot — that is how the archive happened.
6. **No new document may reference `docs/archive/`.** Read the archive while writing if it helps, but
   cite the code, never the archived prose — a citation into a folder that is about to be deleted is
   a dead link by construction. When the last document is rewritten, `docs/archive/` is deleted and
   the two transitional sections below go with it.

**Proposed target set** — to be confirmed before writing:

| Document | Genre | Must cover | Source material in the archive |
|---|---|---|---|
| `docs/ETH-contracts-spec.md` | reference (exists) | Contract system, ABI, storage, deploy, tests, security observations | — already verified |
| `docs/architecture.md` | design | Four circuits, cross-circuit binding, the two directions end to end, trust assumptions | `four_circuit_architecture.md`, `audit_trail_v2.md`, `integration_analysis.md`, `storage_v2_abi_note.md` |
| `docs/deposit-direction.md` | design + reference | ETH → AN: 12 public inputs, proven `chainId`, MPT depth, VK reproducibility, the AN-side consumer | `deposit_*.md` (7 files), `verifying_eth_proof_on_an.md`, `zk_halo2_an_side_design.md`, `zkhalo2verifywithvk_reference.md` |
| `docs/withdrawal-direction.md` | design + reference | AN → ETH: `verifyBlock`, `applyBkSetUpdate`, Circuit 4 payout, anchors and windows | `circuit_4_open_questions.md`, `an_eth_daemon_withdraw_e2e_*.md`, `shellnet_an_eth_relayer_wiring.md` |
| `docs/operations.md` | operations | Running the daemons, the two live lanes, deploy timing, failure modes actually hit, monitoring queries | `archive/crates/an-bridge-prover/docs/live_*_runbook.md` (the freshest material in the archive), `archive/audit/*runbook*.md`, `bridge_verification.md` §11 |
| `docs/verification.md` | verification | How to convince yourself a proof and a deployment are correct, stage by stage | `verifying_an_proof.md`, `bridge_verification.md`, `manual_verification_runbook.md` |
| `docs/aave-yield.md` | design + operations | The three pockets, `harvestYield` vs `skimExcessUsdc`, invariants, ordering rules | `aave_integration.md` — a line-by-line verified rewrite of it exists on branch `docs/verify-against-code-2026-08-18` |
| `docs/user-guide.md` | user | End-user deposit and withdrawal flow | `archive/user/USER_GUIDE.md` |

**Known traps in the source material** — carry these into the rewrite, they are verified against the
code and were wrong in the old docs:

* Deposits take **USDC** via `transferFrom`, signature `deposit(uint256,int8,bytes32)`, cap
  `type(uint64).max`. Any archive text with `msg.value`, ETH deposits or a `100 ether` cap is dead.
  The contract has no `receive()`/`fallback()` — it cannot hold native ETH at all.
* Verification is **SHPLONK aggregator Yul** for four circuits. The gnark wrappers, the
  `*Groth16VerifierGenerated.sol` files and the `gnark-wrappers/` trees do not exist.
* There is no pause switch (removed in `d6bfed4`) and no upgrade path.
* BK-set rotation **shipped** as `applyBkSetUpdate` (16-leaf, depth-4). Archive text calling it
  "pending Phase 1.C" is stale, and so is any monitor asserting the commitment never changes.
* The payout path is `withdrawByProof` against a Circuit-4 proof, nullifier-guarded. The refund-style
  `withdraw(depositId, …)` and `processedDeposits` are gone.
* `skimExcessUsdc` (added `9cc2fdb`, audit finding QC-A1-3) is the *only* way to collect yield after
  `emergencyWithdrawAll` — `harvestYield` necessarily reverts `NoYield` at that point. No archive
  document knows this.
* Key-block stride is `W·P` = 128 × 8 = **1024**; `P` was 4 until `a69ba36`.
* The relayer submits to the contract directly — the `.result.json` ACK gate on
  `bridge-verifier-daemon` was deleted in `a69ba36`.

**Two code-level observations** found while verifying, reported and not fixed:
`_amountSupplyable` (`contracts/ethereum/src/AckiNackiBridge.sol:1358-1363`) does not exclude
`excessUsdc()`, so a `supplyToAave` after an emergency unwind books uncollected yield into
`suppliedPrincipal`; and `usdc.approve`'s return value is ignored at `:1250`.

## Archive contents, by theme (transitional — delete with the archive)

Source material for the table above, and the coverage checklist for the rewrite: every theme here
must be answered by some document in the target set before the archive is deleted. 66 files.

| Theme | Files |
|---|---|
| Architecture and integration | `four_circuit_architecture`, `an_partner_integration_plan`, `integration_analysis`, `audit_trail_v2`, `storage_v2_abi_note`, `integration_plan` (legacy M0–M9), `archive/*live_integration_plan*`, `*live_driver_refactor_plan*` |
| Verifiers, R15 / SHPLONK | `r15_snark_verifier_roadmap`, `r15_verifier_sizing_report`, `halo2_on_chain_verification_paths`, `circuit1a_yul_verifier_implementation_plan`, `layer_hashes_circuit_audit`, `proof_metrics_report`, `hermez_kzg_repos_and_branches`, `keccak_coprocessor_flowchart.mmd` |
| Deposit direction | `deposit_chain_binding_track2`, `deposit_max_key_byte_len`, `deposit_vk_witness_independence`, `deposit_vk_reproducibility`, `deposit_vk_mpt_depth_witness_dependence_*`, `deposit_finalize_vk_gap_*`, `bridge_deposit_chain_binding_fix_proposal_*`, `verifying_eth_proof_on_an`, `zk_halo2_an_side_design`, `zkhalo2verifywithvk_reference`, `shellnet_usdcbridge_deposit_vk_redeploy`, `partner_note_usdcbridge_chainid_hermez_*` |
| Withdrawal / state direction | `verifying_an_proof`, `circuit_4_open_questions`, `an_partner_questions_circuit4_*`, `an_partner_circuit4_alina_replies_*`, `an_partner_circuit4_concept_response_*`, `an_eth_daemon_withdraw_e2e_*`, `shellnet_an_eth_relayer_wiring`, `legacy/verifying_an_proof_v1` |
| Operations and runbooks | `archive/crates/an-bridge-prover/docs/live_verifyBlock_runbook`, `…/live_withdrawByProof_runbook`, `…/daemon_live_performance`, `…/bridge-prover-daemon/docs/PROBE_BK_UPDATES`, `audit/deposit-relayer-operator-runbook`, `audit/eth-qc-hardening-runbook`, `shellnet_e2e_acceptance_runbook`, `manual_verification_runbook`, `bridge_verification`, `user/USER_GUIDE` |
| Status, plans, handoffs | `production_plan`, `testnet_security_status`, `m7_eth_side_prover_status_*`, `handoff_m7_*`, `live_e2e_prover_relayer_plan`, `audit/F10-deposit-pipeline-remediation-plan`, `audit/HANDOFF-f10-prover-relayer-ru.txt`, `SUMMARY`, `FIX_TASK_FOR_AGENT` |
| Reviews and partner threads | `reviews/pr27_answers_nb_q1_q11_*`, `reviews/pr20_review_*`, `reviews/deposit_circuit_audit_*`, `reviews/alina_circuit4_single_final_root_*` (+ pdf), `reviews/alina_review_pack_*`, `reviews/an_partner_questions_circuit4_*_audit`, `reviews/an_token_bridge_pr2112_review`, `an_partner_phase0_questions`, `an_partner_questions_2026-05-11.txt`, `aave_integration` |

**One open dependency before deleting the archive:** `archive/FIX_TASK_FOR_AGENT.md` describes a live
user-facing bug — the frontend advertises a withdrawal flow the contract does not expose, so a user
can believe funds came back when nothing happened. Confirm that fix landed in `frontend/` before the
file goes, or the only written record of the bug goes with it.
