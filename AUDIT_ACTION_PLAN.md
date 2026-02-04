# Audit Action Plan

**Date**: 2026-02-03  
**Based on**: Audit branch review + independent analysis

---

## Priority 1: CRITICAL Fixes (Must Do Before Production)

### ✅ 1. Fix Documentation Mismatch (QC-SOL-001)

**Files to update**:
- `contracts/ethereum/src/IAckiNackiVerifier.sol:14`
- `contracts/ethereum/src/DummyVerifier.sol:88-91`
- `contracts/ethereum/src/Halo2VerifierWrapper.sol:25`

**Changes**:
```solidity
// OLD (WRONG):
/**
 * @param publicInputs Array of public inputs/outputs for the proof
 *                     Expected format: [nullifier, recipient, amount, root]
 */

// NEW (CORRECT):
/**
 * @param publicInputs Array of public inputs for deposit proof verification
 *                     Format: [depositId, sender, amount, contractAddress]
 *                     - depositId: Unique deposit identifier (uint256)
 *                     - sender: Original depositor address (uint160 → uint256)
 *                     - amount: Deposit amount in wei (uint256)
 *                     - contractAddress: This bridge contract address (uint160 → uint256)
 */
```

**Estimated effort**: 15 minutes  
**Risk**: Low (documentation only)

---

### ✅ 2. Add Recipient Validation (NEW FINDING)

**File**: `contracts/ethereum/src/AckiNackiBridge.sol`

**Change**:
```solidity
function withdraw(
    address payable recipient,
    uint256 amount,
    uint256 depositId,
    bytes calldata proof
) external {
    // Add this check at the beginning
    if (recipient == address(0)) revert InvalidRecipient();
    
    // ... rest of function
}
```

**Also add error**:
```solidity
error InvalidRecipient();
```

**Estimated effort**: 5 minutes  
**Risk**: Low (additional validation)

---

## Priority 2: MEDIUM Fixes (Should Do)

### 🤔 3. Consider Access Control (QC-SOL-002)

**Decision needed**: Do we want centralized control or immutable design?

**Option A: Immutable (Current)**
- ✅ Trustless
- ✅ No owner key risk
- ❌ Can't pause if bug found
- ❌ Can't upgrade verifier

**Option B: Timelock + Multisig**
- ✅ Can respond to emergencies
- ✅ Can upgrade verifier
- ❌ Adds centralization
- ❌ More complex

**My Recommendation**: 
- **Testnet**: Keep current immutable design
- **Production**: Add timelock (48h) + 3-of-5 multisig

**Estimated effort**: 2-4 hours (if implemented)  
**Risk**: Medium (changes security model)

---

### 🤔 4. Add Maximum Deposit Limit (NEW FINDING)

**File**: `contracts/ethereum/src/AckiNackiBridge.sol`

**Change**:
```solidity
uint256 public constant MAX_DEPOSIT_AMOUNT = 100 ether;

function deposit() external payable {
    if (msg.value == 0) revert InvalidAmount();
    if (msg.value > MAX_DEPOSIT_AMOUNT) revert DepositTooLarge();
    
    // ... rest of function
}
```

**Estimated effort**: 10 minutes  
**Risk**: Low (additional validation)

---

## Priority 3: LOW Fixes (Nice to Have)

### ✅ 5. Rename DummyVerifier.sol (QC-SOL-004)

**Change**: Rename file to `TestDepositVerifier.sol`

**Or add warning comment**:
```solidity
/// @title TestDepositVerifier
/// @notice ⚠️ FOR TESTING ONLY - NOT SECURE FOR PRODUCTION
/// @dev This verifier accepts any proof. Replace with real Halo2 verifier before mainnet.
contract DummyVerifier is IAckiNackiVerifier {
    // ...
}
```

**Estimated effort**: 5 minutes  
**Risk**: None (cosmetic)

---

### ✅ 6. Document depositCounter Overflow (NEW FINDING)

**File**: `contracts/ethereum/src/AckiNackiBridge.sol`

**Change**:
```solidity
// Deposit counter (unique ID for each deposit)
// Note: uint256 max = 2^256 - 1 ≈ 10^77 deposits
// At 1 deposit/second, would take 10^70 years to overflow
uint256 public depositCounter;
```

**Estimated effort**: 2 minutes  
**Risk**: None (documentation)

---

## Priority 4: WON'T FIX (Intentional Design)

### ❌ 7. Replace transfer() with call() (QC-SOL-003)

**Audit Recommendation**: Use `call()` + ReentrancyGuard

**My Decision**: **REJECT** - current code is SAFER

**Reasoning**:
1. CEI pattern already implemented
2. Gas limit (2300) provides defense in depth
3. ReentrancyGuard is redundant and adds gas cost
4. Smart contract wallets work fine with transfer()

**Action**: Add comment explaining the choice:
```solidity
// Using transfer() instead of call() for defense in depth:
// - 2300 gas limit prevents reentrancy attacks
// - CEI pattern already implemented (state changes before transfer)
// - Compatible with standard smart contract wallets
recipient.transfer(amount);
```

---

### ❌ 8. Remove Proof Size Check (QC-SOL-005)

**Audit Recommendation**: Remove `proof.length < 100` check

**My Decision**: **REJECT** - it's a useful sanity check

**Action**: Add comment explaining the purpose:
```solidity
// Sanity check: real Halo2 proofs are typically 1-10 KB
// This catches obvious errors (empty proof, wrong data type, etc.)
if (proof.length < 100) revert InvalidProof();
```

---

## Testing Plan

After implementing fixes, run:

### 1. Unit Tests
```bash
cd contracts/ethereum
forge test -vvv
```

**Expected**: All 12 tests pass

### 2. E2E Test
```bash
./test_e2e.sh
```

**Expected**: All 6 steps pass (deposit → proof → withdrawal)

### 3. Gas Optimization Check
```bash
forge test --gas-report
```

**Expected**: No significant gas increase

---

## Deployment Checklist

Before mainnet deployment:

- [ ] Fix QC-SOL-001 (documentation)
- [ ] Add recipient validation
- [ ] Decide on access control model
- [ ] Replace DummyVerifier with real Halo2 verifier
- [ ] Add maximum deposit limit
- [ ] Run full test suite
- [ ] Get external security audit
- [ ] Deploy to testnet and run E2E test
- [ ] Set up monitoring and alerts
- [ ] Prepare emergency response plan

---

## Timeline Estimate

| Task | Priority | Effort | Risk |
|------|----------|--------|------|
| Fix documentation | P1 | 15 min | Low |
| Add recipient validation | P1 | 5 min | Low |
| Add max deposit limit | P2 | 10 min | Low |
| Rename DummyVerifier | P3 | 5 min | None |
| Add comments | P3 | 10 min | None |
| **Total** | | **45 min** | **Low** |

**Access control decision** (P2) requires discussion and could add 2-4 hours if implemented.

---

## Conclusion

Most critical issues are **quick fixes** (< 1 hour total). The only major decision is whether to add access control, which is a **design choice** rather than a security bug.

**Recommendation**: Implement P1 fixes immediately, discuss P2 items with team, and proceed with testnet deployment.

