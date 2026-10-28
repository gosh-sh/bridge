// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "forge-std/Test.sol";

import "../src/AckiNackiBridge.sol";
import "../src/MockBlockHeaderOracle.sol";
import "../src/IPrimaryVerifier.sol";
import "../src/IFallbackVerifier.sol";
import "../src/ILayerHashesMovementVerifier.sol";
import "../src/IBridgeWithdrawalVerifier.sol";

import "./helpers/VerifyBlockConfigLib.sol";
import "./helpers/UsdcTestLib.sol";
import "./helpers/Bn254FrLib.sol";
import "./mocks/MockPrimaryVerifier.sol";
import "./mocks/MockFallbackVerifier.sol";
import "./mocks/MockLayerHashesMovementVerifier.sol";
import "./mocks/MockBridgeWithdrawalVerifier.sol";
import "./mocks/MockERC20.sol";
import "./mocks/MockAave.sol";

/// @notice A token that keeps a cut of every transfer. The bridge must refuse
///         it: the custody delta would not match the booked amount.
contract FeeOnTransferERC20 {
    string public name = "Fee";
    string public symbol = "FEE";
    uint8 public decimals = 18;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        return _move(msg.sender, to, amount);
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        allowance[from][msg.sender] -= amount;
        return _move(from, to, amount);
    }

    function _move(address from, address to, uint256 amount) internal returns (bool) {
        uint256 fee = amount / 100;
        balanceOf[from] -= amount;
        balanceOf[to] += amount - fee;
        return true;
    }
}

/// @title AckiNackiBridgeTokenRegistryTest
/// @notice The owner-managed ERC-20 allowlist: who may list, what a listing
///         must satisfy, how liabilities are tracked per token, and which
///         token a withdrawal proof is paid in.
contract AckiNackiBridgeTokenRegistryTest is Test {
    AckiNackiBridge internal bridge;
    MockBlockHeaderOracle internal oracle;
    MockERC20 internal usdc;
    MockERC20 internal dai;
    MockPrimaryVerifier internal primaryVerifier;
    MockFallbackVerifier internal fallbackVerifier;
    MockLayerHashesMovementVerifier internal layerHashesVerifier;
    MockBridgeWithdrawalVerifier internal withdrawalVerifier;

    uint256 internal constant BK_SET = 0xBE5E7;
    uint256 internal constant GENESIS_PREV_ANCHOR = 0xA10C;
    uint8 internal constant ACTIVE_LAYERS = 3;
    uint256 internal constant FIRST_BLOCK_ID = 0xC10C40001;
    uint64 internal constant FIRST_SEQ_NO = 1;
    uint256 internal constant DAPP_FR = 0xD499F4CEC0FFEE01;
    uint256 internal constant ACC_FR = 0xAC0F4CEDEADBEEF1;
    uint256 internal constant RECIPIENT_HALF_MASK = (1 << 80) - 1;

    /// @dev The ECC currency id the AN side would mint for the second token.
    uint32 internal constant DAI_TOKEN_ID = 4;

    address internal constant RECIPIENT = address(0x1111111111111111111111111111111111111111);
    address internal funder = address(0xF00D);
    address internal nonOwner = address(0xBAD);

    uint256 internal seedAnchor;

    // Local copies of the bridge's events, declared here so `expectEmit` has a
    // signature to encode. Emitting the bridge's own declarations from this
    // contract pulls their natspec into this contract's documentation, which
    // trips an internal solc assertion.
    event TokenListed(
        uint32 indexed tokenId,
        address indexed token,
        uint8 decimals,
        uint256 maxTransfer,
        uint256 tvlCap,
        bool depositsEnabled
    );
    event Deposit(
        uint256 indexed depositId,
        address indexed sender,
        uint256 amount,
        bytes32 anAccount,
        uint256 timestamp,
        address token
    );

    function setUp() public {
        oracle = new MockBlockHeaderOracle();
        usdc = new MockERC20("Mock USDC", "mUSDC", 6);
        dai = new MockERC20("Mock DAI", "mDAI", 18);
        primaryVerifier = new MockPrimaryVerifier();
        fallbackVerifier = new MockFallbackVerifier();
        layerHashesVerifier = new MockLayerHashesMovementVerifier();
        withdrawalVerifier = new MockBridgeWithdrawalVerifier();

        primaryVerifier.setShouldAccept(true);
        fallbackVerifier.setShouldAccept(true);
        layerHashesVerifier.setShouldAccept(true);
        withdrawalVerifier.setShouldAccept(true);

        bridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(0),
            address(0),
            VerifyBlockConfigLib.with(
                IPrimaryVerifier(address(primaryVerifier)),
                IFallbackVerifier(address(fallbackVerifier)),
                ILayerHashesMovementVerifier(address(layerHashesVerifier)),
                BK_SET,
                GENESIS_PREV_ANCHOR
            ),
            VerifyBlockConfigLib.withWithdraw(
                IBridgeWithdrawalVerifier(address(withdrawalVerifier)), DAPP_FR, ACC_FR
            )
        );

        seedAnchor = _seedFirstBlock();
    }

    // -----------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------

    /// @dev A deployment that makes no owner call must behave exactly as the
    ///      single-token bridge did: USDC listed, same caps, deposits open.
    function test_constructor_listsUsdcWithTheOldConstants() public {
        (
            address token,
            uint8 decimals,
            bool depositsEnabled,
            uint256 maxTransfer,
            uint256 tvlCap,
            uint256 liabilities
        ) = bridge.tokenInfo(bridge.usdcTokenId());

        assertEq(token, address(usdc), "USDC listed under usdcTokenId");
        assertEq(decimals, 6);
        assertTrue(depositsEnabled);
        assertEq(maxTransfer, bridge.MAX_TRANSFER_AMOUNT());
        assertEq(tvlCap, bridge.MAX_BRIDGE_TVL());
        assertEq(liabilities, 0);

        (bool listed, uint32 id) = bridge.tokenIdOf(address(usdc));
        assertTrue(listed);
        assertEq(id, bridge.usdcTokenId());

        uint32[] memory ids = bridge.listedTokenIds();
        assertEq(ids.length, 1);
        assertEq(ids[0], bridge.usdcTokenId());
    }

    /// @dev `liabilitiesOf` must read `treasuryBalance` for USDC, not the
    ///      registry counter — the AAVE and yield math is written against
    ///      that slot, so a second counter would silently disagree with it.
    function test_usdcLiabilitiesTrackTreasuryBalance() public {
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 7 * UsdcTestLib.UNIT);
        assertEq(bridge.treasuryBalance(), 7 * UsdcTestLib.UNIT);
        assertEq(bridge.liabilitiesOf(bridge.usdcTokenId()), 7 * UsdcTestLib.UNIT);
    }

    // -----------------------------------------------------------------
    // listToken: access and validation
    // -----------------------------------------------------------------

    function test_listToken_onlyOwner() public {
        vm.prank(nonOwner);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
    }

    function test_listToken_rejectsZeroAddress() public {
        vm.expectRevert(AckiNackiBridge.InvalidTokenAddress.selector);
        bridge.listToken(address(0), DAI_TOKEN_ID, 18, 1e18, 10e18);
    }

    /// @dev The aToken is the yield book's own unit: `aUsdcBalance()` feeds
    ///      `_backedPrincipal`, `accruedYield` and `harvestYield`. Listing it
    ///      would let a deposit raise that balance without raising
    ///      `suppliedPrincipal`, so user principal would read back as
    ///      harvestable interest.
    function test_listToken_rejectsTheAaveAToken() public {
        MockAUSDC aUsdc = new MockAUSDC();
        MockAavePool pool = new MockAavePool(address(usdc), address(aUsdc));
        AckiNackiBridge aaveBridge = new AckiNackiBridge(
            address(oracle),
            address(usdc),
            address(pool),
            address(aUsdc),
            VerifyBlockConfigLib.disabled(),
            VerifyBlockConfigLib.disabledWithdraw()
        );

        vm.expectRevert(AckiNackiBridge.CannotListAToken.selector);
        aaveBridge.listToken(address(aUsdc), DAI_TOKEN_ID, 6, 1e6, 10e6);

        // An unrelated token still lists on the same bridge.
        aaveBridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        (address listedToken,,,,,) = aaveBridge.tokenInfo(DAI_TOKEN_ID);
        assertEq(listedToken, address(dai));
    }

    function test_listToken_rejectsTokenListedTwice() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.TokenAlreadyListed.selector, address(dai), DAI_TOKEN_ID
            )
        );
        bridge.listToken(address(dai), DAI_TOKEN_ID + 1, 18, 1e18, 10e18);
    }

    function test_listToken_rejectsIdTakenByAnotherToken() public {
        // `usdcTokenId()` is read first: a call inside the `expectRevert`
        // argument would itself be the "next call" the cheatcode applies to.
        uint32 takenId = bridge.usdcTokenId();
        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.TokenIdAlreadyUsed.selector, takenId, address(usdc)
            )
        );
        bridge.listToken(address(dai), takenId, 18, 1e18, 10e18);
    }

    function test_listToken_rejectsZeroMaxTransfer() public {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TokenLimitsInvalid.selector, 0, 10e18)
        );
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 0, 10e18);
    }

    function test_listToken_rejectsMaxTransferAboveTvlCap() public {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TokenLimitsInvalid.selector, 10e18, 1e18)
        );
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 10e18, 1e18);
    }

    /// @dev The decisive limit for a high-decimal token. `gosh.mintecc` takes a
    ///      uint64 and the AN bridge requires `fr[2] <= type(uint64).max`, so a
    ///      listing may not promise a deposit AN cannot mint. One whole
    ///      18-decimal token (1e18) is inside the ceiling; a hundred are not.
    function test_listToken_rejectsMaxTransferAboveMintCeiling() public {
        uint256 ceiling = bridge.MAX_DEPOSIT_AMOUNT();
        assertLt(ceiling, 100e18, "uint64 ceiling is below 100 whole 18-decimal tokens");

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.TokenLimitExceedsMintCeiling.selector, ceiling + 1, ceiling
            )
        );
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, ceiling + 1, type(uint256).max);
    }

    function test_listToken_acceptsMaxTransferAtTheCeiling() public {
        uint256 ceiling = bridge.MAX_DEPOSIT_AMOUNT();
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, ceiling, ceiling);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        (,,, uint256 maxTransfer,,) = bridge.tokenInfo(DAI_TOKEN_ID);
        assertEq(maxTransfer, ceiling);
    }

    /// @dev A new listing must not accept deposits the moment it lands. The
    ///      Acki Nacki side mints `USDC_ECC_ID` for every proof it accepts
    ///      whatever token the proof carries, so a listing that opened
    ///      deposits in its own transaction would leave a permissionless lane
    ///      open until a second transaction closed it — and a deposit made in
    ///      that window stays provable afterwards.
    function test_listToken_startsWithDepositsClosed() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);

        (,, bool depositsEnabled,,,) = bridge.tokenInfo(DAI_TOKEN_ID);
        assertFalse(depositsEnabled, "a fresh listing must not take deposits");

        dai.mint(funder, 1e18);
        vm.startPrank(funder);
        dai.approve(address(bridge), 1e18);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TokenDepositsDisabled.selector, DAI_TOKEN_ID)
        );
        bridge.deposit(address(dai), 1e18, bytes32(uint256(1)));
        vm.stopPrank();

        // Withdrawals by proof are unaffected, and the owner opens deposits
        // once an Acki Nacki deployment credits this id.
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        _depositDai(funder, 1e18);
        assertEq(bridge.liabilitiesOf(DAI_TOKEN_ID), 1e18);
    }

    /// @dev The constructor's own USDC listing is the exception: the Acki
    ///      Nacki side credits that id today, so it is open from birth.
    function test_constructorListsUsdcWithDepositsOpen() public view {
        (,, bool depositsEnabled,,,) = bridge.tokenInfo(bridge.usdcTokenId());
        assertTrue(depositsEnabled, "USDC must be depositable from deployment");
    }

    function test_listToken_emitsTokenListed() public {
        vm.expectEmit(true, true, false, true);
        emit TokenListed(DAI_TOKEN_ID, address(dai), 18, 1e18, 10e18, false);
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
    }

    // -----------------------------------------------------------------
    // deposit
    // -----------------------------------------------------------------

    function test_deposit_unlistedTokenReverts() public {
        dai.mint(funder, 1e18);
        vm.startPrank(funder);
        dai.approve(address(bridge), 1e18);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TokenNotListed.selector, address(dai))
        );
        bridge.deposit(address(dai), 1e18, bytes32(uint256(1)));
        vm.stopPrank();
    }

    function test_deposit_secondTokenCreditsItsOwnLiabilities() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        _depositDai(funder, 1e18);

        assertEq(bridge.liabilitiesOf(DAI_TOKEN_ID), 1e18);
        assertEq(bridge.treasuryBalance(), 0, "a DAI deposit must not move the USDC book");
        assertEq(dai.balanceOf(address(bridge)), 1e18);
    }

    function test_deposit_emitsTheDepositedToken() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        dai.mint(funder, 1e18);

        vm.startPrank(funder);
        dai.approve(address(bridge), 1e18);
        vm.expectEmit(true, true, false, true);
        emit Deposit(0, funder, 1e18, bytes32(uint256(1)), block.timestamp, address(dai));
        bridge.deposit(address(dai), 1e18, bytes32(uint256(1)));
        vm.stopPrank();
    }

    function test_deposit_perTokenCapsAreIndependent() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 1e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        _depositDai(funder, 1e18);

        // DAI is now at its cap...
        dai.mint(funder, 1);
        vm.startPrank(funder);
        dai.approve(address(bridge), 1);
        vm.expectRevert(AckiNackiBridge.TvlCapReached.selector);
        bridge.deposit(address(dai), 1, bytes32(uint256(1)));
        vm.stopPrank();

        // ...while USDC is untouched and still accepts deposits.
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 1 * UsdcTestLib.UNIT);
        assertEq(bridge.treasuryBalance(), 1 * UsdcTestLib.UNIT);
    }

    function test_deposit_respectsPerTokenMaxTransfer() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 100e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        dai.mint(funder, 2e18);
        vm.startPrank(funder);
        dai.approve(address(bridge), 2e18);
        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        bridge.deposit(address(dai), 2e18, bytes32(uint256(1)));
        vm.stopPrank();
    }

    /// @dev The exact-balance check has to cover every listed token, not just
    ///      USDC — otherwise listing a fee-on-transfer token books more than
    ///      the bridge received.
    function test_deposit_feeOnTransferTokenIsRefused() public {
        FeeOnTransferERC20 fee = new FeeOnTransferERC20();
        bridge.listToken(address(fee), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);

        fee.mint(funder, 1e18);
        vm.startPrank(funder);
        fee.approve(address(bridge), 1e18);
        vm.expectRevert(AckiNackiBridge.TransferAmountMismatch.selector);
        bridge.deposit(address(fee), 1e18, bytes32(uint256(1)));
        vm.stopPrank();

        assertEq(bridge.liabilitiesOf(DAI_TOKEN_ID), 0);
    }

    // -----------------------------------------------------------------
    // Limits, pausing deposits, delisting
    // -----------------------------------------------------------------

    function test_setTokenLimits_appliesAndOnlyOwner() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);

        vm.prank(nonOwner);
        vm.expectRevert(AckiNackiBridge.NotOwner.selector);
        bridge.setTokenLimits(DAI_TOKEN_ID, 2e18, 20e18);

        bridge.setTokenLimits(DAI_TOKEN_ID, 2e18, 20e18);
        (,,, uint256 maxTransfer, uint256 tvlCap,) = bridge.tokenInfo(DAI_TOKEN_ID);
        assertEq(maxTransfer, 2e18);
        assertEq(tvlCap, 20e18);
    }

    function test_setTokenLimits_unlistedIdReverts() public {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.UnsupportedTokenId.selector, DAI_TOKEN_ID)
        );
        bridge.setTokenLimits(DAI_TOKEN_ID, 1e18, 10e18);
    }

    /// @dev Closing deposits must not block withdrawals — that is the only
    ///      safe way to retire a token that the bridge still owes.
    function test_setTokenDepositsEnabled_closesDepositsOnly() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        _depositDai(funder, 1e18);

        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, false);

        dai.mint(funder, 1e18);
        vm.startPrank(funder);
        dai.approve(address(bridge), 1e18);
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.TokenDepositsDisabled.selector, DAI_TOKEN_ID)
        );
        bridge.deposit(address(dai), 1e18, bytes32(uint256(1)));
        vm.stopPrank();

        // The outstanding balance is still payable.
        uint256 before = dai.balanceOf(RECIPIENT);
        bridge.withdrawByProof(
            _dummyProof(),
            _pubFor(DAI_TOKEN_ID, 1e18, Bn254FrLib.toFr(uint256(keccak256("daiOut"))))
        );
        assertEq(dai.balanceOf(RECIPIENT) - before, 1e18);
    }

    function test_delistToken_refusesWhileLiabilitiesRemain() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        _depositDai(funder, 1e18);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.TokenStillHasLiabilities.selector, DAI_TOKEN_ID, 1e18
            )
        );
        bridge.delistToken(DAI_TOKEN_ID);
    }

    function test_delistToken_refusesForUsdc() public {
        uint32 id = bridge.usdcTokenId();
        vm.expectRevert(AckiNackiBridge.CannotDelistUsdc.selector);
        bridge.delistToken(id);
    }

    function test_delistToken_clearsBothDirections() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        assertEq(bridge.listedTokenIds().length, 2);

        bridge.delistToken(DAI_TOKEN_ID);

        (address token,,,,,) = bridge.tokenInfo(DAI_TOKEN_ID);
        assertEq(token, address(0));
        (bool listed,) = bridge.tokenIdOf(address(dai));
        assertFalse(listed);

        uint32[] memory ids = bridge.listedTokenIds();
        assertEq(ids.length, 1);
        assertEq(ids[0], bridge.usdcTokenId());

        // The same token can be listed again afterwards, under any free id.
        bridge.listToken(address(dai), DAI_TOKEN_ID + 1, 18, 1e18, 10e18);
        (bool relisted, uint32 id) = bridge.tokenIdOf(address(dai));
        assertTrue(relisted);
        assertEq(id, DAI_TOKEN_ID + 1);
    }

    // -----------------------------------------------------------------
    // withdrawByProof
    // -----------------------------------------------------------------

    function test_withdrawByProof_paysTheTokenTheIdMapsTo() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        _depositDai(funder, 1e18);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 10 * UsdcTestLib.UNIT);

        uint256 daiBefore = dai.balanceOf(RECIPIENT);
        uint256 usdcBefore = usdc.balanceOf(RECIPIENT);

        bridge.withdrawByProof(
            _dummyProof(),
            _pubFor(DAI_TOKEN_ID, 1e18, Bn254FrLib.toFr(uint256(keccak256("payDai"))))
        );

        assertEq(dai.balanceOf(RECIPIENT) - daiBefore, 1e18, "paid in DAI");
        assertEq(usdc.balanceOf(RECIPIENT), usdcBefore, "USDC untouched");
        assertEq(bridge.liabilitiesOf(DAI_TOKEN_ID), 0);
        assertEq(bridge.treasuryBalance(), 10 * UsdcTestLib.UNIT, "USDC book untouched");
    }

    function test_withdrawByProof_unlistedIdReverts() public {
        vm.expectRevert(
            abi.encodeWithSelector(AckiNackiBridge.UnsupportedTokenId.selector, uint256(99))
        );
        bridge.withdrawByProof(
            _dummyProof(), _pubFor(99, 1, Bn254FrLib.toFr(uint256(keccak256("unlisted"))))
        );
    }

    /// @dev An AN ECC currency id is a uint32; a wider value was never one.
    function test_withdrawByProof_idWiderThanUint32Reverts() public {
        uint256 wide = uint256(type(uint32).max) + 1;
        IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory pub =
            _pubFor(0, 1, Bn254FrLib.toFr(uint256(keccak256("wide"))));
        pub.tokenId = wide;

        vm.expectRevert(abi.encodeWithSelector(AckiNackiBridge.UnsupportedTokenId.selector, wide));
        bridge.withdrawByProof(_dummyProof(), pub);
    }

    /// @dev One token's shortfall must be reported against its own book, not
    ///      against whatever USDC happens to be in the treasury.
    function test_withdrawByProof_shortfallIsPerToken() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        UsdcTestLib.depositUsdc(vm, usdc, bridge, funder, 10 * UsdcTestLib.UNIT);

        vm.expectRevert(
            abi.encodeWithSelector(
                AckiNackiBridge.WithdrawTreasuryShortfall.selector, 1e18, uint256(0)
            )
        );
        bridge.withdrawByProof(
            _dummyProof(),
            _pubFor(DAI_TOKEN_ID, 1e18, Bn254FrLib.toFr(uint256(keccak256("daiShortfall"))))
        );
    }

    function test_withdrawByProof_respectsPerTokenMaxTransfer() public {
        bridge.listToken(address(dai), DAI_TOKEN_ID, 18, 1e18, 10e18);
        bridge.setTokenDepositsEnabled(DAI_TOKEN_ID, true);
        _depositDai(funder, 1e18);

        vm.expectRevert(AckiNackiBridge.TransferTooLarge.selector);
        bridge.withdrawByProof(
            _dummyProof(),
            _pubFor(DAI_TOKEN_ID, 1e18 + 1, Bn254FrLib.toFr(uint256(keccak256("tooBig"))))
        );
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    function _depositDai(address user, uint256 amount) internal {
        dai.mint(user, amount);
        vm.startPrank(user);
        dai.approve(address(bridge), amount);
        bridge.deposit(address(dai), amount, bytes32(uint256(uint160(user))));
        vm.stopPrank();
    }

    function _pubFor(uint256 tokenId, uint256 amount, uint256 nullifier)
        internal
        view
        returns (IBridgeWithdrawalVerifier.WithdrawalPublicInputs memory)
    {
        uint256 a = uint256(uint160(RECIPIENT));
        return IBridgeWithdrawalVerifier.WithdrawalPublicInputs({
            tokenId: tokenId,
            amount: amount,
            recipientHi: a >> 80,
            recipientLo: a & RECIPIENT_HALF_MASK,
            dstChainId: block.chainid,
            senderAccFr: Bn254FrLib.toFr(uint256(keccak256("senderAcc"))),
            dappFr: DAPP_FR,
            accFr: ACC_FR,
            nullifier: nullifier,
            finalRoot: seedAnchor,
            anchorLayer: 1
        });
    }

    function _dummyProof() internal pure returns (bytes memory) {
        bytes memory p = new bytes(256);
        for (uint256 i = 0; i < 256; i++) {
            p[i] = bytes1(uint8(i));
        }
        return p;
    }

    function _seedFirstBlock() internal returns (uint256 l1Anchor) {
        uint256[10] memory layers;
        for (uint256 i = 0; i < ACTIVE_LAYERS; i++) {
            layers[i] = Bn254FrLib.toFr(uint256(keccak256(abi.encode("registry-seed-layer", i))));
        }
        l1Anchor = layers[0];
        bridge.verifyBlock(
            AckiNackiBridge.FinalizationType.Primary,
            abi.encodePacked(keccak256("registry-seed-att")),
            abi.encodePacked(keccak256("registry-seed-lh")),
            FIRST_BLOCK_ID,
            BK_SET,
            FIRST_SEQ_NO,
            ACTIVE_LAYERS,
            layers,
            bridge.expectedPrevAnchor(ACTIVE_LAYERS)
        );
    }
}
