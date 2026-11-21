// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./NacklToken.sol";

library NacklMint {
    struct State {
        NacklToken token;
        bool handedOver;
        uint256 minted;
    }

    event NacklListed(address indexed token);
    event NacklMinterHandedOver(address indexed successor);

    error NacklNotMinter(address token);
    error NacklBadDecimals(uint8 decimals);
    error NacklMinterGone();
    error NacklSuccessorNotReady(address successor);

    function list(State storage s, NacklToken token, address predecessor) public {
        address minter = token.owner();
        if (minter != address(this) && (minter != predecessor || predecessor == address(0))) {
            revert NacklNotMinter(address(token));
        }
        uint8 d = token.decimals();
        if (d != 9) revert NacklBadDecimals(d);
        s.token = token;
        emit NacklListed(address(token));
    }

    function pay(State storage s, address recipient, uint256 amount) public {
        if (s.handedOver) revert NacklMinterGone();
        if (s.token.owner() != address(this)) revert NacklNotMinter(address(s.token));
        s.minted += amount;
        s.token.mint(recipient, amount);
    }

    function handOver(State storage s, address successor) public {
        if (s.handedOver) revert NacklMinterGone();
        (bool ok, bytes memory ret) =
            successor.staticcall(abi.encodeWithSignature("tokenInfo(uint32)", uint32(1)));
        if (!ok || ret.length < 32 || abi.decode(ret, (address)) != address(s.token)) {
            revert NacklSuccessorNotReady(successor);
        }
        s.handedOver = true;
        s.token.transferOwnership(successor);
        emit NacklMinterHandedOver(successor);
    }
}
