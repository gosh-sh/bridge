// SPDX-License-Identifier: MIT

pragma solidity 0.8.19;

import {Blake2bChallengeComputer} from "./Blake2bChallengeComputer.sol";

/// @title Blake2bHalo2Verifier
/// @notice Halo2 proof verifier using Blake2b transcript (EIP-152).
/// @dev Uses a separate Blake2bChallengeComputer contract for Fiat-Shamir
///      challenge computation to avoid stack-too-deep when mixing Solidity
///      library code with large assembly blocks.
contract Blake2bHalo2Verifier {
    address public immutable challengeComputer;

    constructor(address _challengeComputer) {
        challengeComputer = _challengeComputer;
    }

    fallback(bytes calldata) external returns (bytes memory) {
        // Call challenge computer to get Blake2b transcript challenges
        address cc = challengeComputer;
        assembly ("memory-safe") {
            // Copy calldata to memory at 0x120100 (temp area)
            let cdSize := calldatasize()
            calldatacopy(0x120100, 0, cdSize)
            // staticcall challenge computer
            let ok := staticcall(gas(), cc, 0x120100, cdSize, 0x120000, 0x100)
            if iszero(ok) { revert(0, 0) }
            // Now 0x120000..0x120100 contains abi.encode(theta, beta, gamma, y, x, v, u, final)
            // Each is 32 bytes at offsets 0x120000, 0x120020, 0x120040, 0x120060, 0x120080, 0x1200a0, 0x1200c0, 0x1200e0

            let success := true
            let f_p := 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
            let f_q := 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001
            function validate_ec_point(x, y) -> valid {
                {
                    let x_lt_p := lt(x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let y_lt_p := lt(y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    valid := and(x_lt_p, y_lt_p)
                }
                {
                    let y_square := mulmod(y, y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_square := mulmod(x, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_cube :=
                        mulmod(x_square, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_cube_plus_3 :=
                        addmod(x_cube, 3, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let is_affine := eq(x_cube_plus_3, y_square)
                    valid := and(valid, is_affine)
                }
            }
            mstore(0x1000a0, mod(calldataload(0x0), f_q))
            mstore(0x100080, 66080863397757689057462260550644432681803854999226369649442382355899108282)

            {
                let x := calldataload(0x20)
                mstore(0x1000c0, x)
                let y := calldataload(0x40)
                mstore(0x1000e0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x60)
                mstore(0x100100, x)
                let y := calldataload(0x80)
                mstore(0x100120, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0xa0)
                mstore(0x100140, x)
                let y := calldataload(0xc0)
                mstore(0x100160, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x100180, mload(0x120000))
            {
                let hash := mload(0x100180)
                mstore(0x1001a0, mod(hash, f_q))
                mstore(0x1001c0, hash)
            }

            {
                let x := calldataload(0xe0)
                mstore(0x1001e0, x)
                let y := calldataload(0x100)
                mstore(0x100200, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x120)
                mstore(0x100220, x)
                let y := calldataload(0x140)
                mstore(0x100240, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x100260, mload(0x120020))
            {
                let hash := mload(0x100260)
                mstore(0x100280, mod(hash, f_q))
                mstore(0x1002a0, hash)
            }
            mstore(0x1002c0, mload(0x120040))
            {
                let hash := mload(0x1002c0)
                mstore(0x1002e0, mod(hash, f_q))
                mstore(0x100300, hash)
            }

            {
                let x := calldataload(0x160)
                mstore(0x100320, x)
                let y := calldataload(0x180)
                mstore(0x100340, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x1a0)
                mstore(0x100360, x)
                let y := calldataload(0x1c0)
                mstore(0x100380, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x1e0)
                mstore(0x1003a0, x)
                let y := calldataload(0x200)
                mstore(0x1003c0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x220)
                mstore(0x1003e0, x)
                let y := calldataload(0x240)
                mstore(0x100400, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x260)
                mstore(0x100420, x)
                let y := calldataload(0x280)
                mstore(0x100440, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x100460, mload(0x120060))
            {
                let hash := mload(0x100460)
                mstore(0x100480, mod(hash, f_q))
                mstore(0x1004a0, hash)
            }

            {
                let x := calldataload(0x2a0)
                mstore(0x1004c0, x)
                let y := calldataload(0x2c0)
                mstore(0x1004e0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x2e0)
                mstore(0x100500, x)
                let y := calldataload(0x300)
                mstore(0x100520, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x320)
                mstore(0x100540, x)
                let y := calldataload(0x340)
                mstore(0x100560, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x100580, mload(0x120080))
            {
                let hash := mload(0x100580)
                mstore(0x1005a0, mod(hash, f_q))
                mstore(0x1005c0, hash)
            }
            mstore(0x1005e0, mod(calldataload(0x360), f_q))
            mstore(0x100600, mod(calldataload(0x380), f_q))
            mstore(0x100620, mod(calldataload(0x3a0), f_q))
            mstore(0x100640, mod(calldataload(0x3c0), f_q))
            mstore(0x100660, mod(calldataload(0x3e0), f_q))
            mstore(0x100680, mod(calldataload(0x400), f_q))
            mstore(0x1006a0, mod(calldataload(0x420), f_q))
            mstore(0x1006c0, mod(calldataload(0x440), f_q))
            mstore(0x1006e0, mod(calldataload(0x460), f_q))
            mstore(0x100700, mod(calldataload(0x480), f_q))
            mstore(0x100720, mod(calldataload(0x4a0), f_q))
            mstore(0x100740, mod(calldataload(0x4c0), f_q))
            mstore(0x100760, mod(calldataload(0x4e0), f_q))
            mstore(0x100780, mod(calldataload(0x500), f_q))
            mstore(0x1007a0, mod(calldataload(0x520), f_q))
            mstore(0x1007c0, mod(calldataload(0x540), f_q))
            mstore(0x1007e0, mod(calldataload(0x560), f_q))
            mstore(0x100800, mod(calldataload(0x580), f_q))
            mstore(0x100820, mod(calldataload(0x5a0), f_q))
            mstore(0x100840, mod(calldataload(0x5c0), f_q))
            mstore(0x100860, mod(calldataload(0x5e0), f_q))
            mstore(0x100880, mod(calldataload(0x600), f_q))
            mstore(0x1008a0, mod(calldataload(0x620), f_q))
            mstore(0x1008c0, mod(calldataload(0x640), f_q))
            mstore(0x1008e0, mod(calldataload(0x660), f_q))
            mstore(0x100900, mod(calldataload(0x680), f_q))
            mstore(0x100920, mod(calldataload(0x6a0), f_q))
            mstore(0x100940, mod(calldataload(0x6c0), f_q))
            mstore(0x100960, mod(calldataload(0x6e0), f_q))
            mstore(0x100980, mod(calldataload(0x700), f_q))
            mstore(0x1009a0, mod(calldataload(0x720), f_q))
            mstore(0x1009c0, mod(calldataload(0x740), f_q))
            mstore(0x1009e0, mload(0x1200a0))
            {
                let hash := mload(0x1009e0)
                mstore(0x100a00, mod(hash, f_q))
                mstore(0x100a20, hash)
            }
            mstore(0x100a40, mload(0x1200c0))
            {
                let hash := mload(0x100a40)
                mstore(0x100a60, mod(hash, f_q))
                mstore(0x100a80, hash)
            }

            {
                let x := calldataload(0x760)
                mstore(0x100aa0, x)
                let y := calldataload(0x780)
                mstore(0x100ac0, y)
                success := and(validate_ec_point(x, y), success)
            }
            // final SHPLONK challenge (8th challenge from staticcall output)
            mstore(0x100ae0, mload(0x1200e0))
            {
                let hash := mload(0x100ae0)
                mstore(0x100b00, mod(hash, f_q))
                mstore(0x100b20, hash)
            }

            {
                let x := calldataload(0x7a0)
                mstore(0x100b40, x)
                let y := calldataload(0x7c0)
                mstore(0x100b60, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x100b80, mulmod(mload(0x1005a0), mload(0x1005a0), f_q))
            mstore(0x100ba0, mulmod(mload(0x100b80), mload(0x100b80), f_q))
            mstore(0x100bc0, mulmod(mload(0x100ba0), mload(0x100ba0), f_q))
            mstore(0x100be0, mulmod(mload(0x100bc0), mload(0x100bc0), f_q))
            mstore(0x100c00, mulmod(mload(0x100be0), mload(0x100be0), f_q))
            mstore(0x100c20, mulmod(mload(0x100c00), mload(0x100c00), f_q))
            mstore(0x100c40, mulmod(mload(0x100c20), mload(0x100c20), f_q))
            mstore(0x100c60, mulmod(mload(0x100c40), mload(0x100c40), f_q))
            mstore(0x100c80, mulmod(mload(0x100c60), mload(0x100c60), f_q))
            mstore(0x100ca0, mulmod(mload(0x100c80), mload(0x100c80), f_q))
            mstore(0x100cc0, mulmod(mload(0x100ca0), mload(0x100ca0), f_q))
            mstore(0x100ce0, mulmod(mload(0x100cc0), mload(0x100cc0), f_q))
            mstore(
                0x100d00,
                addmod(
                    mload(0x100ce0),
                    21888242871839275222246405745257275088548364400416034343698204186575808495616,
                    f_q
                )
            )
            mstore(
                0x100d20,
                mulmod(
                    mload(0x100d00),
                    21882899062544392586694099493854624386622449272388589022813512242194320261121,
                    f_q
                )
            )
            mstore(
                0x100d40,
                mulmod(
                    mload(0x100d20),
                    13494463686150534520302493401520574237656912536907614359831638685825304135949,
                    f_q
                )
            )
            mstore(
                0x100d60,
                addmod(
                    mload(0x1005a0),
                    8393779185688740701943912343736700850891451863508419983866565500750504359668,
                    f_q
                )
            )
            mstore(
                0x100d80,
                mulmod(
                    mload(0x100d20),
                    18302882236472339419631414285403968768409802182737928837767912484847322191909,
                    f_q
                )
            )
            mstore(
                0x100da0,
                addmod(
                    mload(0x1005a0),
                    3585360635366935802614991459853306320138562217678105505930291701728486303708,
                    f_q
                )
            )
            mstore(
                0x100dc0,
                mulmod(
                    mload(0x100d20),
                    11537035432936037313763253554381703723723793827542696094189990411849785474670,
                    f_q
                )
            )
            mstore(
                0x100de0,
                addmod(
                    mload(0x1005a0),
                    10351207438903237908483152190875571364824570572873338249508213774726023020947,
                    f_q
                )
            )
            mstore(
                0x100e00,
                mulmod(
                    mload(0x100d20),
                    4925592601992654644734291590386747644864797672605745962807370354577123815907,
                    f_q
                )
            )
            mstore(
                0x100e20,
                addmod(
                    mload(0x1005a0),
                    16962650269846620577512114154870527443683566727810288380890833831998684679710,
                    f_q
                )
            )
            mstore(
                0x100e40,
                mulmod(
                    mload(0x100d20),
                    14428378809216400477736413013847344056809954101862299032583274736599544656045,
                    f_q
                )
            )
            mstore(
                0x100e60,
                addmod(
                    mload(0x1005a0),
                    7459864062622874744509992731409931031738410298553735311114929449976263839572,
                    f_q
                )
            )
            mstore(
                0x100e80,
                mulmod(
                    mload(0x100d20),
                    19444693496467964793333684482470811869395409953158764080291550423779334624794,
                    f_q
                )
            )
            mstore(
                0x100ea0,
                addmod(
                    mload(0x1005a0),
                    2443549375371310428912721262786463219152954447257270263406653762796473870823,
                    f_q
                )
            )
            mstore(
                0x100ec0,
                mulmod(
                    mload(0x100d20),
                    10679069158860809785885364198325818746230765378937472123583344754591056515264,
                    f_q
                )
            )
            mstore(
                0x100ee0,
                addmod(
                    mload(0x1005a0),
                    11209173712978465436361041546931456342317599021478562220114859431984751980353,
                    f_q
                )
            )
            mstore(0x100f00, mulmod(mload(0x100d20), 1, f_q))
            mstore(
                0x100f20,
                addmod(
                    mload(0x1005a0),
                    21888242871839275222246405745257275088548364400416034343698204186575808495616,
                    f_q
                )
            )
            {
                let prod := mload(0x100d60)

                prod := mulmod(mload(0x100da0), prod, f_q)
                mstore(0x100f40, prod)

                prod := mulmod(mload(0x100de0), prod, f_q)
                mstore(0x100f60, prod)

                prod := mulmod(mload(0x100e20), prod, f_q)
                mstore(0x100f80, prod)

                prod := mulmod(mload(0x100e60), prod, f_q)
                mstore(0x100fa0, prod)

                prod := mulmod(mload(0x100ea0), prod, f_q)
                mstore(0x100fc0, prod)

                prod := mulmod(mload(0x100ee0), prod, f_q)
                mstore(0x100fe0, prod)

                prod := mulmod(mload(0x100f20), prod, f_q)
                mstore(0x101000, prod)

                prod := mulmod(mload(0x100d00), prod, f_q)
                mstore(0x101020, prod)
            }
            mstore(0x101060, 32)
            mstore(0x101080, 32)
            mstore(0x1010a0, 32)
            mstore(0x1010c0, mload(0x101020))
            mstore(0x1010e0, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
            mstore(0x101100, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
            success := and(eq(staticcall(gas(), 0x5, 0x101060, 0xc0, 0x101040, 0x20), 1), success)
            {
                let inv := mload(0x101040)
                let v

                v := mload(0x100d00)
                mstore(0x100d00, mulmod(mload(0x101000), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x100f20)
                mstore(0x100f20, mulmod(mload(0x100fe0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x100ee0)
                mstore(0x100ee0, mulmod(mload(0x100fc0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x100ea0)
                mstore(0x100ea0, mulmod(mload(0x100fa0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x100e60)
                mstore(0x100e60, mulmod(mload(0x100f80), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x100e20)
                mstore(0x100e20, mulmod(mload(0x100f60), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x100de0)
                mstore(0x100de0, mulmod(mload(0x100f40), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x100da0)
                mstore(0x100da0, mulmod(mload(0x100d60), inv, f_q))
                inv := mulmod(v, inv, f_q)
                mstore(0x100d60, inv)
            }
            mstore(0x101120, mulmod(mload(0x100d40), mload(0x100d60), f_q))
            mstore(0x101140, mulmod(mload(0x100d80), mload(0x100da0), f_q))
            mstore(0x101160, mulmod(mload(0x100dc0), mload(0x100de0), f_q))
            mstore(0x101180, mulmod(mload(0x100e00), mload(0x100e20), f_q))
            mstore(0x1011a0, mulmod(mload(0x100e40), mload(0x100e60), f_q))
            mstore(0x1011c0, mulmod(mload(0x100e80), mload(0x100ea0), f_q))
            mstore(0x1011e0, mulmod(mload(0x100ec0), mload(0x100ee0), f_q))
            mstore(0x101200, mulmod(mload(0x100f00), mload(0x100f20), f_q))
            {
                let result := mulmod(mload(0x101200), mload(0x1000a0), f_q)
                mstore(0x101220, result)
            }
            mstore(0x101240, mulmod(mload(0x100620), mload(0x100600), f_q))
            mstore(0x101260, addmod(mload(0x1005e0), mload(0x101240), f_q))
            mstore(0x101280, addmod(mload(0x101260), sub(f_q, mload(0x100640)), f_q))
            mstore(0x1012a0, mulmod(mload(0x101280), mload(0x100740), f_q))
            mstore(0x1012c0, mulmod(mload(0x100480), mload(0x1012a0), f_q))
            mstore(0x1012e0, mulmod(mload(0x1006a0), mload(0x100680), f_q))
            mstore(0x101300, addmod(mload(0x100660), mload(0x1012e0), f_q))
            mstore(0x101320, addmod(mload(0x101300), sub(f_q, mload(0x1006c0)), f_q))
            mstore(0x101340, mulmod(mload(0x101320), mload(0x100760), f_q))
            mstore(0x101360, addmod(mload(0x1012c0), mload(0x101340), f_q))
            mstore(0x101380, mulmod(mload(0x100480), mload(0x101360), f_q))
            mstore(0x1013a0, addmod(1, sub(f_q, mload(0x100840)), f_q))
            mstore(0x1013c0, mulmod(mload(0x1013a0), mload(0x101200), f_q))
            mstore(0x1013e0, addmod(mload(0x101380), mload(0x1013c0), f_q))
            mstore(0x101400, mulmod(mload(0x100480), mload(0x1013e0), f_q))
            mstore(0x101420, mulmod(mload(0x100900), mload(0x100900), f_q))
            mstore(0x101440, addmod(mload(0x101420), sub(f_q, mload(0x100900)), f_q))
            mstore(0x101460, mulmod(mload(0x101440), mload(0x101120), f_q))
            mstore(0x101480, addmod(mload(0x101400), mload(0x101460), f_q))
            mstore(0x1014a0, mulmod(mload(0x100480), mload(0x101480), f_q))
            mstore(0x1014c0, addmod(mload(0x1008a0), sub(f_q, mload(0x100880)), f_q))
            mstore(0x1014e0, mulmod(mload(0x1014c0), mload(0x101200), f_q))
            mstore(0x101500, addmod(mload(0x1014a0), mload(0x1014e0), f_q))
            mstore(0x101520, mulmod(mload(0x100480), mload(0x101500), f_q))
            mstore(0x101540, addmod(mload(0x100900), sub(f_q, mload(0x1008e0)), f_q))
            mstore(0x101560, mulmod(mload(0x101540), mload(0x101200), f_q))
            mstore(0x101580, addmod(mload(0x101520), mload(0x101560), f_q))
            mstore(0x1015a0, mulmod(mload(0x100480), mload(0x101580), f_q))
            mstore(0x1015c0, addmod(1, sub(f_q, mload(0x101120)), f_q))
            mstore(0x1015e0, addmod(mload(0x101140), mload(0x101160), f_q))
            mstore(0x101600, addmod(mload(0x1015e0), mload(0x101180), f_q))
            mstore(0x101620, addmod(mload(0x101600), mload(0x1011a0), f_q))
            mstore(0x101640, addmod(mload(0x101620), mload(0x1011c0), f_q))
            mstore(0x101660, addmod(mload(0x101640), mload(0x1011e0), f_q))
            mstore(0x101680, addmod(mload(0x1015c0), sub(f_q, mload(0x101660)), f_q))
            mstore(0x1016a0, mulmod(mload(0x1007a0), mload(0x100280), f_q))
            mstore(0x1016c0, addmod(mload(0x100700), mload(0x1016a0), f_q))
            mstore(0x1016e0, addmod(mload(0x1016c0), mload(0x1002e0), f_q))
            mstore(0x101700, mulmod(mload(0x1007c0), mload(0x100280), f_q))
            mstore(0x101720, addmod(mload(0x1005e0), mload(0x101700), f_q))
            mstore(0x101740, addmod(mload(0x101720), mload(0x1002e0), f_q))
            mstore(0x101760, mulmod(mload(0x101740), mload(0x1016e0), f_q))
            mstore(0x101780, mulmod(mload(0x101760), mload(0x100860), f_q))
            mstore(0x1017a0, mulmod(1, mload(0x100280), f_q))
            mstore(0x1017c0, mulmod(mload(0x1005a0), mload(0x1017a0), f_q))
            mstore(0x1017e0, addmod(mload(0x100700), mload(0x1017c0), f_q))
            mstore(0x101800, addmod(mload(0x1017e0), mload(0x1002e0), f_q))
            mstore(
                0x101820,
                mulmod(
                    4131629893567559867359510883348571134090853742863529169391034518566172092834,
                    mload(0x100280),
                    f_q
                )
            )
            mstore(0x101840, mulmod(mload(0x1005a0), mload(0x101820), f_q))
            mstore(0x101860, addmod(mload(0x1005e0), mload(0x101840), f_q))
            mstore(0x101880, addmod(mload(0x101860), mload(0x1002e0), f_q))
            mstore(0x1018a0, mulmod(mload(0x101880), mload(0x101800), f_q))
            mstore(0x1018c0, mulmod(mload(0x1018a0), mload(0x100840), f_q))
            mstore(0x1018e0, addmod(mload(0x101780), sub(f_q, mload(0x1018c0)), f_q))
            mstore(0x101900, mulmod(mload(0x1018e0), mload(0x101680), f_q))
            mstore(0x101920, addmod(mload(0x1015a0), mload(0x101900), f_q))
            mstore(0x101940, mulmod(mload(0x100480), mload(0x101920), f_q))
            mstore(0x101960, mulmod(mload(0x1007e0), mload(0x100280), f_q))
            mstore(0x101980, addmod(mload(0x100660), mload(0x101960), f_q))
            mstore(0x1019a0, addmod(mload(0x101980), mload(0x1002e0), f_q))
            mstore(0x1019c0, mulmod(mload(0x100800), mload(0x100280), f_q))
            mstore(0x1019e0, addmod(mload(0x1006e0), mload(0x1019c0), f_q))
            mstore(0x101a00, addmod(mload(0x1019e0), mload(0x1002e0), f_q))
            mstore(0x101a20, mulmod(mload(0x101a00), mload(0x1019a0), f_q))
            mstore(0x101a40, mulmod(mload(0x101a20), mload(0x1008c0), f_q))
            mstore(
                0x101a60,
                mulmod(
                    8910878055287538404433155982483128285667088683464058436815641868457422632747,
                    mload(0x100280),
                    f_q
                )
            )
            mstore(0x101a80, mulmod(mload(0x1005a0), mload(0x101a60), f_q))
            mstore(0x101aa0, addmod(mload(0x100660), mload(0x101a80), f_q))
            mstore(0x101ac0, addmod(mload(0x101aa0), mload(0x1002e0), f_q))
            mstore(
                0x101ae0,
                mulmod(
                    11166246659983828508719468090013646171463329086121580628794302409516816350802,
                    mload(0x100280),
                    f_q
                )
            )
            mstore(0x101b00, mulmod(mload(0x1005a0), mload(0x101ae0), f_q))
            mstore(0x101b20, addmod(mload(0x1006e0), mload(0x101b00), f_q))
            mstore(0x101b40, addmod(mload(0x101b20), mload(0x1002e0), f_q))
            mstore(0x101b60, mulmod(mload(0x101b40), mload(0x101ac0), f_q))
            mstore(0x101b80, mulmod(mload(0x101b60), mload(0x1008a0), f_q))
            mstore(0x101ba0, addmod(mload(0x101a40), sub(f_q, mload(0x101b80)), f_q))
            mstore(0x101bc0, mulmod(mload(0x101ba0), mload(0x101680), f_q))
            mstore(0x101be0, addmod(mload(0x101940), mload(0x101bc0), f_q))
            mstore(0x101c00, mulmod(mload(0x100480), mload(0x101be0), f_q))
            mstore(0x101c20, mulmod(mload(0x100820), mload(0x100280), f_q))
            mstore(0x101c40, addmod(mload(0x101220), mload(0x101c20), f_q))
            mstore(0x101c60, addmod(mload(0x101c40), mload(0x1002e0), f_q))
            mstore(0x101c80, mulmod(mload(0x101c60), mload(0x100920), f_q))
            mstore(
                0x101ca0,
                mulmod(
                    284840088355319032285349970403338060113257071685626700086398481893096618818,
                    mload(0x100280),
                    f_q
                )
            )
            mstore(0x101cc0, mulmod(mload(0x1005a0), mload(0x101ca0), f_q))
            mstore(0x101ce0, addmod(mload(0x101220), mload(0x101cc0), f_q))
            mstore(0x101d00, addmod(mload(0x101ce0), mload(0x1002e0), f_q))
            mstore(0x101d20, mulmod(mload(0x101d00), mload(0x100900), f_q))
            mstore(0x101d40, addmod(mload(0x101c80), sub(f_q, mload(0x101d20)), f_q))
            mstore(0x101d60, mulmod(mload(0x101d40), mload(0x101680), f_q))
            mstore(0x101d80, addmod(mload(0x101c00), mload(0x101d60), f_q))
            mstore(0x101da0, mulmod(mload(0x100480), mload(0x101d80), f_q))
            mstore(0x101dc0, addmod(1, sub(f_q, mload(0x100940)), f_q))
            mstore(0x101de0, mulmod(mload(0x101dc0), mload(0x101200), f_q))
            mstore(0x101e00, addmod(mload(0x101da0), mload(0x101de0), f_q))
            mstore(0x101e20, mulmod(mload(0x100480), mload(0x101e00), f_q))
            mstore(0x101e40, mulmod(mload(0x100940), mload(0x100940), f_q))
            mstore(0x101e60, addmod(mload(0x101e40), sub(f_q, mload(0x100940)), f_q))
            mstore(0x101e80, mulmod(mload(0x101e60), mload(0x101120), f_q))
            mstore(0x101ea0, addmod(mload(0x101e20), mload(0x101e80), f_q))
            mstore(0x101ec0, mulmod(mload(0x100480), mload(0x101ea0), f_q))
            mstore(0x101ee0, addmod(mload(0x100980), mload(0x100280), f_q))
            mstore(0x101f00, mulmod(mload(0x101ee0), mload(0x100960), f_q))
            mstore(0x101f20, addmod(mload(0x1009c0), mload(0x1002e0), f_q))
            mstore(0x101f40, mulmod(mload(0x101f20), mload(0x101f00), f_q))
            mstore(0x101f60, addmod(mload(0x1006e0), mload(0x100280), f_q))
            mstore(0x101f80, mulmod(mload(0x101f60), mload(0x100940), f_q))
            mstore(0x101fa0, addmod(mload(0x100720), mload(0x1002e0), f_q))
            mstore(0x101fc0, mulmod(mload(0x101fa0), mload(0x101f80), f_q))
            mstore(0x101fe0, addmod(mload(0x101f40), sub(f_q, mload(0x101fc0)), f_q))
            mstore(0x102000, mulmod(mload(0x101fe0), mload(0x101680), f_q))
            mstore(0x102020, addmod(mload(0x101ec0), mload(0x102000), f_q))
            mstore(0x102040, mulmod(mload(0x100480), mload(0x102020), f_q))
            mstore(0x102060, addmod(mload(0x100980), sub(f_q, mload(0x1009c0)), f_q))
            mstore(0x102080, mulmod(mload(0x102060), mload(0x101200), f_q))
            mstore(0x1020a0, addmod(mload(0x102040), mload(0x102080), f_q))
            mstore(0x1020c0, mulmod(mload(0x100480), mload(0x1020a0), f_q))
            mstore(0x1020e0, mulmod(mload(0x102060), mload(0x101680), f_q))
            mstore(0x102100, addmod(mload(0x100980), sub(f_q, mload(0x1009a0)), f_q))
            mstore(0x102120, mulmod(mload(0x102100), mload(0x1020e0), f_q))
            mstore(0x102140, addmod(mload(0x1020c0), mload(0x102120), f_q))
            mstore(0x102160, mulmod(mload(0x100ce0), mload(0x100ce0), f_q))
            mstore(0x102180, mulmod(mload(0x102160), mload(0x100ce0), f_q))
            mstore(0x1021a0, mulmod(1, mload(0x100ce0), f_q))
            mstore(0x1021c0, mulmod(1, mload(0x102160), f_q))
            mstore(0x1021e0, mulmod(mload(0x102140), mload(0x100d00), f_q))
            mstore(0x102200, mulmod(mload(0x100b80), mload(0x1005a0), f_q))
            mstore(0x102220, mulmod(mload(0x102200), mload(0x1005a0), f_q))
            mstore(
                0x102240,
                mulmod(
                    mload(0x1005a0),
                    13494463686150534520302493401520574237656912536907614359831638685825304135949,
                    f_q
                )
            )
            mstore(0x102260, addmod(mload(0x100b00), sub(f_q, mload(0x102240)), f_q))
            mstore(
                0x102280,
                mulmod(
                    mload(0x1005a0),
                    10679069158860809785885364198325818746230765378937472123583344754591056515264,
                    f_q
                )
            )
            mstore(0x1022a0, addmod(mload(0x100b00), sub(f_q, mload(0x102280)), f_q))
            mstore(0x1022c0, mulmod(mload(0x1005a0), 1, f_q))
            mstore(0x1022e0, addmod(mload(0x100b00), sub(f_q, mload(0x1022c0)), f_q))
            mstore(
                0x102300,
                mulmod(
                    mload(0x1005a0),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            mstore(0x102320, addmod(mload(0x100b00), sub(f_q, mload(0x102300)), f_q))
            mstore(
                0x102340,
                mulmod(
                    mload(0x1005a0),
                    9396103202274256930945606623206526900461945684265495839012435492634193195103,
                    f_q
                )
            )
            mstore(0x102360, addmod(mload(0x100b00), sub(f_q, mload(0x102340)), f_q))
            mstore(
                0x102380,
                mulmod(
                    mload(0x1005a0),
                    7141049368301343714688735307754106798758533264534387178564790962720151052558,
                    f_q
                )
            )
            mstore(0x1023a0, addmod(mload(0x100b00), sub(f_q, mload(0x102380)), f_q))
            mstore(
                0x1023c0,
                mulmod(
                    14539938970100077722787609456885990080676645171153683143676448052646487506229,
                    mload(0x102200),
                    f_q
                )
            )
            mstore(0x1023e0, mulmod(mload(0x1023c0), 1, f_q))
            {
                let result := mulmod(mload(0x100b00), mload(0x1023c0), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x1023e0)), f_q), result, f_q)
                mstore(0x102400, result)
            }
            mstore(
                0x102420,
                mulmod(
                    7256530809051719749867553753986957859231619840094570731739941182706686413665,
                    mload(0x102200),
                    f_q
                )
            )
            mstore(
                0x102440,
                mulmod(
                    mload(0x102420),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0x100b00), mload(0x102420), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x102440)), f_q), result, f_q)
                mstore(0x102460, result)
            }
            mstore(
                0x102480,
                mulmod(
                    12809867751727305620475423349830892726279648403976095368629033842110525582729,
                    mload(0x102200),
                    f_q
                )
            )
            mstore(
                0x1024a0,
                mulmod(
                    mload(0x102480),
                    9396103202274256930945606623206526900461945684265495839012435492634193195103,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0x100b00), mload(0x102480), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x1024a0)), f_q), result, f_q)
                mstore(0x1024c0, result)
            }
            mstore(
                0x1024e0,
                mulmod(
                    5787000168993132838335045762627010468577538624811266895797571188740539775353,
                    mload(0x102200),
                    f_q
                )
            )
            mstore(
                0x102500,
                mulmod(
                    mload(0x1024e0),
                    7141049368301343714688735307754106798758533264534387178564790962720151052558,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0x100b00), mload(0x1024e0), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x102500)), f_q), result, f_q)
                mstore(0x102520, result)
            }
            mstore(0x102540, mulmod(1, mload(0x1022e0), f_q))
            mstore(0x102560, mulmod(mload(0x102540), mload(0x102320), f_q))
            mstore(0x102580, mulmod(mload(0x102560), mload(0x102360), f_q))
            mstore(0x1025a0, mulmod(mload(0x102580), mload(0x1023a0), f_q))
            {
                let result := mulmod(mload(0x100b00), 1, f_q)
                result := addmod(
                    mulmod(
                        mload(0x1005a0),
                        21888242871839275222246405745257275088548364400416034343698204186575808495616,
                        f_q
                    ),
                    result,
                    f_q
                )
                mstore(0x1025c0, result)
            }
            mstore(
                0x1025e0,
                mulmod(
                    5266333647111022262519575308227530447403540681101773355208407176447894872116,
                    mload(0x100b80),
                    f_q
                )
            )
            mstore(0x102600, mulmod(mload(0x1025e0), 1, f_q))
            {
                let result := mulmod(mload(0x100b00), mload(0x1025e0), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x102600)), f_q), result, f_q)
                mstore(0x102620, result)
            }
            mstore(
                0x102640,
                mulmod(
                    5045599748741669394807340163667268286359707073706640238348295071038051955298,
                    mload(0x100b80),
                    f_q
                )
            )
            mstore(
                0x102660,
                mulmod(
                    mload(0x102640),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0x100b00), mload(0x102640), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x102660)), f_q), result, f_q)
                mstore(0x102680, result)
            }
            mstore(
                0x1026a0,
                mulmod(
                    9024646962603862267003278632616888159297309104221063589461183531809948543756,
                    mload(0x100b80),
                    f_q
                )
            )
            mstore(
                0x1026c0,
                mulmod(
                    mload(0x1026a0),
                    13494463686150534520302493401520574237656912536907614359831638685825304135949,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0x100b00), mload(0x1026a0), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x1026c0)), f_q), result, f_q)
                mstore(0x1026e0, result)
            }
            mstore(0x102700, mulmod(mload(0x102560), mload(0x102260), f_q))
            mstore(
                0x102720,
                mulmod(
                    457915096789217363190654424344135916650651035271458877272133377425876816156,
                    mload(0x1005a0),
                    f_q
                )
            )
            mstore(0x102740, mulmod(mload(0x102720), 1, f_q))
            {
                let result := mulmod(mload(0x100b00), mload(0x102720), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x102740)), f_q), result, f_q)
                mstore(0x102760, result)
            }
            mstore(
                0x102780,
                mulmod(
                    21430327775050057859055751320913139171897713365144575466426070809149931679461,
                    mload(0x1005a0),
                    f_q
                )
            )
            mstore(
                0x1027a0,
                mulmod(
                    mload(0x102780),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0x100b00), mload(0x102780), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x1027a0)), f_q), result, f_q)
                mstore(0x1027c0, result)
            }
            mstore(
                0x1027e0,
                mulmod(
                    11209173712978465436361041546931456342317599021478562220114859431984751980354,
                    mload(0x1005a0),
                    f_q
                )
            )
            mstore(0x102800, mulmod(mload(0x1027e0), 1, f_q))
            {
                let result := mulmod(mload(0x100b00), mload(0x1027e0), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x102800)), f_q), result, f_q)
                mstore(0x102820, result)
            }
            mstore(
                0x102840,
                mulmod(
                    10679069158860809785885364198325818746230765378937472123583344754591056515263,
                    mload(0x1005a0),
                    f_q
                )
            )
            mstore(
                0x102860,
                mulmod(
                    mload(0x102840),
                    10679069158860809785885364198325818746230765378937472123583344754591056515264,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0x100b00), mload(0x102840), f_q)
                result := addmod(mulmod(mload(0x1005a0), sub(f_q, mload(0x102860)), f_q), result, f_q)
                mstore(0x102880, result)
            }
            mstore(0x1028a0, mulmod(mload(0x102540), mload(0x1022a0), f_q))
            {
                let prod := mload(0x102400)

                prod := mulmod(mload(0x102460), prod, f_q)
                mstore(0x1028c0, prod)

                prod := mulmod(mload(0x1024c0), prod, f_q)
                mstore(0x1028e0, prod)

                prod := mulmod(mload(0x102520), prod, f_q)
                mstore(0x102900, prod)

                prod := mulmod(mload(0x1025c0), prod, f_q)
                mstore(0x102920, prod)

                prod := mulmod(mload(0x102540), prod, f_q)
                mstore(0x102940, prod)

                prod := mulmod(mload(0x102620), prod, f_q)
                mstore(0x102960, prod)

                prod := mulmod(mload(0x102680), prod, f_q)
                mstore(0x102980, prod)

                prod := mulmod(mload(0x1026e0), prod, f_q)
                mstore(0x1029a0, prod)

                prod := mulmod(mload(0x102700), prod, f_q)
                mstore(0x1029c0, prod)

                prod := mulmod(mload(0x102760), prod, f_q)
                mstore(0x1029e0, prod)

                prod := mulmod(mload(0x1027c0), prod, f_q)
                mstore(0x102a00, prod)

                prod := mulmod(mload(0x102560), prod, f_q)
                mstore(0x102a20, prod)

                prod := mulmod(mload(0x102820), prod, f_q)
                mstore(0x102a40, prod)

                prod := mulmod(mload(0x102880), prod, f_q)
                mstore(0x102a60, prod)

                prod := mulmod(mload(0x1028a0), prod, f_q)
                mstore(0x102a80, prod)
            }
            mstore(0x102ac0, 32)
            mstore(0x102ae0, 32)
            mstore(0x102b00, 32)
            mstore(0x102b20, mload(0x102a80))
            mstore(0x102b40, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
            mstore(0x102b60, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
            success := and(eq(staticcall(gas(), 0x5, 0x102ac0, 0xc0, 0x102aa0, 0x20), 1), success)
            {
                let inv := mload(0x102aa0)
                let v

                v := mload(0x1028a0)
                mstore(0x1028a0, mulmod(mload(0x102a60), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102880)
                mstore(0x102880, mulmod(mload(0x102a40), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102820)
                mstore(0x102820, mulmod(mload(0x102a20), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102560)
                mstore(0x102560, mulmod(mload(0x102a00), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1027c0)
                mstore(0x1027c0, mulmod(mload(0x1029e0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102760)
                mstore(0x102760, mulmod(mload(0x1029c0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102700)
                mstore(0x102700, mulmod(mload(0x1029a0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1026e0)
                mstore(0x1026e0, mulmod(mload(0x102980), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102680)
                mstore(0x102680, mulmod(mload(0x102960), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102620)
                mstore(0x102620, mulmod(mload(0x102940), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102540)
                mstore(0x102540, mulmod(mload(0x102920), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1025c0)
                mstore(0x1025c0, mulmod(mload(0x102900), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102520)
                mstore(0x102520, mulmod(mload(0x1028e0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1024c0)
                mstore(0x1024c0, mulmod(mload(0x1028c0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102460)
                mstore(0x102460, mulmod(mload(0x102400), inv, f_q))
                inv := mulmod(v, inv, f_q)
                mstore(0x102400, inv)
            }
            {
                let result := mload(0x102400)
                result := addmod(mload(0x102460), result, f_q)
                result := addmod(mload(0x1024c0), result, f_q)
                result := addmod(mload(0x102520), result, f_q)
                mstore(0x102b80, result)
            }
            mstore(0x102ba0, mulmod(mload(0x1025a0), mload(0x102540), f_q))
            {
                let result := mload(0x1025c0)
                mstore(0x102bc0, result)
            }
            mstore(0x102be0, mulmod(mload(0x1025a0), mload(0x102700), f_q))
            {
                let result := mload(0x102620)
                result := addmod(mload(0x102680), result, f_q)
                result := addmod(mload(0x1026e0), result, f_q)
                mstore(0x102c00, result)
            }
            mstore(0x102c20, mulmod(mload(0x1025a0), mload(0x102560), f_q))
            {
                let result := mload(0x102760)
                result := addmod(mload(0x1027c0), result, f_q)
                mstore(0x102c40, result)
            }
            mstore(0x102c60, mulmod(mload(0x1025a0), mload(0x1028a0), f_q))
            {
                let result := mload(0x102820)
                result := addmod(mload(0x102880), result, f_q)
                mstore(0x102c80, result)
            }
            {
                let prod := mload(0x102b80)

                prod := mulmod(mload(0x102bc0), prod, f_q)
                mstore(0x102ca0, prod)

                prod := mulmod(mload(0x102c00), prod, f_q)
                mstore(0x102cc0, prod)

                prod := mulmod(mload(0x102c40), prod, f_q)
                mstore(0x102ce0, prod)

                prod := mulmod(mload(0x102c80), prod, f_q)
                mstore(0x102d00, prod)
            }
            mstore(0x102d40, 32)
            mstore(0x102d60, 32)
            mstore(0x102d80, 32)
            mstore(0x102da0, mload(0x102d00))
            mstore(0x102dc0, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
            mstore(0x102de0, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
            success := and(eq(staticcall(gas(), 0x5, 0x102d40, 0xc0, 0x102d20, 0x20), 1), success)
            {
                let inv := mload(0x102d20)
                let v

                v := mload(0x102c80)
                mstore(0x102c80, mulmod(mload(0x102ce0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102c40)
                mstore(0x102c40, mulmod(mload(0x102cc0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102c00)
                mstore(0x102c00, mulmod(mload(0x102ca0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x102bc0)
                mstore(0x102bc0, mulmod(mload(0x102b80), inv, f_q))
                inv := mulmod(v, inv, f_q)
                mstore(0x102b80, inv)
            }
            mstore(0x102e00, mulmod(mload(0x102ba0), mload(0x102bc0), f_q))
            mstore(0x102e20, mulmod(mload(0x102be0), mload(0x102c00), f_q))
            mstore(0x102e40, mulmod(mload(0x102c20), mload(0x102c40), f_q))
            mstore(0x102e60, mulmod(mload(0x102c60), mload(0x102c80), f_q))
            mstore(0x102e80, mulmod(mload(0x100a00), mload(0x100a00), f_q))
            mstore(0x102ea0, mulmod(mload(0x102e80), mload(0x100a00), f_q))
            mstore(0x102ec0, mulmod(mload(0x102ea0), mload(0x100a00), f_q))
            mstore(0x102ee0, mulmod(mload(0x102ec0), mload(0x100a00), f_q))
            mstore(0x102f00, mulmod(mload(0x102ee0), mload(0x100a00), f_q))
            mstore(0x102f20, mulmod(mload(0x102f00), mload(0x100a00), f_q))
            mstore(0x102f40, mulmod(mload(0x102f20), mload(0x100a00), f_q))
            mstore(0x102f60, mulmod(mload(0x102f40), mload(0x100a00), f_q))
            mstore(0x102f80, mulmod(mload(0x102f60), mload(0x100a00), f_q))
            mstore(0x102fa0, mulmod(mload(0x102f80), mload(0x100a00), f_q))
            mstore(0x102fc0, mulmod(mload(0x102fa0), mload(0x100a00), f_q))
            mstore(0x102fe0, mulmod(mload(0x102fc0), mload(0x100a00), f_q))
            mstore(0x103000, mulmod(mload(0x100a60), mload(0x100a60), f_q))
            mstore(0x103020, mulmod(mload(0x103000), mload(0x100a60), f_q))
            mstore(0x103040, mulmod(mload(0x103020), mload(0x100a60), f_q))
            mstore(0x103060, mulmod(mload(0x103040), mload(0x100a60), f_q))
            {
                let result := mulmod(mload(0x1005e0), mload(0x102400), f_q)
                result := addmod(mulmod(mload(0x100600), mload(0x102460), f_q), result, f_q)
                result := addmod(mulmod(mload(0x100620), mload(0x1024c0), f_q), result, f_q)
                result := addmod(mulmod(mload(0x100640), mload(0x102520), f_q), result, f_q)
                mstore(0x103080, result)
            }
            mstore(0x1030a0, mulmod(mload(0x103080), mload(0x102b80), f_q))
            mstore(0x1030c0, mulmod(sub(f_q, mload(0x1030a0)), 1, f_q))
            {
                let result := mulmod(mload(0x100660), mload(0x102400), f_q)
                result := addmod(mulmod(mload(0x100680), mload(0x102460), f_q), result, f_q)
                result := addmod(mulmod(mload(0x1006a0), mload(0x1024c0), f_q), result, f_q)
                result := addmod(mulmod(mload(0x1006c0), mload(0x102520), f_q), result, f_q)
                mstore(0x1030e0, result)
            }
            mstore(0x103100, mulmod(mload(0x1030e0), mload(0x102b80), f_q))
            mstore(0x103120, mulmod(sub(f_q, mload(0x103100)), mload(0x100a00), f_q))
            mstore(0x103140, mulmod(1, mload(0x100a00), f_q))
            mstore(0x103160, addmod(mload(0x1030c0), mload(0x103120), f_q))
            mstore(0x103180, mulmod(mload(0x103160), 1, f_q))
            mstore(0x1031a0, mulmod(mload(0x103140), 1, f_q))
            mstore(0x1031c0, mulmod(1, mload(0x102ba0), f_q))
            {
                let result := mulmod(mload(0x1006e0), mload(0x1025c0), f_q)
                mstore(0x1031e0, result)
            }
            mstore(0x103200, mulmod(mload(0x1031e0), mload(0x102e00), f_q))
            mstore(0x103220, mulmod(sub(f_q, mload(0x103200)), 1, f_q))
            mstore(0x103240, mulmod(mload(0x1031c0), 1, f_q))
            {
                let result := mulmod(mload(0x1009c0), mload(0x1025c0), f_q)
                mstore(0x103260, result)
            }
            mstore(0x103280, mulmod(mload(0x103260), mload(0x102e00), f_q))
            mstore(0x1032a0, mulmod(sub(f_q, mload(0x103280)), mload(0x100a00), f_q))
            mstore(0x1032c0, mulmod(mload(0x1031c0), mload(0x100a00), f_q))
            mstore(0x1032e0, addmod(mload(0x103220), mload(0x1032a0), f_q))
            {
                let result := mulmod(mload(0x100700), mload(0x1025c0), f_q)
                mstore(0x103300, result)
            }
            mstore(0x103320, mulmod(mload(0x103300), mload(0x102e00), f_q))
            mstore(0x103340, mulmod(sub(f_q, mload(0x103320)), mload(0x102e80), f_q))
            mstore(0x103360, mulmod(mload(0x1031c0), mload(0x102e80), f_q))
            mstore(0x103380, addmod(mload(0x1032e0), mload(0x103340), f_q))
            {
                let result := mulmod(mload(0x100720), mload(0x1025c0), f_q)
                mstore(0x1033a0, result)
            }
            mstore(0x1033c0, mulmod(mload(0x1033a0), mload(0x102e00), f_q))
            mstore(0x1033e0, mulmod(sub(f_q, mload(0x1033c0)), mload(0x102ea0), f_q))
            mstore(0x103400, mulmod(mload(0x1031c0), mload(0x102ea0), f_q))
            mstore(0x103420, addmod(mload(0x103380), mload(0x1033e0), f_q))
            {
                let result := mulmod(mload(0x100740), mload(0x1025c0), f_q)
                mstore(0x103440, result)
            }
            mstore(0x103460, mulmod(mload(0x103440), mload(0x102e00), f_q))
            mstore(0x103480, mulmod(sub(f_q, mload(0x103460)), mload(0x102ec0), f_q))
            mstore(0x1034a0, mulmod(mload(0x1031c0), mload(0x102ec0), f_q))
            mstore(0x1034c0, addmod(mload(0x103420), mload(0x103480), f_q))
            {
                let result := mulmod(mload(0x100760), mload(0x1025c0), f_q)
                mstore(0x1034e0, result)
            }
            mstore(0x103500, mulmod(mload(0x1034e0), mload(0x102e00), f_q))
            mstore(0x103520, mulmod(sub(f_q, mload(0x103500)), mload(0x102ee0), f_q))
            mstore(0x103540, mulmod(mload(0x1031c0), mload(0x102ee0), f_q))
            mstore(0x103560, addmod(mload(0x1034c0), mload(0x103520), f_q))
            {
                let result := mulmod(mload(0x1007a0), mload(0x1025c0), f_q)
                mstore(0x103580, result)
            }
            mstore(0x1035a0, mulmod(mload(0x103580), mload(0x102e00), f_q))
            mstore(0x1035c0, mulmod(sub(f_q, mload(0x1035a0)), mload(0x102f00), f_q))
            mstore(0x1035e0, mulmod(mload(0x1031c0), mload(0x102f00), f_q))
            mstore(0x103600, addmod(mload(0x103560), mload(0x1035c0), f_q))
            {
                let result := mulmod(mload(0x1007c0), mload(0x1025c0), f_q)
                mstore(0x103620, result)
            }
            mstore(0x103640, mulmod(mload(0x103620), mload(0x102e00), f_q))
            mstore(0x103660, mulmod(sub(f_q, mload(0x103640)), mload(0x102f20), f_q))
            mstore(0x103680, mulmod(mload(0x1031c0), mload(0x102f20), f_q))
            mstore(0x1036a0, addmod(mload(0x103600), mload(0x103660), f_q))
            {
                let result := mulmod(mload(0x1007e0), mload(0x1025c0), f_q)
                mstore(0x1036c0, result)
            }
            mstore(0x1036e0, mulmod(mload(0x1036c0), mload(0x102e00), f_q))
            mstore(0x103700, mulmod(sub(f_q, mload(0x1036e0)), mload(0x102f40), f_q))
            mstore(0x103720, mulmod(mload(0x1031c0), mload(0x102f40), f_q))
            mstore(0x103740, addmod(mload(0x1036a0), mload(0x103700), f_q))
            {
                let result := mulmod(mload(0x100800), mload(0x1025c0), f_q)
                mstore(0x103760, result)
            }
            mstore(0x103780, mulmod(mload(0x103760), mload(0x102e00), f_q))
            mstore(0x1037a0, mulmod(sub(f_q, mload(0x103780)), mload(0x102f60), f_q))
            mstore(0x1037c0, mulmod(mload(0x1031c0), mload(0x102f60), f_q))
            mstore(0x1037e0, addmod(mload(0x103740), mload(0x1037a0), f_q))
            {
                let result := mulmod(mload(0x100820), mload(0x1025c0), f_q)
                mstore(0x103800, result)
            }
            mstore(0x103820, mulmod(mload(0x103800), mload(0x102e00), f_q))
            mstore(0x103840, mulmod(sub(f_q, mload(0x103820)), mload(0x102f80), f_q))
            mstore(0x103860, mulmod(mload(0x1031c0), mload(0x102f80), f_q))
            mstore(0x103880, addmod(mload(0x1037e0), mload(0x103840), f_q))
            mstore(0x1038a0, mulmod(mload(0x1021a0), mload(0x102ba0), f_q))
            mstore(0x1038c0, mulmod(mload(0x1021c0), mload(0x102ba0), f_q))
            {
                let result := mulmod(mload(0x1021e0), mload(0x1025c0), f_q)
                mstore(0x1038e0, result)
            }
            mstore(0x103900, mulmod(mload(0x1038e0), mload(0x102e00), f_q))
            mstore(0x103920, mulmod(sub(f_q, mload(0x103900)), mload(0x102fa0), f_q))
            mstore(0x103940, mulmod(mload(0x1031c0), mload(0x102fa0), f_q))
            mstore(0x103960, mulmod(mload(0x1038a0), mload(0x102fa0), f_q))
            mstore(0x103980, mulmod(mload(0x1038c0), mload(0x102fa0), f_q))
            mstore(0x1039a0, addmod(mload(0x103880), mload(0x103920), f_q))
            {
                let result := mulmod(mload(0x100780), mload(0x1025c0), f_q)
                mstore(0x1039c0, result)
            }
            mstore(0x1039e0, mulmod(mload(0x1039c0), mload(0x102e00), f_q))
            mstore(0x103a00, mulmod(sub(f_q, mload(0x1039e0)), mload(0x102fc0), f_q))
            mstore(0x103a20, mulmod(mload(0x1031c0), mload(0x102fc0), f_q))
            mstore(0x103a40, addmod(mload(0x1039a0), mload(0x103a00), f_q))
            mstore(0x103a60, mulmod(mload(0x103a40), mload(0x100a60), f_q))
            mstore(0x103a80, mulmod(mload(0x103240), mload(0x100a60), f_q))
            mstore(0x103aa0, mulmod(mload(0x1032c0), mload(0x100a60), f_q))
            mstore(0x103ac0, mulmod(mload(0x103360), mload(0x100a60), f_q))
            mstore(0x103ae0, mulmod(mload(0x103400), mload(0x100a60), f_q))
            mstore(0x103b00, mulmod(mload(0x1034a0), mload(0x100a60), f_q))
            mstore(0x103b20, mulmod(mload(0x103540), mload(0x100a60), f_q))
            mstore(0x103b40, mulmod(mload(0x1035e0), mload(0x100a60), f_q))
            mstore(0x103b60, mulmod(mload(0x103680), mload(0x100a60), f_q))
            mstore(0x103b80, mulmod(mload(0x103720), mload(0x100a60), f_q))
            mstore(0x103ba0, mulmod(mload(0x1037c0), mload(0x100a60), f_q))
            mstore(0x103bc0, mulmod(mload(0x103860), mload(0x100a60), f_q))
            mstore(0x103be0, mulmod(mload(0x103940), mload(0x100a60), f_q))
            mstore(0x103c00, mulmod(mload(0x103960), mload(0x100a60), f_q))
            mstore(0x103c20, mulmod(mload(0x103980), mload(0x100a60), f_q))
            mstore(0x103c40, mulmod(mload(0x103a20), mload(0x100a60), f_q))
            mstore(0x103c60, addmod(mload(0x103180), mload(0x103a60), f_q))
            mstore(0x103c80, mulmod(1, mload(0x102be0), f_q))
            {
                let result := mulmod(mload(0x100840), mload(0x102620), f_q)
                result := addmod(mulmod(mload(0x100860), mload(0x102680), f_q), result, f_q)
                result := addmod(mulmod(mload(0x100880), mload(0x1026e0), f_q), result, f_q)
                mstore(0x103ca0, result)
            }
            mstore(0x103cc0, mulmod(mload(0x103ca0), mload(0x102e20), f_q))
            mstore(0x103ce0, mulmod(sub(f_q, mload(0x103cc0)), 1, f_q))
            mstore(0x103d00, mulmod(mload(0x103c80), 1, f_q))
            {
                let result := mulmod(mload(0x1008a0), mload(0x102620), f_q)
                result := addmod(mulmod(mload(0x1008c0), mload(0x102680), f_q), result, f_q)
                result := addmod(mulmod(mload(0x1008e0), mload(0x1026e0), f_q), result, f_q)
                mstore(0x103d20, result)
            }
            mstore(0x103d40, mulmod(mload(0x103d20), mload(0x102e20), f_q))
            mstore(0x103d60, mulmod(sub(f_q, mload(0x103d40)), mload(0x100a00), f_q))
            mstore(0x103d80, mulmod(mload(0x103c80), mload(0x100a00), f_q))
            mstore(0x103da0, addmod(mload(0x103ce0), mload(0x103d60), f_q))
            mstore(0x103dc0, mulmod(mload(0x103da0), mload(0x103000), f_q))
            mstore(0x103de0, mulmod(mload(0x103d00), mload(0x103000), f_q))
            mstore(0x103e00, mulmod(mload(0x103d80), mload(0x103000), f_q))
            mstore(0x103e20, addmod(mload(0x103c60), mload(0x103dc0), f_q))
            mstore(0x103e40, mulmod(1, mload(0x102c20), f_q))
            {
                let result := mulmod(mload(0x100900), mload(0x102760), f_q)
                result := addmod(mulmod(mload(0x100920), mload(0x1027c0), f_q), result, f_q)
                mstore(0x103e60, result)
            }
            mstore(0x103e80, mulmod(mload(0x103e60), mload(0x102e40), f_q))
            mstore(0x103ea0, mulmod(sub(f_q, mload(0x103e80)), 1, f_q))
            mstore(0x103ec0, mulmod(mload(0x103e40), 1, f_q))
            {
                let result := mulmod(mload(0x100940), mload(0x102760), f_q)
                result := addmod(mulmod(mload(0x100960), mload(0x1027c0), f_q), result, f_q)
                mstore(0x103ee0, result)
            }
            mstore(0x103f00, mulmod(mload(0x103ee0), mload(0x102e40), f_q))
            mstore(0x103f20, mulmod(sub(f_q, mload(0x103f00)), mload(0x100a00), f_q))
            mstore(0x103f40, mulmod(mload(0x103e40), mload(0x100a00), f_q))
            mstore(0x103f60, addmod(mload(0x103ea0), mload(0x103f20), f_q))
            mstore(0x103f80, mulmod(mload(0x103f60), mload(0x103020), f_q))
            mstore(0x103fa0, mulmod(mload(0x103ec0), mload(0x103020), f_q))
            mstore(0x103fc0, mulmod(mload(0x103f40), mload(0x103020), f_q))
            mstore(0x103fe0, addmod(mload(0x103e20), mload(0x103f80), f_q))
            mstore(0x104000, mulmod(1, mload(0x102c60), f_q))
            {
                let result := mulmod(mload(0x100980), mload(0x102820), f_q)
                result := addmod(mulmod(mload(0x1009a0), mload(0x102880), f_q), result, f_q)
                mstore(0x104020, result)
            }
            mstore(0x104040, mulmod(mload(0x104020), mload(0x102e60), f_q))
            mstore(0x104060, mulmod(sub(f_q, mload(0x104040)), 1, f_q))
            mstore(0x104080, mulmod(mload(0x104000), 1, f_q))
            mstore(0x1040a0, mulmod(mload(0x104060), mload(0x103040), f_q))
            mstore(0x1040c0, mulmod(mload(0x104080), mload(0x103040), f_q))
            mstore(0x1040e0, addmod(mload(0x103fe0), mload(0x1040a0), f_q))
            mstore(0x104100, mulmod(1, mload(0x1025a0), f_q))
            mstore(0x104120, mulmod(1, mload(0x100b00), f_q))
            mstore(0x104140, 0x0000000000000000000000000000000000000000000000000000000000000001)
            mstore(0x104160, 0x0000000000000000000000000000000000000000000000000000000000000002)
            mstore(0x104180, mload(0x1040e0))
            success := and(eq(staticcall(gas(), 0x7, 0x104140, 0x60, 0x104140, 0x40), 1), success)
            mstore(0x1041a0, mload(0x104140))
            mstore(0x1041c0, mload(0x104160))
            mstore(0x1041e0, mload(0x1000c0))
            mstore(0x104200, mload(0x1000e0))
            success := and(eq(staticcall(gas(), 0x6, 0x1041a0, 0x80, 0x1041a0, 0x40), 1), success)
            mstore(0x104220, mload(0x100100))
            mstore(0x104240, mload(0x100120))
            mstore(0x104260, mload(0x1031a0))
            success := and(eq(staticcall(gas(), 0x7, 0x104220, 0x60, 0x104220, 0x40), 1), success)
            mstore(0x104280, mload(0x1041a0))
            mstore(0x1042a0, mload(0x1041c0))
            mstore(0x1042c0, mload(0x104220))
            mstore(0x1042e0, mload(0x104240))
            success := and(eq(staticcall(gas(), 0x6, 0x104280, 0x80, 0x104280, 0x40), 1), success)
            mstore(0x104300, mload(0x100140))
            mstore(0x104320, mload(0x100160))
            mstore(0x104340, mload(0x103a80))
            success := and(eq(staticcall(gas(), 0x7, 0x104300, 0x60, 0x104300, 0x40), 1), success)
            mstore(0x104360, mload(0x104280))
            mstore(0x104380, mload(0x1042a0))
            mstore(0x1043a0, mload(0x104300))
            mstore(0x1043c0, mload(0x104320))
            success := and(eq(staticcall(gas(), 0x6, 0x104360, 0x80, 0x104360, 0x40), 1), success)
            mstore(0x1043e0, mload(0x100220))
            mstore(0x104400, mload(0x100240))
            mstore(0x104420, mload(0x103aa0))
            success := and(eq(staticcall(gas(), 0x7, 0x1043e0, 0x60, 0x1043e0, 0x40), 1), success)
            mstore(0x104440, mload(0x104360))
            mstore(0x104460, mload(0x104380))
            mstore(0x104480, mload(0x1043e0))
            mstore(0x1044a0, mload(0x104400))
            success := and(eq(staticcall(gas(), 0x6, 0x104440, 0x80, 0x104440, 0x40), 1), success)
            mstore(0x1044c0, 0x1e53727d5393674ddfde40c2e3d608e852acbc14ae92106577cdb169eb18e417)
            mstore(0x1044e0, 0x1eee37dcfc84b1e855ea81071b4fb766ec926fe9512ce0c22452b1adc8acf502)
            mstore(0x104500, mload(0x103ac0))
            success := and(eq(staticcall(gas(), 0x7, 0x1044c0, 0x60, 0x1044c0, 0x40), 1), success)
            mstore(0x104520, mload(0x104440))
            mstore(0x104540, mload(0x104460))
            mstore(0x104560, mload(0x1044c0))
            mstore(0x104580, mload(0x1044e0))
            success := and(eq(staticcall(gas(), 0x6, 0x104520, 0x80, 0x104520, 0x40), 1), success)
            mstore(0x1045a0, 0x2f212be54542ade2116bc81277ee498cc5a02609e3b3bd0432ea6e66b295c3ed)
            mstore(0x1045c0, 0x07619d7ddbf1bd3f7bb8ca42063d0cf35ed2052a83bd8f84a651950603d76c54)
            mstore(0x1045e0, mload(0x103ae0))
            success := and(eq(staticcall(gas(), 0x7, 0x1045a0, 0x60, 0x1045a0, 0x40), 1), success)
            mstore(0x104600, mload(0x104520))
            mstore(0x104620, mload(0x104540))
            mstore(0x104640, mload(0x1045a0))
            mstore(0x104660, mload(0x1045c0))
            success := and(eq(staticcall(gas(), 0x6, 0x104600, 0x80, 0x104600, 0x40), 1), success)
            mstore(0x104680, 0x26568de1703d076bd6eed477fc887c89567b33e13a7a7485aec498591460b6ff)
            mstore(0x1046a0, 0x1efda81e0e9e9327fd7fa3def0eaf208fbb5c2e5ba3cd66c8cbd88408f1e2e0c)
            mstore(0x1046c0, mload(0x103b00))
            success := and(eq(staticcall(gas(), 0x7, 0x104680, 0x60, 0x104680, 0x40), 1), success)
            mstore(0x1046e0, mload(0x104600))
            mstore(0x104700, mload(0x104620))
            mstore(0x104720, mload(0x104680))
            mstore(0x104740, mload(0x1046a0))
            success := and(eq(staticcall(gas(), 0x6, 0x1046e0, 0x80, 0x1046e0, 0x40), 1), success)
            mstore(0x104760, 0x26384a33ffd26fa1688c973a8e43076b9d688abd6d3c5b38678e64b00fa9ff66)
            mstore(0x104780, 0x17972472f0d004f184cf775943ee6154c762166a4318abd9ae912fff2b3804d2)
            mstore(0x1047a0, mload(0x103b20))
            success := and(eq(staticcall(gas(), 0x7, 0x104760, 0x60, 0x104760, 0x40), 1), success)
            mstore(0x1047c0, mload(0x1046e0))
            mstore(0x1047e0, mload(0x104700))
            mstore(0x104800, mload(0x104760))
            mstore(0x104820, mload(0x104780))
            success := and(eq(staticcall(gas(), 0x6, 0x1047c0, 0x80, 0x1047c0, 0x40), 1), success)
            mstore(0x104840, 0x179e8736bc70e3c4bbffec6b2b655931c2d5c27c83d1ccd40736a7811854ecd7)
            mstore(0x104860, 0x0632c233013a54f6de5d30ac7a21afb57a53d4abce0d6faca96e44069492c8f5)
            mstore(0x104880, mload(0x103b40))
            success := and(eq(staticcall(gas(), 0x7, 0x104840, 0x60, 0x104840, 0x40), 1), success)
            mstore(0x1048a0, mload(0x1047c0))
            mstore(0x1048c0, mload(0x1047e0))
            mstore(0x1048e0, mload(0x104840))
            mstore(0x104900, mload(0x104860))
            success := and(eq(staticcall(gas(), 0x6, 0x1048a0, 0x80, 0x1048a0, 0x40), 1), success)
            mstore(0x104920, 0x07f6d6d1999d5018073d545bb0564e8208a725c6a1a6f6fdfae5858db6fc6c62)
            mstore(0x104940, 0x2282000f0f7daafb6b2ee57823510d7fddb05fb7d2de6dfd1443cafb57d1b9b6)
            mstore(0x104960, mload(0x103b60))
            success := and(eq(staticcall(gas(), 0x7, 0x104920, 0x60, 0x104920, 0x40), 1), success)
            mstore(0x104980, mload(0x1048a0))
            mstore(0x1049a0, mload(0x1048c0))
            mstore(0x1049c0, mload(0x104920))
            mstore(0x1049e0, mload(0x104940))
            success := and(eq(staticcall(gas(), 0x6, 0x104980, 0x80, 0x104980, 0x40), 1), success)
            mstore(0x104a00, 0x124c334d20d497909c9c8f2589e18ab4c3c615542e4da535f471d425adeacd4d)
            mstore(0x104a20, 0x05e981debd992b50938200969b3d1b7174ffe8baf02b2d8f339960213dcd564d)
            mstore(0x104a40, mload(0x103b80))
            success := and(eq(staticcall(gas(), 0x7, 0x104a00, 0x60, 0x104a00, 0x40), 1), success)
            mstore(0x104a60, mload(0x104980))
            mstore(0x104a80, mload(0x1049a0))
            mstore(0x104aa0, mload(0x104a00))
            mstore(0x104ac0, mload(0x104a20))
            success := and(eq(staticcall(gas(), 0x6, 0x104a60, 0x80, 0x104a60, 0x40), 1), success)
            mstore(0x104ae0, 0x099e73fae61059f3ee2f2697470ce5dfd730776a6ab7e1a62c498ab96e1f28fe)
            mstore(0x104b00, 0x11b9c6500393f8407441c9a90c986160807821bf82617b7ed349b80bb08ecdfa)
            mstore(0x104b20, mload(0x103ba0))
            success := and(eq(staticcall(gas(), 0x7, 0x104ae0, 0x60, 0x104ae0, 0x40), 1), success)
            mstore(0x104b40, mload(0x104a60))
            mstore(0x104b60, mload(0x104a80))
            mstore(0x104b80, mload(0x104ae0))
            mstore(0x104ba0, mload(0x104b00))
            success := and(eq(staticcall(gas(), 0x6, 0x104b40, 0x80, 0x104b40, 0x40), 1), success)
            mstore(0x104bc0, 0x2e7b0fb7554b86ecfb3eccb11975751e761032c46968c17dbfc080d58c9bc8ce)
            mstore(0x104be0, 0x29f5da6f5a16d9a5a6ae81f205312dd04ca0092b7c0d31b7e6ca37159f7774ea)
            mstore(0x104c00, mload(0x103bc0))
            success := and(eq(staticcall(gas(), 0x7, 0x104bc0, 0x60, 0x104bc0, 0x40), 1), success)
            mstore(0x104c20, mload(0x104b40))
            mstore(0x104c40, mload(0x104b60))
            mstore(0x104c60, mload(0x104bc0))
            mstore(0x104c80, mload(0x104be0))
            success := and(eq(staticcall(gas(), 0x6, 0x104c20, 0x80, 0x104c20, 0x40), 1), success)
            mstore(0x104ca0, mload(0x1004c0))
            mstore(0x104cc0, mload(0x1004e0))
            mstore(0x104ce0, mload(0x103be0))
            success := and(eq(staticcall(gas(), 0x7, 0x104ca0, 0x60, 0x104ca0, 0x40), 1), success)
            mstore(0x104d00, mload(0x104c20))
            mstore(0x104d20, mload(0x104c40))
            mstore(0x104d40, mload(0x104ca0))
            mstore(0x104d60, mload(0x104cc0))
            success := and(eq(staticcall(gas(), 0x6, 0x104d00, 0x80, 0x104d00, 0x40), 1), success)
            mstore(0x104d80, mload(0x100500))
            mstore(0x104da0, mload(0x100520))
            mstore(0x104dc0, mload(0x103c00))
            success := and(eq(staticcall(gas(), 0x7, 0x104d80, 0x60, 0x104d80, 0x40), 1), success)
            mstore(0x104de0, mload(0x104d00))
            mstore(0x104e00, mload(0x104d20))
            mstore(0x104e20, mload(0x104d80))
            mstore(0x104e40, mload(0x104da0))
            success := and(eq(staticcall(gas(), 0x6, 0x104de0, 0x80, 0x104de0, 0x40), 1), success)
            mstore(0x104e60, mload(0x100540))
            mstore(0x104e80, mload(0x100560))
            mstore(0x104ea0, mload(0x103c20))
            success := and(eq(staticcall(gas(), 0x7, 0x104e60, 0x60, 0x104e60, 0x40), 1), success)
            mstore(0x104ec0, mload(0x104de0))
            mstore(0x104ee0, mload(0x104e00))
            mstore(0x104f00, mload(0x104e60))
            mstore(0x104f20, mload(0x104e80))
            success := and(eq(staticcall(gas(), 0x6, 0x104ec0, 0x80, 0x104ec0, 0x40), 1), success)
            mstore(0x104f40, mload(0x100420))
            mstore(0x104f60, mload(0x100440))
            mstore(0x104f80, mload(0x103c40))
            success := and(eq(staticcall(gas(), 0x7, 0x104f40, 0x60, 0x104f40, 0x40), 1), success)
            mstore(0x104fa0, mload(0x104ec0))
            mstore(0x104fc0, mload(0x104ee0))
            mstore(0x104fe0, mload(0x104f40))
            mstore(0x105000, mload(0x104f60))
            success := and(eq(staticcall(gas(), 0x6, 0x104fa0, 0x80, 0x104fa0, 0x40), 1), success)
            mstore(0x105020, mload(0x100320))
            mstore(0x105040, mload(0x100340))
            mstore(0x105060, mload(0x103de0))
            success := and(eq(staticcall(gas(), 0x7, 0x105020, 0x60, 0x105020, 0x40), 1), success)
            mstore(0x105080, mload(0x104fa0))
            mstore(0x1050a0, mload(0x104fc0))
            mstore(0x1050c0, mload(0x105020))
            mstore(0x1050e0, mload(0x105040))
            success := and(eq(staticcall(gas(), 0x6, 0x105080, 0x80, 0x105080, 0x40), 1), success)
            mstore(0x105100, mload(0x100360))
            mstore(0x105120, mload(0x100380))
            mstore(0x105140, mload(0x103e00))
            success := and(eq(staticcall(gas(), 0x7, 0x105100, 0x60, 0x105100, 0x40), 1), success)
            mstore(0x105160, mload(0x105080))
            mstore(0x105180, mload(0x1050a0))
            mstore(0x1051a0, mload(0x105100))
            mstore(0x1051c0, mload(0x105120))
            success := and(eq(staticcall(gas(), 0x6, 0x105160, 0x80, 0x105160, 0x40), 1), success)
            mstore(0x1051e0, mload(0x1003a0))
            mstore(0x105200, mload(0x1003c0))
            mstore(0x105220, mload(0x103fa0))
            success := and(eq(staticcall(gas(), 0x7, 0x1051e0, 0x60, 0x1051e0, 0x40), 1), success)
            mstore(0x105240, mload(0x105160))
            mstore(0x105260, mload(0x105180))
            mstore(0x105280, mload(0x1051e0))
            mstore(0x1052a0, mload(0x105200))
            success := and(eq(staticcall(gas(), 0x6, 0x105240, 0x80, 0x105240, 0x40), 1), success)
            mstore(0x1052c0, mload(0x1003e0))
            mstore(0x1052e0, mload(0x100400))
            mstore(0x105300, mload(0x103fc0))
            success := and(eq(staticcall(gas(), 0x7, 0x1052c0, 0x60, 0x1052c0, 0x40), 1), success)
            mstore(0x105320, mload(0x105240))
            mstore(0x105340, mload(0x105260))
            mstore(0x105360, mload(0x1052c0))
            mstore(0x105380, mload(0x1052e0))
            success := and(eq(staticcall(gas(), 0x6, 0x105320, 0x80, 0x105320, 0x40), 1), success)
            mstore(0x1053a0, mload(0x1001e0))
            mstore(0x1053c0, mload(0x100200))
            mstore(0x1053e0, mload(0x1040c0))
            success := and(eq(staticcall(gas(), 0x7, 0x1053a0, 0x60, 0x1053a0, 0x40), 1), success)
            mstore(0x105400, mload(0x105320))
            mstore(0x105420, mload(0x105340))
            mstore(0x105440, mload(0x1053a0))
            mstore(0x105460, mload(0x1053c0))
            success := and(eq(staticcall(gas(), 0x6, 0x105400, 0x80, 0x105400, 0x40), 1), success)
            mstore(0x105480, mload(0x100aa0))
            mstore(0x1054a0, mload(0x100ac0))
            mstore(0x1054c0, sub(f_q, mload(0x104100)))
            success := and(eq(staticcall(gas(), 0x7, 0x105480, 0x60, 0x105480, 0x40), 1), success)
            mstore(0x1054e0, mload(0x105400))
            mstore(0x105500, mload(0x105420))
            mstore(0x105520, mload(0x105480))
            mstore(0x105540, mload(0x1054a0))
            success := and(eq(staticcall(gas(), 0x6, 0x1054e0, 0x80, 0x1054e0, 0x40), 1), success)
            mstore(0x105560, mload(0x100b40))
            mstore(0x105580, mload(0x100b60))
            mstore(0x1055a0, mload(0x104120))
            success := and(eq(staticcall(gas(), 0x7, 0x105560, 0x60, 0x105560, 0x40), 1), success)
            mstore(0x1055c0, mload(0x1054e0))
            mstore(0x1055e0, mload(0x105500))
            mstore(0x105600, mload(0x105560))
            mstore(0x105620, mload(0x105580))
            success := and(eq(staticcall(gas(), 0x6, 0x1055c0, 0x80, 0x1055c0, 0x40), 1), success)
            mstore(0x105640, mload(0x1055c0))
            mstore(0x105660, mload(0x1055e0))
            mstore(0x105680, 0x198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2)
            mstore(0x1056a0, 0x1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed)
            mstore(0x1056c0, 0x090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b)
            mstore(0x1056e0, 0x12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa)
            mstore(0x105700, mload(0x100b40))
            mstore(0x105720, mload(0x100b60))
            mstore(0x105740, 0x0181624e80f3d6ae28df7e01eaeab1c0e919877a3b8a6b7fbc69a6817d596ea2)
            mstore(0x105760, 0x1783d30dcb12d259bb89098addf6280fa4b653be7a152542a28f7b926e27e648)
            mstore(0x105780, 0x00ae44489d41a0d179e2dfdc03bddd883b7109f8b6ae316a59e815c1a6b35304)
            mstore(0x1057a0, 0x0b2147ab62a386bd63e6de1522109b8c9588ab466f5aadfde8c41ca3749423ee)
            success := and(eq(staticcall(gas(), 0x8, 0x105640, 0x180, 0x105640, 0x20), 1), success)
            success := and(eq(mload(0x105640), 1), success)

            // Revert if anything fails
            if iszero(success) { revert(0, 0) }

            // Return empty bytes on success
            return(0, 0)
        }
    }
}
