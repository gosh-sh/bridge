# NACKL Bridge extension — requirements

| | |
|---|---|
| Status | **Normative.** What the bridge must do to carry NACKL between Acki Nacki and an EVM chain, as set by gosh-sh/bridge-audit#152. Every requirement and property is covered by a test case listed in [§9](#9-traceability); §9 names the tests behind each test case ID (`TC-NB-…`): a test whose name starts with the case (`test_nb_an_lock_…` for TC-NB-AN-LOCK, `test_NB_EVM_LIST_…` for TC-NB-EVM-LIST), or the test named in the row. |
| Written against | `main` `40090b6c`. A `file:line` points at that commit and marks behaviour that stays as it is; a requirement without one is new. |
| Genre | Design and correctness. The verified contract reference is [`EVM-contracts-spec.md`](EVM-contracts-spec.md); the Acki Nacki contracts are described in [`contracts/an/README.md`](../contracts/an/README.md). |
| Paths | Relative to the repository root. |

## 1. Method

Requirements are written in **EARS** (Easy Approach to Requirements Syntax). Each one has an ID and
one of five forms:

| Form | Template | Becomes |
|---|---|---|
| Ubiquitous | The bridge shall … | a test that the behaviour holds |
| Event-driven | When ‹trigger›, the bridge shall … | a test that fires the trigger |
| State-driven | While ‹state›, the bridge shall … | a test in that state |
| Unwanted behaviour | If ‹condition›, then the bridge shall … | a negative test that forces the condition |
| Optional feature | Where ‹feature›, the bridge shall … | not used here |

System-wide properties (§8) are invariants and high-level properties; each becomes a Foundry
invariant, a fuzz test or a scenario test.

## 2. Glossary

| Term | Meaning |
|---|---|
| **NACKL** | The native currency of Acki Nacki: ECC currency id 1, nine decimal places. |
| **AN bridge** | `eccBridge` on Acki Nacki (`contracts/an/exchange/eccBridge.sol`). |
| **EVM bridge** | `AckiNackiBridge` on the EVM chain (`contracts/ethereum/src/AckiNackiBridge.sol`). |
| **NACKL token** | `NacklToken`, the ERC20 that represents NACKL on the EVM chain (`contracts/ethereum/src/NacklToken.sol`). Its owner is the minter. |
| **Minter** | The owner of the NACKL token: the one account that may mint it. |
| **Lock** | A NACKL withdrawal on Acki Nacki: the NACKL stays on the AN bridge instead of being burned. |
| **Unlock** | A NACKL deposit paid on Acki Nacki: the AN bridge sends NACKL it holds from earlier locks. |
| **Locked amount** | Per destination chain: NACKL the AN bridge holds from locks to that chain and has not unlocked. The registry field `outstanding` of that chain's NACKL entry. |
| **Locked to a bridge** | Per destination chain and EVM bridge: the total NACKL ever locked by withdrawals bound to that EVM bridge (never decreases). |
| **Refused deposit** | A proven deposit the AN bridge cannot pay when its voucher confirms it (for NACKL: the locked amount is short). The bridge pays nothing and releases the voucher, so the deposit can be finalized again. |
| **Payout** | The NACKL a proven withdrawal owes on the EVM chain. |
| **Daily limit** | `L`: the most NACKL the EVM bridge mints in any 24 hours. |
| **Mint window** | The NACKL minted in the current hour and the 24 hours before it (25 hourly buckets). Any 24-hour interval lies inside one such window, so keeping the window at or below `L` keeps every 24 hours at or below `L`. |
| **Handover** | Moving the minter role from an EVM bridge to the successor that names it as `predecessor`. |

## 3. Acki Nacki bridge (AN)

| ID | Requirement |
|---|---|
| NB-AN-1 | The AN bridge shall never mint or burn NACKL: no path calls `gosh.mintecc` or `gosh.burnecc` with currency id 1. |
| NB-AN-2 | When the owner calls `setBridgeToken` with currency id 1, the AN bridge shall register NACKL for that chain under the same rules as USDC (`eccBridge.sol:693-718`), except that the USDC bounds on `maxTransfer` and `maxTvl` do not apply. Any currency id other than 1 and 3 shall be refused with `ERR_TOKEN_CONFIG`. |
| NB-AN-3 | When `initiateWithdrawal` receives NACKL registered for the destination chain for a recipient of 20 bytes (the form Circuit 4 proves) and every check passes, the AN bridge shall keep the NACKL, add the amount to that chain's locked amount and to the amount locked to the chain's EVM bridge, and emit `WithdrawalInitiated` with the same fields as for USDC. |
| NB-AN-4 | If a NACKL withdrawal is above the entry's per-withdrawal cap, then the AN bridge shall refund it with `ERR_TRANSFER_TOO_LARGE` (234). If the chain's locked NACKL plus the withdrawal would exceed 10,000,000 NACKL (`MAX_NACKL_LOCKED`), then the AN bridge shall refund it with `ERR_TVL_CAP` (235). A NACKL withdrawal is not bounded by bridged inventory: `ERR_BURN_EXCEEDS_BRIDGED` (233) and `ERR_CHAIN_BALANCE_EXCEEDED` (236) do not apply to it. |
| NB-AN-5 | When the AN bridge confirms a NACKL deposit and the source chain's locked amount covers it, the AN bridge shall send that amount of NACKL it holds to the recipient and subtract it from the locked amount. |
| NB-AN-6 | If `finalizeDeposit` receives a NACKL deposit above the source chain's locked amount, then the AN bridge shall refuse it with `ERR_CHAIN_BALANCE_EXCEEDED` before accepting it, leaving the voucher unspent. If the voucher confirms a NACKL deposit that the locked amount no longer covers, then the AN bridge shall send nothing, emit `DepositRefused` with `ERR_CHAIN_BALANCE_EXCEEDED`, and call the voucher's `release`, which deletes the voucher, so the same deposit can be finalized again. |
| NB-AN-7 | The voucher shall send its confirmation with `bounce: false`, and shall delete itself on `release` only when the AN bridge sends it. A deposit that has been paid keeps its voucher, so finalizing it again pays nothing. |
| NB-AN-8 | The AN bridge shall send locked NACKL only by NB-AN-5. `mintAndSendTokenBridged` with currency id 1 shall be refused with `ERR_TOKEN_CONFIG`. |
| NB-AN-9 | A NACKL deposit shall pass `finalizeDeposit` under the entry's per-deposit cap and switch; the entry's `maxTvl` shall not bound it. |
| NB-AN-10 | When `updateCode` runs, the AN bridge shall carry the locked amounts and the amounts locked to each EVM bridge to the new code. An upgrade from 1.17.0 shall start with none of them. |
| NB-AN-11 | `getTotalBridged(1)` shall return `(0, 0)`: NACKL never enters the minted/burned inventory. |

The owner key of the AN bridge can replace its code with `updateCode` (`eccBridge.sol:1093`), and
the replacing code can send away every locked NACKL or, because the bridge is a special account
that may mint ECC, mint NACKL. Only a change in the node — refusing `MintToken` and `BurnToken`
with id 1 from this address — removes that power; it is outside this repository. NB-AN-1 and
NB-AN-8 hold for the code in this repository.

## 4. NACKL token (TOK)

| ID | Requirement |
|---|---|
| NB-TOK-1 | The NACKL token shall be an OpenZeppelin ERC20 with nine decimals, so an amount crosses the bridge unchanged. |
| NB-TOK-2 | Only the minter shall mint the NACKL token. |
| NB-TOK-3 | A holder, or an account the holder approved, shall be able to burn the holder's NACKL token (`burn`, `burnFrom`). |
| NB-TOK-4 | The production NACKL token shall be deployed with zero supply and the EVM bridge as minter. Only a test token shall be deployed with an initial supply. |

## 5. EVM bridge (EVM)

| ID | Requirement |
|---|---|
| NB-EVM-1 | When the owner calls `listNackl`, the EVM bridge shall list the NACKL token under currency id 1 with the given per-deposit cap and daily limit. It shall be refused unless the token is not yet listed, its minter is the EVM bridge or, before a handover, the bridge's `predecessor`, the token has nine decimals, and no other token holds id 1. |
| NB-EVM-2 | When `deposit` receives NACKL, the EVM bridge shall burn it from the depositor with `burnFrom` and emit `Deposit` with the same fields as for USDC. The per-deposit cap and the deposit switch apply; the TVL cap does not. |
| NB-EVM-3 | When `withdrawByProof` accepts a NACKL proof, the EVM bridge shall run every check it runs for USDC, record the nullifier and mint the payout to its recipient in the same transaction. |
| NB-EVM-4 | If a NACKL payout does not fit the mint window, then `withdrawByProof` shall revert with `NacklWindowFull`; if the bridge is not the token's minter (a successor before the handover), it shall revert with `NacklNotMinter`. The nullifier stays unspent. |
| NB-EVM-5 | When the same proof is sent again after such a revert and the payout fits, the EVM bridge shall pay it. There is no payout queue: a proof the bridge refused is retried by its sender or a relayer. |
| NB-EVM-6 | The EVM bridge shall mint no more than the daily limit in any mint window. |
| NB-EVM-7 | When the owner calls `listNackl` again with the listed token, the EVM bridge shall set the daily limit for payouts after the call. It shall refuse a daily limit below the NACKL per-withdrawal cap, so that every payout can fit an empty window. |
| NB-EVM-8 | The EVM bridge shall keep no payout pending: every NACKL proof it accepts is minted in the same transaction. |
| NB-EVM-9 | While the EVM bridge is paused, `deposit` and `withdrawByProof` shall revert. |
| NB-EVM-10 | When the owner calls `migrateToken(1, successor)`, the EVM bridge shall close NACKL deposits on itself and make `successor` the minter. It shall be refused unless this bridge is paused, `successor` names it as `predecessor` and lists the same NACKL token. |
| NB-EVM-11 | After a handover, a NACKL `withdrawByProof` on the old EVM bridge shall revert with `NacklMinterGone`, and `deposit` of NACKL into it shall revert with `TokenDepositsDisabled`, whatever `setTokenDepositsEnabled` says. |
| NB-EVM-12 | The USDC paths shall be unchanged: a USDC deposit and a USDC withdrawal move the same balances and emit the same events as on `40090b6c`. |

## 6. Handover procedure (HO)

A withdrawal proof is bound to one EVM bridge (bridge-audit#140), so a withdrawal bound to the old
bridge can be paid only by the old bridge. The EVM chain cannot see which withdrawals Acki Nacki
bound to it; the handover precondition is checked by comparing two counters.

| ID | Requirement |
|---|---|
| NB-HO-1 | The AN bridge shall report, per chain and EVM bridge, the NACKL locked to that bridge (`getNacklLockedTo`). |
| NB-HO-2 | The EVM bridge shall report the NACKL it has minted (`nacklState`). |
| NB-HO-3 | The operator shall hand the minter over only when, for the old bridge, the NACKL locked to it on Acki Nacki equals the NACKL it has minted (`nacklState().minted`). The order is: `setEvmBridge` to the successor on Acki Nacki; let the old bridge pay out; pause it; `migrateToken(1, successor)`. |

## 7. Unchanged parts (CR)

| ID | Requirement |
|---|---|
| NB-CR-1 | The circuits, their verification keys and the public inputs of a withdrawal proof shall not change. |
| NB-CR-2 | `Deposit` on the EVM chain and `WithdrawalInitiated` on Acki Nacki shall keep the encodings the circuits and `finalizeDeposit` bind (`AckiNackiBridge.sol:399-407`, `eccBridge.sol:64-71`). |

## 8. Properties

| ID | Property |
|---|---|
| NB-P-1 | (AN) For every chain, the NACKL the AN bridge holds is at least the sum over chains of the locked amounts. |
| NB-P-2 | (AN) Locked amounts change only by a lock (up) and an unlock (down); an unlock never takes one below zero. |
| NB-P-3 | (EVM) The NACKL token supply minted by the EVM bridge equals the NACKL paid by proofs it accepted; a refused proof mints nothing and spends no nullifier. |
| NB-P-4 | (EVM) In every mint window the minted amount is at most the daily limit. |
| NB-P-5 | (EVM) A nullifier pays at most once. |
| NB-P-6 | (end to end) NACKL on the EVM chain comes only from NACKL locked on Acki Nacki: deposits unlock no more than earlier withdrawals locked. |

## 9. Traceability

| Requirement | Test case | Where |
|---|---|---|
| NB-AN-1, NB-AN-8 | TC-NB-AN-NOMINT | `scripts/test_an_bridge_limits.py`: `test_nb_an_owner_cannot_mint_or_take_nackl`, `test_nb_an_lock_keeps_nackl_and_counts_it` |
| NB-AN-2 | TC-NB-AN-REGISTER | `scripts/test_an_bridge_limits.py` |
| NB-AN-3, NB-HO-1 | TC-NB-AN-LOCK | `scripts/test_an_bridge_limits.py` (including `test_nb_an_lock_needs_an_evm_recipient`) |
| NB-AN-4 | TC-NB-AN-CAP | `scripts/test_an_bridge_limits.py`: `test_nb_an_lock_is_capped_per_withdrawal_only`, `test_nb_an_lock_stops_at_ten_million_nackl` |
| NB-AN-5 | TC-NB-AN-UNLOCK | `scripts/test_an_bridge_limits.py` |
| NB-AN-6, NB-AN-7 | TC-NB-AN-REFUSED | `scripts/test_an_bridge_limits.py`: `test_nb_an_short_lock_refuses_the_confirmation_until_it_is_covered`, `test_nb_an_a_later_deposit_is_paid_while_an_earlier_one_is_refused`, `test_voucher_deletes_itself_only_when_the_bridge_releases_it` |
| NB-AN-9 | TC-NB-AN-DEPOSIT-CAPS | `scripts/test_an_bridge_limits.py`: `test_nb_an_nackl_deposit_ignores_max_tvl` |
| NB-AN-10 | TC-NB-AN-UPGRADE | `scripts/test_an_bridge_limits.py` |
| NB-AN-11 | TC-NB-AN-INVENTORY | `scripts/test_an_bridge_limits.py`: `test_nb_an_lock_keeps_nackl_and_counts_it`, `test_nb_an_unlock_pays_from_the_lock` |
| NB-TOK-1…4 | TC-NB-TOK-* | `contracts/ethereum/test/NacklToken.t.sol` |
| NB-EVM-1 | TC-NB-EVM-LIST | `contracts/ethereum/test/NacklBridge.t.sol` |
| NB-EVM-2 | TC-NB-EVM-DEPOSIT | `contracts/ethereum/test/NacklBridge.t.sol` |
| NB-EVM-3 | TC-NB-EVM-WITHDRAW-* | `contracts/ethereum/test/NacklBridge.t.sol` |
| NB-EVM-4, NB-EVM-5 | TC-NB-EVM-WITHDRAW-* | `contracts/ethereum/test/NacklBridge.t.sol`: `test_NB_EVM_WITHDRAW_overTheLimitIsRefusedAndCanBeSentAgain`, `test_NB_HO_3_aSuccessorRefusesPayoutsUntilItIsTheMinter` |
| NB-EVM-6, NB-P-4 | TC-NB-EVM-WINDOW, TC-NB-P-WINDOW | `NacklBridge.t.sol`, `NacklBridgeInvariant.t.sol`: `invariant_NB_P_4_any24HoursWithinTheLimit` |
| NB-EVM-7 | TC-NB-EVM-LIMITS | `contracts/ethereum/test/NacklBridge.t.sol` |
| NB-EVM-8 | TC-NB-EVM-WITHDRAW-* | `contracts/ethereum/test/NacklBridge.t.sol`: `test_NB_EVM_WITHDRAW_payoutIsMintedAtOnce` |
| NB-EVM-9 | TC-NB-EVM-PAUSE | `contracts/ethereum/test/NacklBridge.t.sol` |
| NB-EVM-10, NB-EVM-11 | TC-NB-EVM-HANDOVER-* | `contracts/ethereum/test/NacklBridge.t.sol` |
| NB-EVM-12 | the existing USDC suites | `contracts/ethereum/test/`, and `test_NB_EVM_12_usdcStillPaysFromCustody` |
| NB-HO-2 | TC-NB-EVM-STATE | `contracts/ethereum/test/NacklBridge.t.sol` |
| NB-HO-3 | TC-NB-HO-PROCEDURE | `contracts/ethereum/test/NacklBridge.t.sol`: `test_NB_HO_3_procedureHandsOverOnlyWhenEverythingBoundIsPaid`, `test_NB_HO_3_earlyHandoverStrandsAWithdrawalOfTheOldBridge`, `test_NB_HO_3_aSuccessorRefusesPayoutsUntilItIsTheMinter` |
| NB-P-6 | TC-NB-E2E | `tests/e2e/local-net/arbitrum/test_arbitrum_e2e.py`: `test_45_nackl_locked_on_acki_nacki`, `test_46_nackl_deposit_sent`, `test_66_nackl_deposit_delivered`, `test_74_nackl_minted`, `test_75_nackl_reconciles` |
| NB-CR-1, NB-CR-2 | TC-NB-CR | the check `scripts/check_withdrawal_event_header.py`, and no change in the circuit crates |
| NB-P-1, NB-P-2 | TC-NB-P-AN | `scripts/test_an_bridge_limits.py`: `test_nb_an_unlock_pays_from_the_lock`, `test_nb_an_lock_of_another_chain_does_not_pay`, `test_nb_an_finalize_refuses_a_deposit_above_the_lock` |
| NB-P-3, NB-P-5 | TC-NB-P-SUPPLY | `NacklBridgeInvariant.t.sol`: `invariant_NB_P_3_supplyIsWhatProofsPaid`; `NacklBridge.t.sol`: `test_NB_EVM_WITHDRAW_overTheLimitIsRefusedAndCanBeSentAgain` |
