// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

import "../mocks/MockAave.sol";

/// @notice A mock pool that deploys its own aToken, so the two addresses a
///         bridge takes follow from the USDC address alone.
contract MockAaveMarket is MockAavePool {
    constructor(address underlying_) MockAavePool(underlying_, address(new MockAUSDC())) { }
}

/// @title MockAaveLib
/// @notice One `MockAaveMarket` per USDC address and caller, at a CREATE2
///         address. `pool(usdc)` deploys it on first use; `aToken(usdc)` only
///         computes, so `new AckiNackiBridge(.., usdc, pool(usdc),
///         aToken(usdc), ..)` makes no call once the market exists — a
///         preceding `vm.expectRevert` still applies to the constructor.
library MockAaveLib {
    function pool(address usdc) internal returns (address market) {
        market = poolAddress(usdc);
        if (market.code.length == 0) {
            new MockAaveMarket{ salt: _salt(usdc) }(usdc);
        }
    }

    function aToken(address usdc) internal view returns (address) {
        // The market's first CREATE, at nonce 1.
        address market = poolAddress(usdc);
        return address(
            uint160(
                uint256(
                    keccak256(abi.encodePacked(bytes1(0xd6), bytes1(0x94), market, bytes1(0x01)))
                )
            )
        );
    }

    function poolAddress(address usdc) internal view returns (address) {
        bytes32 initCode =
            keccak256(abi.encodePacked(type(MockAaveMarket).creationCode, abi.encode(usdc)));
        return address(
            uint160(
                uint256(
                    keccak256(abi.encodePacked(bytes1(0xff), address(this), _salt(usdc), initCode))
                )
            )
        );
    }

    function _salt(address usdc) private pure returns (bytes32) {
        return bytes32(uint256(uint160(usdc)));
    }
}
