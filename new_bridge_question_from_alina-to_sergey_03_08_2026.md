# bridge-EVM — Follow-up Questions (2026-08-03)

**To:** Pruvendo / Sergey Egorov
**From:** Alina

---

## NB-Q1 (2026-08-03) — Stale Groth16 one-shot Sepolia E2E path

**State.** `scripts/run_an_eth_e2e_sepolia.sh` (last real E2E success `eb325f1`,
2026-06-10) still drives Sepolia through a **Groth16-wrap → 256-byte submit**
pipeline, but the R15 SHPLONK-only cutover landed on 2026-06-21 (`bc022cc`
"feat(r15): Shplonk-only deploy, HistoryWindow anchors, and n14 proving
script") and the gnark hybrid was formally retired
(`contracts/ethereum/verifiers/README.md`: *"The gnark Groth16 fallback hybrid
is retired."*). The script was never updated.

**Concretely broken pieces.**

1. `scripts/wrap_partner_proof_groth16.py` — invokes
   ```python
   subprocess.run(["go", "run", ".", "prove", str(halo2_path)],
                  cwd=repo/"crates/bridge-prover-orchestrator/gnark-wrappers/circuit-1a")
   ```
   The directories
   `crates/bridge-prover-orchestrator/gnark-wrappers/{circuit-1a,circuit-2}`
   **do not exist in the repo**. First invocation fails on `go run` in a
   nonexistent working directory.

2. `scripts/run_an_eth_e2e_sepolia.sh` calls that wrapper for every block in
   `VERIFY_CHAIN`, so the script cannot get past its first `for SEQ` iteration.

3. Even if the wrapper were resurrected: it produces a **256-byte Groth16
   blob** and patches it into `primary_proof_hex` / `layer_proof_hex`. The
   R15-deployed `PrimaryAggregatorVerifier` / `LayerHashesAggregatorVerifier`
   are SHPLONK Yul (21 494 B / 19 100 B, EIP-170 OK) and expect
   `instances ‖ proof` calldata produced by `bridge-evm-aggregator`'s
   `aggregate-proof` bin — not fixed 256 B, and not Groth16-shape. Submitting
   the wrapper's output would revert on-chain at the SHPLONK pairing check.

4. Relayer `bin/relayer.rs:1461` (**withdraw** path, not verifyBlock) still
   parks proofs that aren't 256-byte Groth16:
   ```rust
   warn!(?e, proof = %proof_path.display(),
         "proof not 256-B Groth16 (gnark-wrap first); parking");
   ```
   Plus stale "Groth16 verifier triple" vocabulary in comments at
   `bin/relayer.rs:72, 138, 143, 148, 1062`. Vocabulary is cosmetic, but the
   size-gate at :1461 is a real footgun for anyone driving a live withdraw.

**Why this hasn't blown up in daily work.** The **daemon** path
(`relayer daemon-prover --enable-c12-aggregation ...`) is SHPLONK-native — it
uses `AggregatedBlockSource` + `Circuit12ShplonkPipeline` to run
`aggregate-proof` in-process per block and submits fresh SHPLONK calldata.
Nobody in the current workflow drives Sepolia through the one-shot script
anymore; live shellnet runs go through the daemon. But the script is still on
disk, still referenced in ops docs (I have not audited which), and still the
first thing someone would try on a cold Sepolia bring-up.

**Proposal.** Delete the dead pre-SHPLONK one-shot path in one atomic PR:

- `scripts/run_an_eth_e2e_sepolia.sh` — delete (Sepolia E2E is a daemon
  run, not a scripted one-shot; documenting the daemon command line in
  `docs/` is enough).
- `scripts/wrap_partner_proof_groth16.py` — delete.
- `scripts/wrap_proof_event_groth16.py` — audit + likely delete (same
  gnark-wrapper dependency; Circuit 4 is also SHPLONK on-chain).
- `bin/relayer.rs:1461` — replace the 256-B Groth16 gate with a SHPLONK
  shape check (or drop the gate entirely; live-computed calldata always
  satisfies the on-chain verifier since it comes from `aggregate-proof`
  which self-verifies against the committed `.bin`).
- `bin/relayer.rs:72, 138, 143, 148, 1062` — comment sweep: "Groth16
  verifier triple" → "SHPLONK aggregator" or equivalent.
- If `gnark-wrappers/circuit-1a` and `gnark-wrappers/circuit-2` are
  referenced from anywhere else (I haven't grepped exhaustively), those
  refs also go.

Alternative if the one-shot flow needs to survive for some ops reason I'm not
aware of: replace `wrap_partner_proof_groth16.py` with a
`wrap_partner_proof_shplonk.py` that runs `export-1a1b2-poseidon-snark` +
`aggregate-proof` × 3 and patches the per-block JSON with SHPLONK
(variable-length) calldata. I don't think this is worth the maintenance
because it duplicates what the daemon already does in-process — but flagging
in case there's an operator scenario I'm missing.

**Questions.**

1. Any objection to deleting the four script/gate items above?
2. Is `wrap_proof_event_groth16.py` (Circuit 4 event proof wrapper) also
   dead, or does that one still have a live consumer I'm missing?
3. Do any of your ops docs / runbooks still point at
   `run_an_eth_e2e_sepolia.sh`? If yes, they need pointing at the daemon
   invocation instead — happy to draft the swap once the script deletion
   lands.
4. Any git history you want preserved on the gnark-wrappers pre-deletion
   (they're not in the repo now, but they were at some point — worth a tag?).

