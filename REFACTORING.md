# Refactoring register

Things worth changing that are **not bugs**: the code does what it should, but a
name lies, one fact is written down twice, or a value that should adapt is a
literal. None of these block a release. All of them have cost someone time, or
are set up to.

**Verified against `b5928a6c`.** Every entry cites `file:line`; re-check before
acting, and delete an entry once it is done rather than marking it done.

This is a backlog, not a plan. An entry here has not been scheduled, and
nothing here is an instruction to change the code on sight — several of these
are deliberate and need a decision before they are touched.

---

## 1. `num_active_layers` promises something it does not mean

**Where:** `crates/bridge-prover-libraries/bridge-prover-lib/src/bridge_state.rs`
and 5 other files — 22 uses across the prover library, the event witness, the
AN→ETH relayer and the end-user CLI. All of it internal Rust; the name does not
cross an ABI boundary.

The value is the highest layer window that has ever been filled. It never
decreases, because nothing clears a window. The name says "active", which reads
as "in use right now" — and a layer untouched for thousands of blocks does not
look active.

Getting this wrong is expensive and silent: the prover picks the wrong previous
anchor, the contract rejects with `PrevAnchorMismatch` **before** verifying any
proof, and minutes to hours of proving are thrown away. PR #117 answers it with
a comment and a test; the name itself still says the wrong thing.

The Solidity side is honest — `numLayers`, no claim about activity — but its
NatSpec repeats it: "Number of active layer slots in the new commitment"
(`contracts/ethereum/src/AckiNackiBridge.sol:351`).

**Shape of the change:** rename the Rust side to something that states the
meaning (`highest_populated_layer`), and fix the Solidity comment. The compiler
catches every Rust call site, so the rename is mechanical. Renaming the Solidity
**parameter** is not: `numLayers` is in a function signature and in
`InvalidNumLayers`, so it is part of the ABI that generated bindings and tooling
read. Leave the parameter, fix the prose.

---

## 2. One EVM deployment address, written down in two places

**Where:** `crates/ackinacki-bridge/config/bridge_config.shellnet:180`
(`BRIDGE_ADDRESS`) and `frontend/src/config.rs:7`
(`BRIDGE_CONTRACT_ADDRESS`). The deposit relayer and the AN→ETH relayer carry a
third and fourth copy in `scripts/ursus/*.env.example`.

Two clients can deposit — the CLI and the web form — and each pins the bridge
address itself. Nothing compares them.

This has already cost a week. The bridge moved to `0x5384f162…fad5`; the CLI
profile was updated in #111, the web form in #118 seven days later. In between,
the form sent deposits to a bridge nobody advanced and asked for an approval on
a token the depositor did not hold. The three deposits that went through in that
window all came from the CLI.

**Shape of the change:** one source of truth, or a check that fails when the
copies disagree. The precedent exists: `scripts/check_bridge_abi_in_sync.sh`
does exactly this for the two `USDCBridge.abi.json` copies — see entry 5 for the
part of that precedent worth learning from.

---

## 3. The two directions keep separate lists of supported chains

**Where:** `crates/deposit-chain-ids/src/lib.rs` (deposit side, the stated
single source of truth) and `crates/ackinacki-bridge/src/args.rs:40-45`
(withdrawal side, `SUPPORTED_CHAINS`).

**They disagree today.** Both lists now carry Arbitrum One (42161). The
deposit allowlist still has OP Mainnet, World Chain, Mantle and Blast, and
the withdrawal list still has Ethereum mainnet and Base Sepolia, so a chain
can be accepted in one direction and refused in the other. Tracked as
`gosh-sh/bridge-audit#119`.

The duplication is deliberate and documented — `args.rs:37-39` carries a
`TODO(v2)` saying the list is copied "to keep the dep footprint tight and avoid
a cross-workspace path". That reasoning was sound when written; the lists have
since drifted, which is what the TODO was warning about.

**Shape of the change:** depend on `deposit-chain-ids` from the CLI, or add a
test that fails when the two lists disagree. The second is cheaper and keeps the
dependency graph as it is.

---

## 4. Gas amounts in the deposit path are literals

**Where:** `contracts/an/exchange/eccUSDCBridge.sol:695,875` (`value: 2 vmshell`
on the `DepositVoucher` deploy) and
`contracts/an/exchange/DepositVoucher.sol:44` (`value: 0.5 vmshell` on the
`confirmDeposit` callback).

The bridge itself cannot run out: `ensureBalance()` mints vmshell when its
balance falls below `MIN_BALANCE` (`eccUSDCBridge.sol:406-409`). The voucher has
no such floor — it executes on the 2 vmshell it was deployed with.

These numbers do not adapt. A deposit whose mint leg runs out of gas leaves the
voucher deployed and the credit unmade, which is the state behind CLI exit 37:
the anti-replay slot is spent, so resending the same proof is a no-op, and the
EVM side has no refund path. Today that state is reached through a release
mistake (see `.onboarding/decisions.md`, 2026-10-20); a change in network gas
pricing could reach it without anyone touching this repository.

**Shape of the change:** establish what these numbers need to cover and whether
they still do, then either widen them with the reasoning written down, or derive
them. Measuring first is the cheap half.

---

## 5. Guard scripts that no pipeline runs

**Where:** the pattern, not one file. `scripts/check_bridge_abi_in_sync.sh` was
written to catch drift between the two `USDCBridge.abi.json` copies, was wired
into no pipeline and no `Makefile` target, and the drift it existed to catch
landed anyway — 14 functions and a `confirmDeposit` missing `chainId`. Fixed in
#98 by running it in `.woodpecker/an-contracts.yaml`.

A guard nobody runs is worse than no guard: it reads like coverage.

**Shape of the change:** a check that every `scripts/check_*.{sh,py}` is
reachable from a pipeline or a `Makefile` target, and fails when one is not.

---

## 6. Owner mint has no ceiling and no expiry

**Where:** `contracts/an/exchange/eccUSDCBridge.sol` — `mintAndSend` and
`mintAndSendBridged` (added in #119).

Both let the owner key create eccUSDC with nothing behind it. They exist to
repair a deposit stuck in the state described in entry 4, which is a real need
while the code is still changing.

**This is a recorded decision, not an oversight** (`.onboarding/decisions.md`,
2026-10-20): the lever stays, and is revisited **when the bridge enters its
mature phase** — not tied to a mainnet launch.

**Shape of the change, when that time comes:** bound it rather than remove it —
permit a mint only against a deposit whose `DepositVoucher` is deployed and
whose `confirmDeposit` did not complete. The owner then cannot mint freely, and
a stuck deposit can still be repaired.
