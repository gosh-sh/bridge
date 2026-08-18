> **⚠️ ARCHIVED 2026-08-18 — not maintained, not authoritative.**
> Parts of this document are contradicted by the current code. Do not act on it, and do not cite it
> from anything new. Authority is the source tree, plus `docs/ETH-contracts-spec.md` for the
> Ethereum contracts. Kept only as source material while the documentation is rewritten (see
> `DOCS.md` at the repository root); this folder is scheduled for deletion.

# Task for Cursor Agent — Clean up misleading Withdraw flow in the bridge frontend

The frontend (Yew/WASM) advertises a withdrawal feature that does NOT exist on
the deployed contract. Real withdrawals were removed in Phase 4.3; burn-proof
withdrawals are still under development. The current UI can make a user believe
funds were returned when nothing happened. Fix the following.

## Scope (files)
- frontend/src/components/withdraw_form.rs
- frontend/src/components/deposit_form.rs
- frontend/src/config.rs
- frontend/src/components/transaction_history.rs

## Required changes

1. withdraw_form.rs
   - This component is a SIMULATION: it runs timer-based fake "proof generation"
     and shows "Withdrawal Successful!" while moving no funds.
   - Either remove the component entirely, OR replace its body with a clearly
     labeled, disabled "Coming soon" notice (no fake proof, no success message,
     no inputs that imply funds will move). Pick removal unless it breaks routing;
     if it would break navigation, keep a disabled placeholder.
   - Remove any nav/menu/route entry that exposes a working withdraw screen.

2. deposit_form.rs
   - Remove copy that promises "withdrawal" and "Deposit ID for withdrawal".
   - Deposit IDs / receipts should be described only as deposit references,
     not as something usable to withdraw.

3. config.rs
   - Remove ABI entries that are not in the deployed contract's public surface:
     `withdraw`, the `Withdrawal` event, and `totalWithdrawn`.
   - Keep deposit-related ABI intact (deposit(), Deposit event, limits, pause).

4. transaction_history.rs
   - Remove hardcoded mock data (the fake Withdraw transaction).
   - Show an empty state or real data only; never display fabricated history.

## Acceptance criteria
- No code path shows "Withdrawal Successful!" or simulated proof progress.
- No UI text implies a user can withdraw / get funds back right now.
- ABI in config.rs matches the deployed contract's public functions/events.
- `cargo build` (or the project's wasm build, see build.sh / Makefile) succeeds.
- Run `cargo fmt` and fix any clippy warnings you introduce.

## Do not
- Do not touch contracts/ or prover crates.
- Do not invent a working withdraw implementation.
