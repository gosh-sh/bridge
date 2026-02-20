// SPDX-License-Identifier: MIT

pragma solidity ^0.8.19;

contract Halo2Verifier {
    fallback(bytes calldata) external returns (bytes memory) {
        assembly ("memory-safe") {
            // Enforce that Solidity memory layout is respected
            let data := mload(0x40)
            if iszero(eq(data, 0x80)) {
                revert(0, 0)
            }

            let success := true
            let f_p := 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
            let f_q := 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001
            function validate_ec_point(x, y) -> valid {
                {
                    let x_lt_p :=
                        lt(x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let y_lt_p :=
                        lt(y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    valid := and(x_lt_p, y_lt_p)
                }
                {
                    let y_square :=
                        mulmod(
                            y,
                            y,
                            0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
                        )
                    let x_square :=
                        mulmod(
                            x,
                            x,
                            0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
                        )
                    let x_cube :=
                        mulmod(
                            x_square,
                            x,
                            0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
                        )
                    let x_cube_plus_3 :=
                        addmod(
                            x_cube,
                            3,
                            0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
                        )
                    let is_affine := eq(x_cube_plus_3, y_square)
                    valid := and(valid, is_affine)
                }
            }
            mstore(0xa0, mod(calldataload(0x0), f_q))
            mstore(0xc0, mod(calldataload(0x20), f_q))
            mstore(0xe0, mod(calldataload(0x40), f_q))
            mstore(
                0x80,
                12254604387300489704510947836340974478925549543029056780612540686516654961079
            )

            {
                let x := calldataload(0x60)
                mstore(0x100, x)
                let y := calldataload(0x80)
                mstore(0x120, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0xa0)
                mstore(0x140, x)
                let y := calldataload(0xc0)
                mstore(0x160, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0xe0)
                mstore(0x180, x)
                let y := calldataload(0x100)
                mstore(0x1a0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x120)
                mstore(0x1c0, x)
                let y := calldataload(0x140)
                mstore(0x1e0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x160)
                mstore(0x200, x)
                let y := calldataload(0x180)
                mstore(0x220, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x240, keccak256(0x80, 448))
            {
                let hash := mload(0x240)
                mstore(0x260, mod(hash, f_q))
                mstore(0x280, hash)
            }

            {
                let x := calldataload(0x1a0)
                mstore(0x2a0, x)
                let y := calldataload(0x1c0)
                mstore(0x2c0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x1e0)
                mstore(0x2e0, x)
                let y := calldataload(0x200)
                mstore(0x300, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x320, keccak256(0x280, 160))
            {
                let hash := mload(0x320)
                mstore(0x340, mod(hash, f_q))
                mstore(0x360, hash)
            }
            mstore8(896, 1)
            mstore(0x380, keccak256(0x360, 33))
            {
                let hash := mload(0x380)
                mstore(0x3a0, mod(hash, f_q))
                mstore(0x3c0, hash)
            }

            {
                let x := calldataload(0x220)
                mstore(0x3e0, x)
                let y := calldataload(0x240)
                mstore(0x400, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x260)
                mstore(0x420, x)
                let y := calldataload(0x280)
                mstore(0x440, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x2a0)
                mstore(0x460, x)
                let y := calldataload(0x2c0)
                mstore(0x480, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x2e0)
                mstore(0x4a0, x)
                let y := calldataload(0x300)
                mstore(0x4c0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x320)
                mstore(0x4e0, x)
                let y := calldataload(0x340)
                mstore(0x500, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x360)
                mstore(0x520, x)
                let y := calldataload(0x380)
                mstore(0x540, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x560, keccak256(0x3c0, 416))
            {
                let hash := mload(0x560)
                mstore(0x580, mod(hash, f_q))
                mstore(0x5a0, hash)
            }

            {
                let x := calldataload(0x3a0)
                mstore(0x5c0, x)
                let y := calldataload(0x3c0)
                mstore(0x5e0, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x3e0)
                mstore(0x600, x)
                let y := calldataload(0x400)
                mstore(0x620, y)
                success := and(validate_ec_point(x, y), success)
            }

            {
                let x := calldataload(0x420)
                mstore(0x640, x)
                let y := calldataload(0x440)
                mstore(0x660, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0x680, keccak256(0x5a0, 224))
            {
                let hash := mload(0x680)
                mstore(0x6a0, mod(hash, f_q))
                mstore(0x6c0, hash)
            }
            mstore(0x6e0, mod(calldataload(0x460), f_q))
            mstore(0x700, mod(calldataload(0x480), f_q))
            mstore(0x720, mod(calldataload(0x4a0), f_q))
            mstore(0x740, mod(calldataload(0x4c0), f_q))
            mstore(0x760, mod(calldataload(0x4e0), f_q))
            mstore(0x780, mod(calldataload(0x500), f_q))
            mstore(0x7a0, mod(calldataload(0x520), f_q))
            mstore(0x7c0, mod(calldataload(0x540), f_q))
            mstore(0x7e0, mod(calldataload(0x560), f_q))
            mstore(0x800, mod(calldataload(0x580), f_q))
            mstore(0x820, mod(calldataload(0x5a0), f_q))
            mstore(0x840, mod(calldataload(0x5c0), f_q))
            mstore(0x860, mod(calldataload(0x5e0), f_q))
            mstore(0x880, mod(calldataload(0x600), f_q))
            mstore(0x8a0, mod(calldataload(0x620), f_q))
            mstore(0x8c0, mod(calldataload(0x640), f_q))
            mstore(0x8e0, mod(calldataload(0x660), f_q))
            mstore(0x900, mod(calldataload(0x680), f_q))
            mstore(0x920, mod(calldataload(0x6a0), f_q))
            mstore(0x940, mod(calldataload(0x6c0), f_q))
            mstore(0x960, mod(calldataload(0x6e0), f_q))
            mstore(0x980, mod(calldataload(0x700), f_q))
            mstore(0x9a0, mod(calldataload(0x720), f_q))
            mstore(0x9c0, mod(calldataload(0x740), f_q))
            mstore(0x9e0, mod(calldataload(0x760), f_q))
            mstore(0xa00, mod(calldataload(0x780), f_q))
            mstore(0xa20, mod(calldataload(0x7a0), f_q))
            mstore(0xa40, mod(calldataload(0x7c0), f_q))
            mstore(0xa60, mod(calldataload(0x7e0), f_q))
            mstore(0xa80, mod(calldataload(0x800), f_q))
            mstore(0xaa0, mod(calldataload(0x820), f_q))
            mstore(0xac0, mod(calldataload(0x840), f_q))
            mstore(0xae0, mod(calldataload(0x860), f_q))
            mstore(0xb00, mod(calldataload(0x880), f_q))
            mstore(0xb20, mod(calldataload(0x8a0), f_q))
            mstore(0xb40, mod(calldataload(0x8c0), f_q))
            mstore(0xb60, mod(calldataload(0x8e0), f_q))
            mstore(0xb80, mod(calldataload(0x900), f_q))
            mstore(0xba0, mod(calldataload(0x920), f_q))
            mstore(0xbc0, mod(calldataload(0x940), f_q))
            mstore(0xbe0, mod(calldataload(0x960), f_q))
            mstore(0xc00, mod(calldataload(0x980), f_q))
            mstore(0xc20, mod(calldataload(0x9a0), f_q))
            mstore(0xc40, mod(calldataload(0x9c0), f_q))
            mstore(0xc60, mod(calldataload(0x9e0), f_q))
            mstore(0xc80, mod(calldataload(0xa00), f_q))
            mstore(0xca0, mod(calldataload(0xa20), f_q))
            mstore(0xcc0, keccak256(0x6c0, 1536))
            {
                let hash := mload(0xcc0)
                mstore(0xce0, mod(hash, f_q))
                mstore(0xd00, hash)
            }
            mstore8(3360, 1)
            mstore(0xd20, keccak256(0xd00, 33))
            {
                let hash := mload(0xd20)
                mstore(0xd40, mod(hash, f_q))
                mstore(0xd60, hash)
            }

            {
                let x := calldataload(0xa40)
                mstore(0xd80, x)
                let y := calldataload(0xa60)
                mstore(0xda0, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0xdc0, keccak256(0xd60, 96))
            {
                let hash := mload(0xdc0)
                mstore(0xde0, mod(hash, f_q))
                mstore(0xe00, hash)
            }

            {
                let x := calldataload(0xa80)
                mstore(0xe20, x)
                let y := calldataload(0xaa0)
                mstore(0xe40, y)
                success := and(validate_ec_point(x, y), success)
            }
            mstore(0xe60, mulmod(mload(0x6a0), mload(0x6a0), f_q))
            mstore(0xe80, mulmod(mload(0xe60), mload(0xe60), f_q))
            mstore(0xea0, mulmod(mload(0xe80), mload(0xe80), f_q))
            mstore(0xec0, mulmod(mload(0xea0), mload(0xea0), f_q))
            mstore(0xee0, mulmod(mload(0xec0), mload(0xec0), f_q))
            mstore(0xf00, mulmod(mload(0xee0), mload(0xee0), f_q))
            mstore(0xf20, mulmod(mload(0xf00), mload(0xf00), f_q))
            mstore(0xf40, mulmod(mload(0xf20), mload(0xf20), f_q))
            mstore(0xf60, mulmod(mload(0xf40), mload(0xf40), f_q))
            mstore(0xf80, mulmod(mload(0xf60), mload(0xf60), f_q))
            mstore(0xfa0, mulmod(mload(0xf80), mload(0xf80), f_q))
            mstore(0xfc0, mulmod(mload(0xfa0), mload(0xfa0), f_q))
            mstore(
                0xfe0,
                addmod(
                    mload(0xfc0),
                    21888242871839275222246405745257275088548364400416034343698204186575808495616,
                    f_q
                )
            )
            mstore(
                0x1000,
                mulmod(
                    mload(0xfe0),
                    21882899062544392586694099493854624386622449272388589022813512242194320261121,
                    f_q
                )
            )
            mstore(
                0x1020,
                mulmod(
                    mload(0x1000),
                    13494463686150534520302493401520574237656912536907614359831638685825304135949,
                    f_q
                )
            )
            mstore(
                0x1040,
                addmod(
                    mload(0x6a0),
                    8393779185688740701943912343736700850891451863508419983866565500750504359668,
                    f_q
                )
            )
            mstore(
                0x1060,
                mulmod(
                    mload(0x1000),
                    18302882236472339419631414285403968768409802182737928837767912484847322191909,
                    f_q
                )
            )
            mstore(
                0x1080,
                addmod(
                    mload(0x6a0),
                    3585360635366935802614991459853306320138562217678105505930291701728486303708,
                    f_q
                )
            )
            mstore(
                0x10a0,
                mulmod(
                    mload(0x1000),
                    11537035432936037313763253554381703723723793827542696094189990411849785474670,
                    f_q
                )
            )
            mstore(
                0x10c0,
                addmod(
                    mload(0x6a0),
                    10351207438903237908483152190875571364824570572873338249508213774726023020947,
                    f_q
                )
            )
            mstore(
                0x10e0,
                mulmod(
                    mload(0x1000),
                    4925592601992654644734291590386747644864797672605745962807370354577123815907,
                    f_q
                )
            )
            mstore(
                0x1100,
                addmod(
                    mload(0x6a0),
                    16962650269846620577512114154870527443683566727810288380890833831998684679710,
                    f_q
                )
            )
            mstore(
                0x1120,
                mulmod(
                    mload(0x1000),
                    14428378809216400477736413013847344056809954101862299032583274736599544656045,
                    f_q
                )
            )
            mstore(
                0x1140,
                addmod(
                    mload(0x6a0),
                    7459864062622874744509992731409931031738410298553735311114929449976263839572,
                    f_q
                )
            )
            mstore(
                0x1160,
                mulmod(
                    mload(0x1000),
                    19444693496467964793333684482470811869395409953158764080291550423779334624794,
                    f_q
                )
            )
            mstore(
                0x1180,
                addmod(
                    mload(0x6a0),
                    2443549375371310428912721262786463219152954447257270263406653762796473870823,
                    f_q
                )
            )
            mstore(
                0x11a0,
                mulmod(
                    mload(0x1000),
                    10679069158860809785885364198325818746230765378937472123583344754591056515264,
                    f_q
                )
            )
            mstore(
                0x11c0,
                addmod(
                    mload(0x6a0),
                    11209173712978465436361041546931456342317599021478562220114859431984751980353,
                    f_q
                )
            )
            mstore(0x11e0, mulmod(mload(0x1000), 1, f_q))
            mstore(
                0x1200,
                addmod(
                    mload(0x6a0),
                    21888242871839275222246405745257275088548364400416034343698204186575808495616,
                    f_q
                )
            )
            mstore(
                0x1220,
                mulmod(
                    mload(0x1000),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            mstore(
                0x1240,
                addmod(
                    mload(0x6a0),
                    457915096789217363190654424344135916650651035271458877272133377425876816155,
                    f_q
                )
            )
            mstore(
                0x1260,
                mulmod(
                    mload(0x1000),
                    9396103202274256930945606623206526900461945684265495839012435492634193195103,
                    f_q
                )
            )
            mstore(
                0x1280,
                addmod(
                    mload(0x6a0),
                    12492139669565018291300799122050748188086418716150538504685768693941615300514,
                    f_q
                )
            )
            {
                let prod := mload(0x1040)

                prod := mulmod(mload(0x1080), prod, f_q)
                mstore(0x12a0, prod)

                prod := mulmod(mload(0x10c0), prod, f_q)
                mstore(0x12c0, prod)

                prod := mulmod(mload(0x1100), prod, f_q)
                mstore(0x12e0, prod)

                prod := mulmod(mload(0x1140), prod, f_q)
                mstore(0x1300, prod)

                prod := mulmod(mload(0x1180), prod, f_q)
                mstore(0x1320, prod)

                prod := mulmod(mload(0x11c0), prod, f_q)
                mstore(0x1340, prod)

                prod := mulmod(mload(0x1200), prod, f_q)
                mstore(0x1360, prod)

                prod := mulmod(mload(0x1240), prod, f_q)
                mstore(0x1380, prod)

                prod := mulmod(mload(0x1280), prod, f_q)
                mstore(0x13a0, prod)

                prod := mulmod(mload(0xfe0), prod, f_q)
                mstore(0x13c0, prod)
            }
            mstore(0x1400, 32)
            mstore(0x1420, 32)
            mstore(0x1440, 32)
            mstore(0x1460, mload(0x13c0))
            mstore(
                0x1480,
                21888242871839275222246405745257275088548364400416034343698204186575808495615
            )
            mstore(
                0x14a0,
                21888242871839275222246405745257275088548364400416034343698204186575808495617
            )
            success := and(eq(staticcall(gas(), 0x5, 0x1400, 0xc0, 0x13e0, 0x20), 1), success)
            {
                let inv := mload(0x13e0)
                let v

                v := mload(0xfe0)
                mstore(4064, mulmod(mload(0x13a0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1280)
                mstore(4736, mulmod(mload(0x1380), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1240)
                mstore(4672, mulmod(mload(0x1360), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1200)
                mstore(4608, mulmod(mload(0x1340), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x11c0)
                mstore(4544, mulmod(mload(0x1320), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1180)
                mstore(4480, mulmod(mload(0x1300), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1140)
                mstore(4416, mulmod(mload(0x12e0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1100)
                mstore(4352, mulmod(mload(0x12c0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x10c0)
                mstore(4288, mulmod(mload(0x12a0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x1080)
                mstore(4224, mulmod(mload(0x1040), inv, f_q))
                inv := mulmod(v, inv, f_q)
                mstore(0x1040, inv)
            }
            mstore(0x14c0, mulmod(mload(0x1020), mload(0x1040), f_q))
            mstore(0x14e0, mulmod(mload(0x1060), mload(0x1080), f_q))
            mstore(0x1500, mulmod(mload(0x10a0), mload(0x10c0), f_q))
            mstore(0x1520, mulmod(mload(0x10e0), mload(0x1100), f_q))
            mstore(0x1540, mulmod(mload(0x1120), mload(0x1140), f_q))
            mstore(0x1560, mulmod(mload(0x1160), mload(0x1180), f_q))
            mstore(0x1580, mulmod(mload(0x11a0), mload(0x11c0), f_q))
            mstore(0x15a0, mulmod(mload(0x11e0), mload(0x1200), f_q))
            mstore(0x15c0, mulmod(mload(0x1220), mload(0x1240), f_q))
            mstore(0x15e0, mulmod(mload(0x1260), mload(0x1280), f_q))
            {
                let result := mulmod(mload(0x15a0), mload(0xa0), f_q)
                result := addmod(mulmod(mload(0x15c0), mload(0xc0), f_q), result, f_q)
                result := addmod(mulmod(mload(0x15e0), mload(0xe0), f_q), result, f_q)
                mstore(5632, result)
            }
            mstore(0x1620, mulmod(mload(0x720), mload(0x700), f_q))
            mstore(0x1640, addmod(mload(0x6e0), mload(0x1620), f_q))
            mstore(0x1660, addmod(mload(0x1640), sub(f_q, mload(0x740)), f_q))
            mstore(0x1680, mulmod(mload(0x1660), mload(0x940), f_q))
            mstore(0x16a0, mulmod(mload(0x580), mload(0x1680), f_q))
            mstore(0x16c0, mulmod(mload(0x7a0), mload(0x780), f_q))
            mstore(0x16e0, addmod(mload(0x760), mload(0x16c0), f_q))
            mstore(0x1700, addmod(mload(0x16e0), sub(f_q, mload(0x7c0)), f_q))
            mstore(0x1720, mulmod(mload(0x1700), mload(0x960), f_q))
            mstore(0x1740, addmod(mload(0x16a0), mload(0x1720), f_q))
            mstore(0x1760, mulmod(mload(0x580), mload(0x1740), f_q))
            mstore(0x1780, mulmod(mload(0x820), mload(0x800), f_q))
            mstore(0x17a0, addmod(mload(0x7e0), mload(0x1780), f_q))
            mstore(0x17c0, addmod(mload(0x17a0), sub(f_q, mload(0x840)), f_q))
            mstore(0x17e0, mulmod(mload(0x17c0), mload(0x980), f_q))
            mstore(0x1800, addmod(mload(0x1760), mload(0x17e0), f_q))
            mstore(0x1820, mulmod(mload(0x580), mload(0x1800), f_q))
            mstore(0x1840, mulmod(mload(0x8a0), mload(0x880), f_q))
            mstore(0x1860, addmod(mload(0x860), mload(0x1840), f_q))
            mstore(0x1880, addmod(mload(0x1860), sub(f_q, mload(0x8c0)), f_q))
            mstore(0x18a0, mulmod(mload(0x1880), mload(0x9a0), f_q))
            mstore(0x18c0, addmod(mload(0x1820), mload(0x18a0), f_q))
            mstore(0x18e0, mulmod(mload(0x580), mload(0x18c0), f_q))
            mstore(0x1900, addmod(1, sub(f_q, mload(0xac0)), f_q))
            mstore(0x1920, mulmod(mload(0x1900), mload(0x15a0), f_q))
            mstore(0x1940, addmod(mload(0x18e0), mload(0x1920), f_q))
            mstore(0x1960, mulmod(mload(0x580), mload(0x1940), f_q))
            mstore(0x1980, mulmod(mload(0xbe0), mload(0xbe0), f_q))
            mstore(0x19a0, addmod(mload(0x1980), sub(f_q, mload(0xbe0)), f_q))
            mstore(0x19c0, mulmod(mload(0x19a0), mload(0x14c0), f_q))
            mstore(0x19e0, addmod(mload(0x1960), mload(0x19c0), f_q))
            mstore(0x1a00, mulmod(mload(0x580), mload(0x19e0), f_q))
            mstore(0x1a20, addmod(mload(0xb20), sub(f_q, mload(0xb00)), f_q))
            mstore(0x1a40, mulmod(mload(0x1a20), mload(0x15a0), f_q))
            mstore(0x1a60, addmod(mload(0x1a00), mload(0x1a40), f_q))
            mstore(0x1a80, mulmod(mload(0x580), mload(0x1a60), f_q))
            mstore(0x1aa0, addmod(mload(0xb80), sub(f_q, mload(0xb60)), f_q))
            mstore(0x1ac0, mulmod(mload(0x1aa0), mload(0x15a0), f_q))
            mstore(0x1ae0, addmod(mload(0x1a80), mload(0x1ac0), f_q))
            mstore(0x1b00, mulmod(mload(0x580), mload(0x1ae0), f_q))
            mstore(0x1b20, addmod(mload(0xbe0), sub(f_q, mload(0xbc0)), f_q))
            mstore(0x1b40, mulmod(mload(0x1b20), mload(0x15a0), f_q))
            mstore(0x1b60, addmod(mload(0x1b00), mload(0x1b40), f_q))
            mstore(0x1b80, mulmod(mload(0x580), mload(0x1b60), f_q))
            mstore(0x1ba0, addmod(1, sub(f_q, mload(0x14c0)), f_q))
            mstore(0x1bc0, addmod(mload(0x14e0), mload(0x1500), f_q))
            mstore(0x1be0, addmod(mload(0x1bc0), mload(0x1520), f_q))
            mstore(0x1c00, addmod(mload(0x1be0), mload(0x1540), f_q))
            mstore(0x1c20, addmod(mload(0x1c00), mload(0x1560), f_q))
            mstore(0x1c40, addmod(mload(0x1c20), mload(0x1580), f_q))
            mstore(0x1c60, addmod(mload(0x1ba0), sub(f_q, mload(0x1c40)), f_q))
            mstore(0x1c80, mulmod(mload(0x9e0), mload(0x340), f_q))
            mstore(0x1ca0, addmod(mload(0x900), mload(0x1c80), f_q))
            mstore(0x1cc0, addmod(mload(0x1ca0), mload(0x3a0), f_q))
            mstore(0x1ce0, mulmod(mload(0xa00), mload(0x340), f_q))
            mstore(0x1d00, addmod(mload(0x6e0), mload(0x1ce0), f_q))
            mstore(0x1d20, addmod(mload(0x1d00), mload(0x3a0), f_q))
            mstore(0x1d40, mulmod(mload(0x1d20), mload(0x1cc0), f_q))
            mstore(0x1d60, mulmod(mload(0x1d40), mload(0xae0), f_q))
            mstore(0x1d80, mulmod(1, mload(0x340), f_q))
            mstore(0x1da0, mulmod(mload(0x6a0), mload(0x1d80), f_q))
            mstore(0x1dc0, addmod(mload(0x900), mload(0x1da0), f_q))
            mstore(0x1de0, addmod(mload(0x1dc0), mload(0x3a0), f_q))
            mstore(
                0x1e00,
                mulmod(
                    4131629893567559867359510883348571134090853742863529169391034518566172092834,
                    mload(0x340),
                    f_q
                )
            )
            mstore(0x1e20, mulmod(mload(0x6a0), mload(0x1e00), f_q))
            mstore(0x1e40, addmod(mload(0x6e0), mload(0x1e20), f_q))
            mstore(0x1e60, addmod(mload(0x1e40), mload(0x3a0), f_q))
            mstore(0x1e80, mulmod(mload(0x1e60), mload(0x1de0), f_q))
            mstore(0x1ea0, mulmod(mload(0x1e80), mload(0xac0), f_q))
            mstore(0x1ec0, addmod(mload(0x1d60), sub(f_q, mload(0x1ea0)), f_q))
            mstore(0x1ee0, mulmod(mload(0x1ec0), mload(0x1c60), f_q))
            mstore(0x1f00, addmod(mload(0x1b80), mload(0x1ee0), f_q))
            mstore(0x1f20, mulmod(mload(0x580), mload(0x1f00), f_q))
            mstore(0x1f40, mulmod(mload(0xa20), mload(0x340), f_q))
            mstore(0x1f60, addmod(mload(0x760), mload(0x1f40), f_q))
            mstore(0x1f80, addmod(mload(0x1f60), mload(0x3a0), f_q))
            mstore(0x1fa0, mulmod(mload(0xa40), mload(0x340), f_q))
            mstore(0x1fc0, addmod(mload(0x7e0), mload(0x1fa0), f_q))
            mstore(0x1fe0, addmod(mload(0x1fc0), mload(0x3a0), f_q))
            mstore(0x2000, mulmod(mload(0x1fe0), mload(0x1f80), f_q))
            mstore(0x2020, mulmod(mload(0x2000), mload(0xb40), f_q))
            mstore(
                0x2040,
                mulmod(
                    8910878055287538404433155982483128285667088683464058436815641868457422632747,
                    mload(0x340),
                    f_q
                )
            )
            mstore(0x2060, mulmod(mload(0x6a0), mload(0x2040), f_q))
            mstore(0x2080, addmod(mload(0x760), mload(0x2060), f_q))
            mstore(0x20a0, addmod(mload(0x2080), mload(0x3a0), f_q))
            mstore(
                0x20c0,
                mulmod(
                    11166246659983828508719468090013646171463329086121580628794302409516816350802,
                    mload(0x340),
                    f_q
                )
            )
            mstore(0x20e0, mulmod(mload(0x6a0), mload(0x20c0), f_q))
            mstore(0x2100, addmod(mload(0x7e0), mload(0x20e0), f_q))
            mstore(0x2120, addmod(mload(0x2100), mload(0x3a0), f_q))
            mstore(0x2140, mulmod(mload(0x2120), mload(0x20a0), f_q))
            mstore(0x2160, mulmod(mload(0x2140), mload(0xb20), f_q))
            mstore(0x2180, addmod(mload(0x2020), sub(f_q, mload(0x2160)), f_q))
            mstore(0x21a0, mulmod(mload(0x2180), mload(0x1c60), f_q))
            mstore(0x21c0, addmod(mload(0x1f20), mload(0x21a0), f_q))
            mstore(0x21e0, mulmod(mload(0x580), mload(0x21c0), f_q))
            mstore(0x2200, mulmod(mload(0xa60), mload(0x340), f_q))
            mstore(0x2220, addmod(mload(0x860), mload(0x2200), f_q))
            mstore(0x2240, addmod(mload(0x2220), mload(0x3a0), f_q))
            mstore(0x2260, mulmod(mload(0xa80), mload(0x340), f_q))
            mstore(0x2280, addmod(mload(0x8e0), mload(0x2260), f_q))
            mstore(0x22a0, addmod(mload(0x2280), mload(0x3a0), f_q))
            mstore(0x22c0, mulmod(mload(0x22a0), mload(0x2240), f_q))
            mstore(0x22e0, mulmod(mload(0x22c0), mload(0xba0), f_q))
            mstore(
                0x2300,
                mulmod(
                    284840088355319032285349970403338060113257071685626700086398481893096618818,
                    mload(0x340),
                    f_q
                )
            )
            mstore(0x2320, mulmod(mload(0x6a0), mload(0x2300), f_q))
            mstore(0x2340, addmod(mload(0x860), mload(0x2320), f_q))
            mstore(0x2360, addmod(mload(0x2340), mload(0x3a0), f_q))
            mstore(
                0x2380,
                mulmod(
                    21134065618345176623193549882539580312263652408302468683943992798037078993309,
                    mload(0x340),
                    f_q
                )
            )
            mstore(0x23a0, mulmod(mload(0x6a0), mload(0x2380), f_q))
            mstore(0x23c0, addmod(mload(0x8e0), mload(0x23a0), f_q))
            mstore(0x23e0, addmod(mload(0x23c0), mload(0x3a0), f_q))
            mstore(0x2400, mulmod(mload(0x23e0), mload(0x2360), f_q))
            mstore(0x2420, mulmod(mload(0x2400), mload(0xb80), f_q))
            mstore(0x2440, addmod(mload(0x22e0), sub(f_q, mload(0x2420)), f_q))
            mstore(0x2460, mulmod(mload(0x2440), mload(0x1c60), f_q))
            mstore(0x2480, addmod(mload(0x21e0), mload(0x2460), f_q))
            mstore(0x24a0, mulmod(mload(0x580), mload(0x2480), f_q))
            mstore(0x24c0, mulmod(mload(0xaa0), mload(0x340), f_q))
            mstore(0x24e0, addmod(mload(0x1600), mload(0x24c0), f_q))
            mstore(0x2500, addmod(mload(0x24e0), mload(0x3a0), f_q))
            mstore(0x2520, mulmod(mload(0x2500), mload(0xc00), f_q))
            mstore(
                0x2540,
                mulmod(
                    5625741653535312224677218588085279924365897425605943700675464992185016992283,
                    mload(0x340),
                    f_q
                )
            )
            mstore(0x2560, mulmod(mload(0x6a0), mload(0x2540), f_q))
            mstore(0x2580, addmod(mload(0x1600), mload(0x2560), f_q))
            mstore(0x25a0, addmod(mload(0x2580), mload(0x3a0), f_q))
            mstore(0x25c0, mulmod(mload(0x25a0), mload(0xbe0), f_q))
            mstore(0x25e0, addmod(mload(0x2520), sub(f_q, mload(0x25c0)), f_q))
            mstore(0x2600, mulmod(mload(0x25e0), mload(0x1c60), f_q))
            mstore(0x2620, addmod(mload(0x24a0), mload(0x2600), f_q))
            mstore(0x2640, mulmod(mload(0x580), mload(0x2620), f_q))
            mstore(0x2660, addmod(1, sub(f_q, mload(0xc20)), f_q))
            mstore(0x2680, mulmod(mload(0x2660), mload(0x15a0), f_q))
            mstore(0x26a0, addmod(mload(0x2640), mload(0x2680), f_q))
            mstore(0x26c0, mulmod(mload(0x580), mload(0x26a0), f_q))
            mstore(0x26e0, mulmod(mload(0xc20), mload(0xc20), f_q))
            mstore(0x2700, addmod(mload(0x26e0), sub(f_q, mload(0xc20)), f_q))
            mstore(0x2720, mulmod(mload(0x2700), mload(0x14c0), f_q))
            mstore(0x2740, addmod(mload(0x26c0), mload(0x2720), f_q))
            mstore(0x2760, mulmod(mload(0x580), mload(0x2740), f_q))
            mstore(0x2780, addmod(mload(0xc60), mload(0x340), f_q))
            mstore(0x27a0, mulmod(mload(0x2780), mload(0xc40), f_q))
            mstore(0x27c0, addmod(mload(0xca0), mload(0x3a0), f_q))
            mstore(0x27e0, mulmod(mload(0x27c0), mload(0x27a0), f_q))
            mstore(0x2800, addmod(mload(0x8e0), mload(0x340), f_q))
            mstore(0x2820, mulmod(mload(0x2800), mload(0xc20), f_q))
            mstore(0x2840, addmod(mload(0x920), mload(0x3a0), f_q))
            mstore(0x2860, mulmod(mload(0x2840), mload(0x2820), f_q))
            mstore(0x2880, addmod(mload(0x27e0), sub(f_q, mload(0x2860)), f_q))
            mstore(0x28a0, mulmod(mload(0x2880), mload(0x1c60), f_q))
            mstore(0x28c0, addmod(mload(0x2760), mload(0x28a0), f_q))
            mstore(0x28e0, mulmod(mload(0x580), mload(0x28c0), f_q))
            mstore(0x2900, addmod(mload(0xc60), sub(f_q, mload(0xca0)), f_q))
            mstore(0x2920, mulmod(mload(0x2900), mload(0x15a0), f_q))
            mstore(0x2940, addmod(mload(0x28e0), mload(0x2920), f_q))
            mstore(0x2960, mulmod(mload(0x580), mload(0x2940), f_q))
            mstore(0x2980, mulmod(mload(0x2900), mload(0x1c60), f_q))
            mstore(0x29a0, addmod(mload(0xc60), sub(f_q, mload(0xc80)), f_q))
            mstore(0x29c0, mulmod(mload(0x29a0), mload(0x2980), f_q))
            mstore(0x29e0, addmod(mload(0x2960), mload(0x29c0), f_q))
            mstore(0x2a00, mulmod(mload(0xfc0), mload(0xfc0), f_q))
            mstore(0x2a20, mulmod(mload(0x2a00), mload(0xfc0), f_q))
            mstore(0x2a40, mulmod(1, mload(0xfc0), f_q))
            mstore(0x2a60, mulmod(1, mload(0x2a00), f_q))
            mstore(0x2a80, mulmod(mload(0x29e0), mload(0xfe0), f_q))
            mstore(0x2aa0, mulmod(mload(0xe60), mload(0x6a0), f_q))
            mstore(0x2ac0, mulmod(mload(0x2aa0), mload(0x6a0), f_q))
            mstore(
                0x2ae0,
                mulmod(
                    mload(0x6a0),
                    13494463686150534520302493401520574237656912536907614359831638685825304135949,
                    f_q
                )
            )
            mstore(0x2b00, addmod(mload(0xde0), sub(f_q, mload(0x2ae0)), f_q))
            mstore(
                0x2b20,
                mulmod(
                    mload(0x6a0),
                    10679069158860809785885364198325818746230765378937472123583344754591056515264,
                    f_q
                )
            )
            mstore(0x2b40, addmod(mload(0xde0), sub(f_q, mload(0x2b20)), f_q))
            mstore(0x2b60, mulmod(mload(0x6a0), 1, f_q))
            mstore(0x2b80, addmod(mload(0xde0), sub(f_q, mload(0x2b60)), f_q))
            mstore(
                0x2ba0,
                mulmod(
                    mload(0x6a0),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            mstore(0x2bc0, addmod(mload(0xde0), sub(f_q, mload(0x2ba0)), f_q))
            mstore(
                0x2be0,
                mulmod(
                    mload(0x6a0),
                    9396103202274256930945606623206526900461945684265495839012435492634193195103,
                    f_q
                )
            )
            mstore(0x2c00, addmod(mload(0xde0), sub(f_q, mload(0x2be0)), f_q))
            mstore(
                0x2c20,
                mulmod(
                    mload(0x6a0),
                    7141049368301343714688735307754106798758533264534387178564790962720151052558,
                    f_q
                )
            )
            mstore(0x2c40, addmod(mload(0xde0), sub(f_q, mload(0x2c20)), f_q))
            mstore(
                0x2c60,
                mulmod(
                    14539938970100077722787609456885990080676645171153683143676448052646487506229,
                    mload(0x2aa0),
                    f_q
                )
            )
            mstore(0x2c80, mulmod(mload(0x2c60), 1, f_q))
            {
                let result := mulmod(mload(0xde0), mload(0x2c60), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2c80)), f_q), result, f_q)
                mstore(11424, result)
            }
            mstore(
                0x2cc0,
                mulmod(
                    7256530809051719749867553753986957859231619840094570731739941182706686413665,
                    mload(0x2aa0),
                    f_q
                )
            )
            mstore(
                0x2ce0,
                mulmod(
                    mload(0x2cc0),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0xde0), mload(0x2cc0), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2ce0)), f_q), result, f_q)
                mstore(11520, result)
            }
            mstore(
                0x2d20,
                mulmod(
                    12809867751727305620475423349830892726279648403976095368629033842110525582729,
                    mload(0x2aa0),
                    f_q
                )
            )
            mstore(
                0x2d40,
                mulmod(
                    mload(0x2d20),
                    9396103202274256930945606623206526900461945684265495839012435492634193195103,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0xde0), mload(0x2d20), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2d40)), f_q), result, f_q)
                mstore(11616, result)
            }
            mstore(
                0x2d80,
                mulmod(
                    5787000168993132838335045762627010468577538624811266895797571188740539775353,
                    mload(0x2aa0),
                    f_q
                )
            )
            mstore(
                0x2da0,
                mulmod(
                    mload(0x2d80),
                    7141049368301343714688735307754106798758533264534387178564790962720151052558,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0xde0), mload(0x2d80), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2da0)), f_q), result, f_q)
                mstore(11712, result)
            }
            mstore(0x2de0, mulmod(1, mload(0x2b80), f_q))
            mstore(0x2e00, mulmod(mload(0x2de0), mload(0x2bc0), f_q))
            mstore(0x2e20, mulmod(mload(0x2e00), mload(0x2c00), f_q))
            mstore(0x2e40, mulmod(mload(0x2e20), mload(0x2c40), f_q))
            {
                let result := mulmod(mload(0xde0), 1, f_q)
                result := addmod(
                    mulmod(
                        mload(0x6a0),
                        21888242871839275222246405745257275088548364400416034343698204186575808495616,
                        f_q
                    ),
                    result,
                    f_q
                )
                mstore(11872, result)
            }
            mstore(
                0x2e80,
                mulmod(
                    5266333647111022262519575308227530447403540681101773355208407176447894872116,
                    mload(0xe60),
                    f_q
                )
            )
            mstore(0x2ea0, mulmod(mload(0x2e80), 1, f_q))
            {
                let result := mulmod(mload(0xde0), mload(0x2e80), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2ea0)), f_q), result, f_q)
                mstore(11968, result)
            }
            mstore(
                0x2ee0,
                mulmod(
                    5045599748741669394807340163667268286359707073706640238348295071038051955298,
                    mload(0xe60),
                    f_q
                )
            )
            mstore(
                0x2f00,
                mulmod(
                    mload(0x2ee0),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0xde0), mload(0x2ee0), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2f00)), f_q), result, f_q)
                mstore(12064, result)
            }
            mstore(
                0x2f40,
                mulmod(
                    9024646962603862267003278632616888159297309104221063589461183531809948543756,
                    mload(0xe60),
                    f_q
                )
            )
            mstore(
                0x2f60,
                mulmod(
                    mload(0x2f40),
                    13494463686150534520302493401520574237656912536907614359831638685825304135949,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0xde0), mload(0x2f40), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2f60)), f_q), result, f_q)
                mstore(12160, result)
            }
            mstore(0x2fa0, mulmod(mload(0x2e00), mload(0x2b00), f_q))
            mstore(
                0x2fc0,
                mulmod(
                    457915096789217363190654424344135916650651035271458877272133377425876816156,
                    mload(0x6a0),
                    f_q
                )
            )
            mstore(0x2fe0, mulmod(mload(0x2fc0), 1, f_q))
            {
                let result := mulmod(mload(0xde0), mload(0x2fc0), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x2fe0)), f_q), result, f_q)
                mstore(12288, result)
            }
            mstore(
                0x3020,
                mulmod(
                    21430327775050057859055751320913139171897713365144575466426070809149931679461,
                    mload(0x6a0),
                    f_q
                )
            )
            mstore(
                0x3040,
                mulmod(
                    mload(0x3020),
                    21430327775050057859055751320913139171897713365144575466426070809149931679462,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0xde0), mload(0x3020), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x3040)), f_q), result, f_q)
                mstore(12384, result)
            }
            mstore(
                0x3080,
                mulmod(
                    11209173712978465436361041546931456342317599021478562220114859431984751980354,
                    mload(0x6a0),
                    f_q
                )
            )
            mstore(0x30a0, mulmod(mload(0x3080), 1, f_q))
            {
                let result := mulmod(mload(0xde0), mload(0x3080), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x30a0)), f_q), result, f_q)
                mstore(12480, result)
            }
            mstore(
                0x30e0,
                mulmod(
                    10679069158860809785885364198325818746230765378937472123583344754591056515263,
                    mload(0x6a0),
                    f_q
                )
            )
            mstore(
                0x3100,
                mulmod(
                    mload(0x30e0),
                    10679069158860809785885364198325818746230765378937472123583344754591056515264,
                    f_q
                )
            )
            {
                let result := mulmod(mload(0xde0), mload(0x30e0), f_q)
                result := addmod(mulmod(mload(0x6a0), sub(f_q, mload(0x3100)), f_q), result, f_q)
                mstore(12576, result)
            }
            mstore(0x3140, mulmod(mload(0x2de0), mload(0x2b40), f_q))
            {
                let prod := mload(0x2ca0)

                prod := mulmod(mload(0x2d00), prod, f_q)
                mstore(0x3160, prod)

                prod := mulmod(mload(0x2d60), prod, f_q)
                mstore(0x3180, prod)

                prod := mulmod(mload(0x2dc0), prod, f_q)
                mstore(0x31a0, prod)

                prod := mulmod(mload(0x2e60), prod, f_q)
                mstore(0x31c0, prod)

                prod := mulmod(mload(0x2de0), prod, f_q)
                mstore(0x31e0, prod)

                prod := mulmod(mload(0x2ec0), prod, f_q)
                mstore(0x3200, prod)

                prod := mulmod(mload(0x2f20), prod, f_q)
                mstore(0x3220, prod)

                prod := mulmod(mload(0x2f80), prod, f_q)
                mstore(0x3240, prod)

                prod := mulmod(mload(0x2fa0), prod, f_q)
                mstore(0x3260, prod)

                prod := mulmod(mload(0x3000), prod, f_q)
                mstore(0x3280, prod)

                prod := mulmod(mload(0x3060), prod, f_q)
                mstore(0x32a0, prod)

                prod := mulmod(mload(0x2e00), prod, f_q)
                mstore(0x32c0, prod)

                prod := mulmod(mload(0x30c0), prod, f_q)
                mstore(0x32e0, prod)

                prod := mulmod(mload(0x3120), prod, f_q)
                mstore(0x3300, prod)

                prod := mulmod(mload(0x3140), prod, f_q)
                mstore(0x3320, prod)
            }
            mstore(0x3360, 32)
            mstore(0x3380, 32)
            mstore(0x33a0, 32)
            mstore(0x33c0, mload(0x3320))
            mstore(
                0x33e0,
                21888242871839275222246405745257275088548364400416034343698204186575808495615
            )
            mstore(
                0x3400,
                21888242871839275222246405745257275088548364400416034343698204186575808495617
            )
            success := and(eq(staticcall(gas(), 0x5, 0x3360, 0xc0, 0x3340, 0x20), 1), success)
            {
                let inv := mload(0x3340)
                let v

                v := mload(0x3140)
                mstore(12608, mulmod(mload(0x3300), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x3120)
                mstore(12576, mulmod(mload(0x32e0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x30c0)
                mstore(12480, mulmod(mload(0x32c0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2e00)
                mstore(11776, mulmod(mload(0x32a0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x3060)
                mstore(12384, mulmod(mload(0x3280), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x3000)
                mstore(12288, mulmod(mload(0x3260), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2fa0)
                mstore(12192, mulmod(mload(0x3240), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2f80)
                mstore(12160, mulmod(mload(0x3220), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2f20)
                mstore(12064, mulmod(mload(0x3200), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2ec0)
                mstore(11968, mulmod(mload(0x31e0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2de0)
                mstore(11744, mulmod(mload(0x31c0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2e60)
                mstore(11872, mulmod(mload(0x31a0), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2dc0)
                mstore(11712, mulmod(mload(0x3180), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2d60)
                mstore(11616, mulmod(mload(0x3160), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x2d00)
                mstore(11520, mulmod(mload(0x2ca0), inv, f_q))
                inv := mulmod(v, inv, f_q)
                mstore(0x2ca0, inv)
            }
            {
                let result := mload(0x2ca0)
                result := addmod(mload(0x2d00), result, f_q)
                result := addmod(mload(0x2d60), result, f_q)
                result := addmod(mload(0x2dc0), result, f_q)
                mstore(13344, result)
            }
            mstore(0x3440, mulmod(mload(0x2e40), mload(0x2de0), f_q))
            {
                let result := mload(0x2e60)
                mstore(13408, result)
            }
            mstore(0x3480, mulmod(mload(0x2e40), mload(0x2fa0), f_q))
            {
                let result := mload(0x2ec0)
                result := addmod(mload(0x2f20), result, f_q)
                result := addmod(mload(0x2f80), result, f_q)
                mstore(13472, result)
            }
            mstore(0x34c0, mulmod(mload(0x2e40), mload(0x2e00), f_q))
            {
                let result := mload(0x3000)
                result := addmod(mload(0x3060), result, f_q)
                mstore(13536, result)
            }
            mstore(0x3500, mulmod(mload(0x2e40), mload(0x3140), f_q))
            {
                let result := mload(0x30c0)
                result := addmod(mload(0x3120), result, f_q)
                mstore(13600, result)
            }
            {
                let prod := mload(0x3420)

                prod := mulmod(mload(0x3460), prod, f_q)
                mstore(0x3540, prod)

                prod := mulmod(mload(0x34a0), prod, f_q)
                mstore(0x3560, prod)

                prod := mulmod(mload(0x34e0), prod, f_q)
                mstore(0x3580, prod)

                prod := mulmod(mload(0x3520), prod, f_q)
                mstore(0x35a0, prod)
            }
            mstore(0x35e0, 32)
            mstore(0x3600, 32)
            mstore(0x3620, 32)
            mstore(0x3640, mload(0x35a0))
            mstore(
                0x3660,
                21888242871839275222246405745257275088548364400416034343698204186575808495615
            )
            mstore(
                0x3680,
                21888242871839275222246405745257275088548364400416034343698204186575808495617
            )
            success := and(eq(staticcall(gas(), 0x5, 0x35e0, 0xc0, 0x35c0, 0x20), 1), success)
            {
                let inv := mload(0x35c0)
                let v

                v := mload(0x3520)
                mstore(13600, mulmod(mload(0x3580), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x34e0)
                mstore(13536, mulmod(mload(0x3560), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x34a0)
                mstore(13472, mulmod(mload(0x3540), inv, f_q))
                inv := mulmod(v, inv, f_q)

                v := mload(0x3460)
                mstore(13408, mulmod(mload(0x3420), inv, f_q))
                inv := mulmod(v, inv, f_q)
                mstore(0x3420, inv)
            }
            mstore(0x36a0, mulmod(mload(0x3440), mload(0x3460), f_q))
            mstore(0x36c0, mulmod(mload(0x3480), mload(0x34a0), f_q))
            mstore(0x36e0, mulmod(mload(0x34c0), mload(0x34e0), f_q))
            mstore(0x3700, mulmod(mload(0x3500), mload(0x3520), f_q))
            mstore(0x3720, mulmod(mload(0xce0), mload(0xce0), f_q))
            mstore(0x3740, mulmod(mload(0x3720), mload(0xce0), f_q))
            mstore(0x3760, mulmod(mload(0x3740), mload(0xce0), f_q))
            mstore(0x3780, mulmod(mload(0x3760), mload(0xce0), f_q))
            mstore(0x37a0, mulmod(mload(0x3780), mload(0xce0), f_q))
            mstore(0x37c0, mulmod(mload(0x37a0), mload(0xce0), f_q))
            mstore(0x37e0, mulmod(mload(0x37c0), mload(0xce0), f_q))
            mstore(0x3800, mulmod(mload(0x37e0), mload(0xce0), f_q))
            mstore(0x3820, mulmod(mload(0x3800), mload(0xce0), f_q))
            mstore(0x3840, mulmod(mload(0x3820), mload(0xce0), f_q))
            mstore(0x3860, mulmod(mload(0x3840), mload(0xce0), f_q))
            mstore(0x3880, mulmod(mload(0x3860), mload(0xce0), f_q))
            mstore(0x38a0, mulmod(mload(0x3880), mload(0xce0), f_q))
            mstore(0x38c0, mulmod(mload(0x38a0), mload(0xce0), f_q))
            mstore(0x38e0, mulmod(mload(0x38c0), mload(0xce0), f_q))
            mstore(0x3900, mulmod(mload(0x38e0), mload(0xce0), f_q))
            mstore(0x3920, mulmod(mload(0xd40), mload(0xd40), f_q))
            mstore(0x3940, mulmod(mload(0x3920), mload(0xd40), f_q))
            mstore(0x3960, mulmod(mload(0x3940), mload(0xd40), f_q))
            mstore(0x3980, mulmod(mload(0x3960), mload(0xd40), f_q))
            {
                let result := mulmod(mload(0x6e0), mload(0x2ca0), f_q)
                result := addmod(mulmod(mload(0x700), mload(0x2d00), f_q), result, f_q)
                result := addmod(mulmod(mload(0x720), mload(0x2d60), f_q), result, f_q)
                result := addmod(mulmod(mload(0x740), mload(0x2dc0), f_q), result, f_q)
                mstore(14752, result)
            }
            mstore(0x39c0, mulmod(mload(0x39a0), mload(0x3420), f_q))
            mstore(0x39e0, mulmod(sub(f_q, mload(0x39c0)), 1, f_q))
            {
                let result := mulmod(mload(0x760), mload(0x2ca0), f_q)
                result := addmod(mulmod(mload(0x780), mload(0x2d00), f_q), result, f_q)
                result := addmod(mulmod(mload(0x7a0), mload(0x2d60), f_q), result, f_q)
                result := addmod(mulmod(mload(0x7c0), mload(0x2dc0), f_q), result, f_q)
                mstore(14848, result)
            }
            mstore(0x3a20, mulmod(mload(0x3a00), mload(0x3420), f_q))
            mstore(0x3a40, mulmod(sub(f_q, mload(0x3a20)), mload(0xce0), f_q))
            mstore(0x3a60, mulmod(1, mload(0xce0), f_q))
            mstore(0x3a80, addmod(mload(0x39e0), mload(0x3a40), f_q))
            {
                let result := mulmod(mload(0x7e0), mload(0x2ca0), f_q)
                result := addmod(mulmod(mload(0x800), mload(0x2d00), f_q), result, f_q)
                result := addmod(mulmod(mload(0x820), mload(0x2d60), f_q), result, f_q)
                result := addmod(mulmod(mload(0x840), mload(0x2dc0), f_q), result, f_q)
                mstore(15008, result)
            }
            mstore(0x3ac0, mulmod(mload(0x3aa0), mload(0x3420), f_q))
            mstore(0x3ae0, mulmod(sub(f_q, mload(0x3ac0)), mload(0x3720), f_q))
            mstore(0x3b00, mulmod(1, mload(0x3720), f_q))
            mstore(0x3b20, addmod(mload(0x3a80), mload(0x3ae0), f_q))
            {
                let result := mulmod(mload(0x860), mload(0x2ca0), f_q)
                result := addmod(mulmod(mload(0x880), mload(0x2d00), f_q), result, f_q)
                result := addmod(mulmod(mload(0x8a0), mload(0x2d60), f_q), result, f_q)
                result := addmod(mulmod(mload(0x8c0), mload(0x2dc0), f_q), result, f_q)
                mstore(15168, result)
            }
            mstore(0x3b60, mulmod(mload(0x3b40), mload(0x3420), f_q))
            mstore(0x3b80, mulmod(sub(f_q, mload(0x3b60)), mload(0x3740), f_q))
            mstore(0x3ba0, mulmod(1, mload(0x3740), f_q))
            mstore(0x3bc0, addmod(mload(0x3b20), mload(0x3b80), f_q))
            mstore(0x3be0, mulmod(mload(0x3bc0), 1, f_q))
            mstore(0x3c00, mulmod(mload(0x3a60), 1, f_q))
            mstore(0x3c20, mulmod(mload(0x3b00), 1, f_q))
            mstore(0x3c40, mulmod(mload(0x3ba0), 1, f_q))
            mstore(0x3c60, mulmod(1, mload(0x3440), f_q))
            {
                let result := mulmod(mload(0x8e0), mload(0x2e60), f_q)
                mstore(15488, result)
            }
            mstore(0x3ca0, mulmod(mload(0x3c80), mload(0x36a0), f_q))
            mstore(0x3cc0, mulmod(sub(f_q, mload(0x3ca0)), 1, f_q))
            mstore(0x3ce0, mulmod(mload(0x3c60), 1, f_q))
            {
                let result := mulmod(mload(0xca0), mload(0x2e60), f_q)
                mstore(15616, result)
            }
            mstore(0x3d20, mulmod(mload(0x3d00), mload(0x36a0), f_q))
            mstore(0x3d40, mulmod(sub(f_q, mload(0x3d20)), mload(0xce0), f_q))
            mstore(0x3d60, mulmod(mload(0x3c60), mload(0xce0), f_q))
            mstore(0x3d80, addmod(mload(0x3cc0), mload(0x3d40), f_q))
            {
                let result := mulmod(mload(0x900), mload(0x2e60), f_q)
                mstore(15776, result)
            }
            mstore(0x3dc0, mulmod(mload(0x3da0), mload(0x36a0), f_q))
            mstore(0x3de0, mulmod(sub(f_q, mload(0x3dc0)), mload(0x3720), f_q))
            mstore(0x3e00, mulmod(mload(0x3c60), mload(0x3720), f_q))
            mstore(0x3e20, addmod(mload(0x3d80), mload(0x3de0), f_q))
            {
                let result := mulmod(mload(0x920), mload(0x2e60), f_q)
                mstore(15936, result)
            }
            mstore(0x3e60, mulmod(mload(0x3e40), mload(0x36a0), f_q))
            mstore(0x3e80, mulmod(sub(f_q, mload(0x3e60)), mload(0x3740), f_q))
            mstore(0x3ea0, mulmod(mload(0x3c60), mload(0x3740), f_q))
            mstore(0x3ec0, addmod(mload(0x3e20), mload(0x3e80), f_q))
            {
                let result := mulmod(mload(0x940), mload(0x2e60), f_q)
                mstore(16096, result)
            }
            mstore(0x3f00, mulmod(mload(0x3ee0), mload(0x36a0), f_q))
            mstore(0x3f20, mulmod(sub(f_q, mload(0x3f00)), mload(0x3760), f_q))
            mstore(0x3f40, mulmod(mload(0x3c60), mload(0x3760), f_q))
            mstore(0x3f60, addmod(mload(0x3ec0), mload(0x3f20), f_q))
            {
                let result := mulmod(mload(0x960), mload(0x2e60), f_q)
                mstore(16256, result)
            }
            mstore(0x3fa0, mulmod(mload(0x3f80), mload(0x36a0), f_q))
            mstore(0x3fc0, mulmod(sub(f_q, mload(0x3fa0)), mload(0x3780), f_q))
            mstore(0x3fe0, mulmod(mload(0x3c60), mload(0x3780), f_q))
            mstore(0x4000, addmod(mload(0x3f60), mload(0x3fc0), f_q))
            {
                let result := mulmod(mload(0x980), mload(0x2e60), f_q)
                mstore(16416, result)
            }
            mstore(0x4040, mulmod(mload(0x4020), mload(0x36a0), f_q))
            mstore(0x4060, mulmod(sub(f_q, mload(0x4040)), mload(0x37a0), f_q))
            mstore(0x4080, mulmod(mload(0x3c60), mload(0x37a0), f_q))
            mstore(0x40a0, addmod(mload(0x4000), mload(0x4060), f_q))
            {
                let result := mulmod(mload(0x9a0), mload(0x2e60), f_q)
                mstore(16576, result)
            }
            mstore(0x40e0, mulmod(mload(0x40c0), mload(0x36a0), f_q))
            mstore(0x4100, mulmod(sub(f_q, mload(0x40e0)), mload(0x37c0), f_q))
            mstore(0x4120, mulmod(mload(0x3c60), mload(0x37c0), f_q))
            mstore(0x4140, addmod(mload(0x40a0), mload(0x4100), f_q))
            {
                let result := mulmod(mload(0x9e0), mload(0x2e60), f_q)
                mstore(16736, result)
            }
            mstore(0x4180, mulmod(mload(0x4160), mload(0x36a0), f_q))
            mstore(0x41a0, mulmod(sub(f_q, mload(0x4180)), mload(0x37e0), f_q))
            mstore(0x41c0, mulmod(mload(0x3c60), mload(0x37e0), f_q))
            mstore(0x41e0, addmod(mload(0x4140), mload(0x41a0), f_q))
            {
                let result := mulmod(mload(0xa00), mload(0x2e60), f_q)
                mstore(16896, result)
            }
            mstore(0x4220, mulmod(mload(0x4200), mload(0x36a0), f_q))
            mstore(0x4240, mulmod(sub(f_q, mload(0x4220)), mload(0x3800), f_q))
            mstore(0x4260, mulmod(mload(0x3c60), mload(0x3800), f_q))
            mstore(0x4280, addmod(mload(0x41e0), mload(0x4240), f_q))
            {
                let result := mulmod(mload(0xa20), mload(0x2e60), f_q)
                mstore(17056, result)
            }
            mstore(0x42c0, mulmod(mload(0x42a0), mload(0x36a0), f_q))
            mstore(0x42e0, mulmod(sub(f_q, mload(0x42c0)), mload(0x3820), f_q))
            mstore(0x4300, mulmod(mload(0x3c60), mload(0x3820), f_q))
            mstore(0x4320, addmod(mload(0x4280), mload(0x42e0), f_q))
            {
                let result := mulmod(mload(0xa40), mload(0x2e60), f_q)
                mstore(17216, result)
            }
            mstore(0x4360, mulmod(mload(0x4340), mload(0x36a0), f_q))
            mstore(0x4380, mulmod(sub(f_q, mload(0x4360)), mload(0x3840), f_q))
            mstore(0x43a0, mulmod(mload(0x3c60), mload(0x3840), f_q))
            mstore(0x43c0, addmod(mload(0x4320), mload(0x4380), f_q))
            {
                let result := mulmod(mload(0xa60), mload(0x2e60), f_q)
                mstore(17376, result)
            }
            mstore(0x4400, mulmod(mload(0x43e0), mload(0x36a0), f_q))
            mstore(0x4420, mulmod(sub(f_q, mload(0x4400)), mload(0x3860), f_q))
            mstore(0x4440, mulmod(mload(0x3c60), mload(0x3860), f_q))
            mstore(0x4460, addmod(mload(0x43c0), mload(0x4420), f_q))
            {
                let result := mulmod(mload(0xa80), mload(0x2e60), f_q)
                mstore(17536, result)
            }
            mstore(0x44a0, mulmod(mload(0x4480), mload(0x36a0), f_q))
            mstore(0x44c0, mulmod(sub(f_q, mload(0x44a0)), mload(0x3880), f_q))
            mstore(0x44e0, mulmod(mload(0x3c60), mload(0x3880), f_q))
            mstore(0x4500, addmod(mload(0x4460), mload(0x44c0), f_q))
            {
                let result := mulmod(mload(0xaa0), mload(0x2e60), f_q)
                mstore(17696, result)
            }
            mstore(0x4540, mulmod(mload(0x4520), mload(0x36a0), f_q))
            mstore(0x4560, mulmod(sub(f_q, mload(0x4540)), mload(0x38a0), f_q))
            mstore(0x4580, mulmod(mload(0x3c60), mload(0x38a0), f_q))
            mstore(0x45a0, addmod(mload(0x4500), mload(0x4560), f_q))
            mstore(0x45c0, mulmod(mload(0x2a40), mload(0x3440), f_q))
            mstore(0x45e0, mulmod(mload(0x2a60), mload(0x3440), f_q))
            {
                let result := mulmod(mload(0x2a80), mload(0x2e60), f_q)
                mstore(17920, result)
            }
            mstore(0x4620, mulmod(mload(0x4600), mload(0x36a0), f_q))
            mstore(0x4640, mulmod(sub(f_q, mload(0x4620)), mload(0x38c0), f_q))
            mstore(0x4660, mulmod(mload(0x3c60), mload(0x38c0), f_q))
            mstore(0x4680, mulmod(mload(0x45c0), mload(0x38c0), f_q))
            mstore(0x46a0, mulmod(mload(0x45e0), mload(0x38c0), f_q))
            mstore(0x46c0, addmod(mload(0x45a0), mload(0x4640), f_q))
            {
                let result := mulmod(mload(0x9c0), mload(0x2e60), f_q)
                mstore(18144, result)
            }
            mstore(0x4700, mulmod(mload(0x46e0), mload(0x36a0), f_q))
            mstore(0x4720, mulmod(sub(f_q, mload(0x4700)), mload(0x38e0), f_q))
            mstore(0x4740, mulmod(mload(0x3c60), mload(0x38e0), f_q))
            mstore(0x4760, addmod(mload(0x46c0), mload(0x4720), f_q))
            mstore(0x4780, mulmod(mload(0x4760), mload(0xd40), f_q))
            mstore(0x47a0, mulmod(mload(0x3ce0), mload(0xd40), f_q))
            mstore(0x47c0, mulmod(mload(0x3d60), mload(0xd40), f_q))
            mstore(0x47e0, mulmod(mload(0x3e00), mload(0xd40), f_q))
            mstore(0x4800, mulmod(mload(0x3ea0), mload(0xd40), f_q))
            mstore(0x4820, mulmod(mload(0x3f40), mload(0xd40), f_q))
            mstore(0x4840, mulmod(mload(0x3fe0), mload(0xd40), f_q))
            mstore(0x4860, mulmod(mload(0x4080), mload(0xd40), f_q))
            mstore(0x4880, mulmod(mload(0x4120), mload(0xd40), f_q))
            mstore(0x48a0, mulmod(mload(0x41c0), mload(0xd40), f_q))
            mstore(0x48c0, mulmod(mload(0x4260), mload(0xd40), f_q))
            mstore(0x48e0, mulmod(mload(0x4300), mload(0xd40), f_q))
            mstore(0x4900, mulmod(mload(0x43a0), mload(0xd40), f_q))
            mstore(0x4920, mulmod(mload(0x4440), mload(0xd40), f_q))
            mstore(0x4940, mulmod(mload(0x44e0), mload(0xd40), f_q))
            mstore(0x4960, mulmod(mload(0x4580), mload(0xd40), f_q))
            mstore(0x4980, mulmod(mload(0x4660), mload(0xd40), f_q))
            mstore(0x49a0, mulmod(mload(0x4680), mload(0xd40), f_q))
            mstore(0x49c0, mulmod(mload(0x46a0), mload(0xd40), f_q))
            mstore(0x49e0, mulmod(mload(0x4740), mload(0xd40), f_q))
            mstore(0x4a00, addmod(mload(0x3be0), mload(0x4780), f_q))
            mstore(0x4a20, mulmod(1, mload(0x3480), f_q))
            {
                let result := mulmod(mload(0xac0), mload(0x2ec0), f_q)
                result := addmod(mulmod(mload(0xae0), mload(0x2f20), f_q), result, f_q)
                result := addmod(mulmod(mload(0xb00), mload(0x2f80), f_q), result, f_q)
                mstore(19008, result)
            }
            mstore(0x4a60, mulmod(mload(0x4a40), mload(0x36c0), f_q))
            mstore(0x4a80, mulmod(sub(f_q, mload(0x4a60)), 1, f_q))
            mstore(0x4aa0, mulmod(mload(0x4a20), 1, f_q))
            {
                let result := mulmod(mload(0xb20), mload(0x2ec0), f_q)
                result := addmod(mulmod(mload(0xb40), mload(0x2f20), f_q), result, f_q)
                result := addmod(mulmod(mload(0xb60), mload(0x2f80), f_q), result, f_q)
                mstore(19136, result)
            }
            mstore(0x4ae0, mulmod(mload(0x4ac0), mload(0x36c0), f_q))
            mstore(0x4b00, mulmod(sub(f_q, mload(0x4ae0)), mload(0xce0), f_q))
            mstore(0x4b20, mulmod(mload(0x4a20), mload(0xce0), f_q))
            mstore(0x4b40, addmod(mload(0x4a80), mload(0x4b00), f_q))
            {
                let result := mulmod(mload(0xb80), mload(0x2ec0), f_q)
                result := addmod(mulmod(mload(0xba0), mload(0x2f20), f_q), result, f_q)
                result := addmod(mulmod(mload(0xbc0), mload(0x2f80), f_q), result, f_q)
                mstore(19296, result)
            }
            mstore(0x4b80, mulmod(mload(0x4b60), mload(0x36c0), f_q))
            mstore(0x4ba0, mulmod(sub(f_q, mload(0x4b80)), mload(0x3720), f_q))
            mstore(0x4bc0, mulmod(mload(0x4a20), mload(0x3720), f_q))
            mstore(0x4be0, addmod(mload(0x4b40), mload(0x4ba0), f_q))
            mstore(0x4c00, mulmod(mload(0x4be0), mload(0x3920), f_q))
            mstore(0x4c20, mulmod(mload(0x4aa0), mload(0x3920), f_q))
            mstore(0x4c40, mulmod(mload(0x4b20), mload(0x3920), f_q))
            mstore(0x4c60, mulmod(mload(0x4bc0), mload(0x3920), f_q))
            mstore(0x4c80, addmod(mload(0x4a00), mload(0x4c00), f_q))
            mstore(0x4ca0, mulmod(1, mload(0x34c0), f_q))
            {
                let result := mulmod(mload(0xbe0), mload(0x3000), f_q)
                result := addmod(mulmod(mload(0xc00), mload(0x3060), f_q), result, f_q)
                mstore(19648, result)
            }
            mstore(0x4ce0, mulmod(mload(0x4cc0), mload(0x36e0), f_q))
            mstore(0x4d00, mulmod(sub(f_q, mload(0x4ce0)), 1, f_q))
            mstore(0x4d20, mulmod(mload(0x4ca0), 1, f_q))
            {
                let result := mulmod(mload(0xc20), mload(0x3000), f_q)
                result := addmod(mulmod(mload(0xc40), mload(0x3060), f_q), result, f_q)
                mstore(19776, result)
            }
            mstore(0x4d60, mulmod(mload(0x4d40), mload(0x36e0), f_q))
            mstore(0x4d80, mulmod(sub(f_q, mload(0x4d60)), mload(0xce0), f_q))
            mstore(0x4da0, mulmod(mload(0x4ca0), mload(0xce0), f_q))
            mstore(0x4dc0, addmod(mload(0x4d00), mload(0x4d80), f_q))
            mstore(0x4de0, mulmod(mload(0x4dc0), mload(0x3940), f_q))
            mstore(0x4e00, mulmod(mload(0x4d20), mload(0x3940), f_q))
            mstore(0x4e20, mulmod(mload(0x4da0), mload(0x3940), f_q))
            mstore(0x4e40, addmod(mload(0x4c80), mload(0x4de0), f_q))
            mstore(0x4e60, mulmod(1, mload(0x3500), f_q))
            {
                let result := mulmod(mload(0xc60), mload(0x30c0), f_q)
                result := addmod(mulmod(mload(0xc80), mload(0x3120), f_q), result, f_q)
                mstore(20096, result)
            }
            mstore(0x4ea0, mulmod(mload(0x4e80), mload(0x3700), f_q))
            mstore(0x4ec0, mulmod(sub(f_q, mload(0x4ea0)), 1, f_q))
            mstore(0x4ee0, mulmod(mload(0x4e60), 1, f_q))
            mstore(0x4f00, mulmod(mload(0x4ec0), mload(0x3960), f_q))
            mstore(0x4f20, mulmod(mload(0x4ee0), mload(0x3960), f_q))
            mstore(0x4f40, addmod(mload(0x4e40), mload(0x4f00), f_q))
            mstore(0x4f60, mulmod(1, mload(0x2e40), f_q))
            mstore(0x4f80, mulmod(1, mload(0xde0), f_q))
            mstore(0x4fa0, 0x0000000000000000000000000000000000000000000000000000000000000001)
            mstore(0x4fc0, 0x0000000000000000000000000000000000000000000000000000000000000002)
            mstore(0x4fe0, mload(0x4f40))
            success := and(eq(staticcall(gas(), 0x7, 0x4fa0, 0x60, 0x4fa0, 0x40), 1), success)
            mstore(0x5000, mload(0x4fa0))
            mstore(0x5020, mload(0x4fc0))
            mstore(0x5040, mload(0x100))
            mstore(0x5060, mload(0x120))
            success := and(eq(staticcall(gas(), 0x6, 0x5000, 0x80, 0x5000, 0x40), 1), success)
            mstore(0x5080, mload(0x140))
            mstore(0x50a0, mload(0x160))
            mstore(0x50c0, mload(0x3c00))
            success := and(eq(staticcall(gas(), 0x7, 0x5080, 0x60, 0x5080, 0x40), 1), success)
            mstore(0x50e0, mload(0x5000))
            mstore(0x5100, mload(0x5020))
            mstore(0x5120, mload(0x5080))
            mstore(0x5140, mload(0x50a0))
            success := and(eq(staticcall(gas(), 0x6, 0x50e0, 0x80, 0x50e0, 0x40), 1), success)
            mstore(0x5160, mload(0x180))
            mstore(0x5180, mload(0x1a0))
            mstore(0x51a0, mload(0x3c20))
            success := and(eq(staticcall(gas(), 0x7, 0x5160, 0x60, 0x5160, 0x40), 1), success)
            mstore(0x51c0, mload(0x50e0))
            mstore(0x51e0, mload(0x5100))
            mstore(0x5200, mload(0x5160))
            mstore(0x5220, mload(0x5180))
            success := and(eq(staticcall(gas(), 0x6, 0x51c0, 0x80, 0x51c0, 0x40), 1), success)
            mstore(0x5240, mload(0x1c0))
            mstore(0x5260, mload(0x1e0))
            mstore(0x5280, mload(0x3c40))
            success := and(eq(staticcall(gas(), 0x7, 0x5240, 0x60, 0x5240, 0x40), 1), success)
            mstore(0x52a0, mload(0x51c0))
            mstore(0x52c0, mload(0x51e0))
            mstore(0x52e0, mload(0x5240))
            mstore(0x5300, mload(0x5260))
            success := and(eq(staticcall(gas(), 0x6, 0x52a0, 0x80, 0x52a0, 0x40), 1), success)
            mstore(0x5320, mload(0x200))
            mstore(0x5340, mload(0x220))
            mstore(0x5360, mload(0x47a0))
            success := and(eq(staticcall(gas(), 0x7, 0x5320, 0x60, 0x5320, 0x40), 1), success)
            mstore(0x5380, mload(0x52a0))
            mstore(0x53a0, mload(0x52c0))
            mstore(0x53c0, mload(0x5320))
            mstore(0x53e0, mload(0x5340))
            success := and(eq(staticcall(gas(), 0x6, 0x5380, 0x80, 0x5380, 0x40), 1), success)
            mstore(0x5400, mload(0x2e0))
            mstore(0x5420, mload(0x300))
            mstore(0x5440, mload(0x47c0))
            success := and(eq(staticcall(gas(), 0x7, 0x5400, 0x60, 0x5400, 0x40), 1), success)
            mstore(0x5460, mload(0x5380))
            mstore(0x5480, mload(0x53a0))
            mstore(0x54a0, mload(0x5400))
            mstore(0x54c0, mload(0x5420))
            success := and(eq(staticcall(gas(), 0x6, 0x5460, 0x80, 0x5460, 0x40), 1), success)
            mstore(0x54e0, 0x14305c1f17543b2e591905c1c33395a0f51a2ba8dfc136821b6c40e763066caf)
            mstore(0x5500, 0x07e3a08d0a0617f2eb9ba52253e006cbb27c6b99abb9129e02fd174e98eb19a1)
            mstore(0x5520, mload(0x47e0))
            success := and(eq(staticcall(gas(), 0x7, 0x54e0, 0x60, 0x54e0, 0x40), 1), success)
            mstore(0x5540, mload(0x5460))
            mstore(0x5560, mload(0x5480))
            mstore(0x5580, mload(0x54e0))
            mstore(0x55a0, mload(0x5500))
            success := and(eq(staticcall(gas(), 0x6, 0x5540, 0x80, 0x5540, 0x40), 1), success)
            mstore(0x55c0, 0x2f212be54542ade2116bc81277ee498cc5a02609e3b3bd0432ea6e66b295c3ed)
            mstore(0x55e0, 0x07619d7ddbf1bd3f7bb8ca42063d0cf35ed2052a83bd8f84a651950603d76c54)
            mstore(0x5600, mload(0x4800))
            success := and(eq(staticcall(gas(), 0x7, 0x55c0, 0x60, 0x55c0, 0x40), 1), success)
            mstore(0x5620, mload(0x5540))
            mstore(0x5640, mload(0x5560))
            mstore(0x5660, mload(0x55c0))
            mstore(0x5680, mload(0x55e0))
            success := and(eq(staticcall(gas(), 0x6, 0x5620, 0x80, 0x5620, 0x40), 1), success)
            mstore(0x56a0, 0x15c144dc950d67cc347b92f32474804dd084663afbfc595047ce3381a5bc5b3b)
            mstore(0x56c0, 0x0ad59cae8303b307adb8cbb69ec9e1296ceaea34e38bc4e5fcdc2daf1557698a)
            mstore(0x56e0, mload(0x4820))
            success := and(eq(staticcall(gas(), 0x7, 0x56a0, 0x60, 0x56a0, 0x40), 1), success)
            mstore(0x5700, mload(0x5620))
            mstore(0x5720, mload(0x5640))
            mstore(0x5740, mload(0x56a0))
            mstore(0x5760, mload(0x56c0))
            success := and(eq(staticcall(gas(), 0x6, 0x5700, 0x80, 0x5700, 0x40), 1), success)
            mstore(0x5780, 0x0134ac2bce0cbb0c805ed402cc0dee2268e5442a5394a7fd15fa97c009191356)
            mstore(0x57a0, 0x0105bddd2b032c702973700ce7e9c39f9c74bdbf08a55777c875c8465fe50abd)
            mstore(0x57c0, mload(0x4840))
            success := and(eq(staticcall(gas(), 0x7, 0x5780, 0x60, 0x5780, 0x40), 1), success)
            mstore(0x57e0, mload(0x5700))
            mstore(0x5800, mload(0x5720))
            mstore(0x5820, mload(0x5780))
            mstore(0x5840, mload(0x57a0))
            success := and(eq(staticcall(gas(), 0x6, 0x57e0, 0x80, 0x57e0, 0x40), 1), success)
            mstore(0x5860, 0x0f61562354a102c482c5d920cd4b72abb0f8e625d9b49fe8b6fe0c8ef406bba1)
            mstore(0x5880, 0x1e272b5c6007594e473b2e96078e77467020aa1d80224602745ccc35f7ae1127)
            mstore(0x58a0, mload(0x4860))
            success := and(eq(staticcall(gas(), 0x7, 0x5860, 0x60, 0x5860, 0x40), 1), success)
            mstore(0x58c0, mload(0x57e0))
            mstore(0x58e0, mload(0x5800))
            mstore(0x5900, mload(0x5860))
            mstore(0x5920, mload(0x5880))
            success := and(eq(staticcall(gas(), 0x6, 0x58c0, 0x80, 0x58c0, 0x40), 1), success)
            mstore(0x5940, 0x13a664874475c6f3d3266d8306e431df907401375fc584e0500d5327775fa689)
            mstore(0x5960, 0x2895d002847b0f4deebb6953d367fff13a91de0623515db3723c16cfd618204f)
            mstore(0x5980, mload(0x4880))
            success := and(eq(staticcall(gas(), 0x7, 0x5940, 0x60, 0x5940, 0x40), 1), success)
            mstore(0x59a0, mload(0x58c0))
            mstore(0x59c0, mload(0x58e0))
            mstore(0x59e0, mload(0x5940))
            mstore(0x5a00, mload(0x5960))
            success := and(eq(staticcall(gas(), 0x6, 0x59a0, 0x80, 0x59a0, 0x40), 1), success)
            mstore(0x5a20, 0x2c8d5c87fdb5726372ce4c05379d41c4f5885b5bb77a53f7e5849f8ef3b57194)
            mstore(0x5a40, 0x0714289dc8ec997cc27db0147a68f9352923ec45c1f00dcaf381e78092bb1199)
            mstore(0x5a60, mload(0x48a0))
            success := and(eq(staticcall(gas(), 0x7, 0x5a20, 0x60, 0x5a20, 0x40), 1), success)
            mstore(0x5a80, mload(0x59a0))
            mstore(0x5aa0, mload(0x59c0))
            mstore(0x5ac0, mload(0x5a20))
            mstore(0x5ae0, mload(0x5a40))
            success := and(eq(staticcall(gas(), 0x6, 0x5a80, 0x80, 0x5a80, 0x40), 1), success)
            mstore(0x5b00, 0x1f11cc901857ead2710a87e30155b3b79d3af204e8581d7882ed93e5d8f1605b)
            mstore(0x5b20, 0x2758c739fbb4fe7db87bb5b5d7dadaee71b42a0c43b7664b6dd4f22483a06a6d)
            mstore(0x5b40, mload(0x48c0))
            success := and(eq(staticcall(gas(), 0x7, 0x5b00, 0x60, 0x5b00, 0x40), 1), success)
            mstore(0x5b60, mload(0x5a80))
            mstore(0x5b80, mload(0x5aa0))
            mstore(0x5ba0, mload(0x5b00))
            mstore(0x5bc0, mload(0x5b20))
            success := and(eq(staticcall(gas(), 0x6, 0x5b60, 0x80, 0x5b60, 0x40), 1), success)
            mstore(0x5be0, 0x0a07d0f1d29b879f1bd1a7737b9391c612ee13db2a81570e2bd550bfaee85763)
            mstore(0x5c00, 0x16d510887019a75dd35b21bf873db31ae78b1bb619bd51b1050cc6b52f2d2bf8)
            mstore(0x5c20, mload(0x48e0))
            success := and(eq(staticcall(gas(), 0x7, 0x5be0, 0x60, 0x5be0, 0x40), 1), success)
            mstore(0x5c40, mload(0x5b60))
            mstore(0x5c60, mload(0x5b80))
            mstore(0x5c80, mload(0x5be0))
            mstore(0x5ca0, mload(0x5c00))
            success := and(eq(staticcall(gas(), 0x6, 0x5c40, 0x80, 0x5c40, 0x40), 1), success)
            mstore(0x5cc0, 0x0a2218b9e13906190f464cd94771fa8b2ee1e53956108bb36a7ea4ba2cef3c82)
            mstore(0x5ce0, 0x1dfa82bea9f56c6ce00114c1e0d2585b3c67548f9a2799cae72e84d468a4c155)
            mstore(0x5d00, mload(0x4900))
            success := and(eq(staticcall(gas(), 0x7, 0x5cc0, 0x60, 0x5cc0, 0x40), 1), success)
            mstore(0x5d20, mload(0x5c40))
            mstore(0x5d40, mload(0x5c60))
            mstore(0x5d60, mload(0x5cc0))
            mstore(0x5d80, mload(0x5ce0))
            success := and(eq(staticcall(gas(), 0x6, 0x5d20, 0x80, 0x5d20, 0x40), 1), success)
            mstore(0x5da0, 0x1c11e3afc107d46850a4a29d5cfe1d1790e4054869426c7240aea5acf3c5aa19)
            mstore(0x5dc0, 0x274b2ca3fde8c544c5581c538a97d07a90428dbf1c3dace0a4c5540eba2f99b1)
            mstore(0x5de0, mload(0x4920))
            success := and(eq(staticcall(gas(), 0x7, 0x5da0, 0x60, 0x5da0, 0x40), 1), success)
            mstore(0x5e00, mload(0x5d20))
            mstore(0x5e20, mload(0x5d40))
            mstore(0x5e40, mload(0x5da0))
            mstore(0x5e60, mload(0x5dc0))
            success := and(eq(staticcall(gas(), 0x6, 0x5e00, 0x80, 0x5e00, 0x40), 1), success)
            mstore(0x5e80, 0x2a3b8e85d19345dad5c267fe1644bb2dbccd123c20b0e77f6e77605adc49825a)
            mstore(0x5ea0, 0x2844f0a9a0c69451ac6d39d06cefd3a9d70603707e06172d3a573a51a0d43394)
            mstore(0x5ec0, mload(0x4940))
            success := and(eq(staticcall(gas(), 0x7, 0x5e80, 0x60, 0x5e80, 0x40), 1), success)
            mstore(0x5ee0, mload(0x5e00))
            mstore(0x5f00, mload(0x5e20))
            mstore(0x5f20, mload(0x5e80))
            mstore(0x5f40, mload(0x5ea0))
            success := and(eq(staticcall(gas(), 0x6, 0x5ee0, 0x80, 0x5ee0, 0x40), 1), success)
            mstore(0x5f60, 0x0dcb00bb6eb127dba9de5687b699336a994a2bbab76bc3a4a0638a94376fea0e)
            mstore(0x5f80, 0x1dfbddf7dcf2501f81e6460600fe1b5072a863467f90e2758689bf3d03afd9ca)
            mstore(0x5fa0, mload(0x4960))
            success := and(eq(staticcall(gas(), 0x7, 0x5f60, 0x60, 0x5f60, 0x40), 1), success)
            mstore(0x5fc0, mload(0x5ee0))
            mstore(0x5fe0, mload(0x5f00))
            mstore(0x6000, mload(0x5f60))
            mstore(0x6020, mload(0x5f80))
            success := and(eq(staticcall(gas(), 0x6, 0x5fc0, 0x80, 0x5fc0, 0x40), 1), success)
            mstore(0x6040, mload(0x5c0))
            mstore(0x6060, mload(0x5e0))
            mstore(0x6080, mload(0x4980))
            success := and(eq(staticcall(gas(), 0x7, 0x6040, 0x60, 0x6040, 0x40), 1), success)
            mstore(0x60a0, mload(0x5fc0))
            mstore(0x60c0, mload(0x5fe0))
            mstore(0x60e0, mload(0x6040))
            mstore(0x6100, mload(0x6060))
            success := and(eq(staticcall(gas(), 0x6, 0x60a0, 0x80, 0x60a0, 0x40), 1), success)
            mstore(0x6120, mload(0x600))
            mstore(0x6140, mload(0x620))
            mstore(0x6160, mload(0x49a0))
            success := and(eq(staticcall(gas(), 0x7, 0x6120, 0x60, 0x6120, 0x40), 1), success)
            mstore(0x6180, mload(0x60a0))
            mstore(0x61a0, mload(0x60c0))
            mstore(0x61c0, mload(0x6120))
            mstore(0x61e0, mload(0x6140))
            success := and(eq(staticcall(gas(), 0x6, 0x6180, 0x80, 0x6180, 0x40), 1), success)
            mstore(0x6200, mload(0x640))
            mstore(0x6220, mload(0x660))
            mstore(0x6240, mload(0x49c0))
            success := and(eq(staticcall(gas(), 0x7, 0x6200, 0x60, 0x6200, 0x40), 1), success)
            mstore(0x6260, mload(0x6180))
            mstore(0x6280, mload(0x61a0))
            mstore(0x62a0, mload(0x6200))
            mstore(0x62c0, mload(0x6220))
            success := and(eq(staticcall(gas(), 0x6, 0x6260, 0x80, 0x6260, 0x40), 1), success)
            mstore(0x62e0, mload(0x520))
            mstore(0x6300, mload(0x540))
            mstore(0x6320, mload(0x49e0))
            success := and(eq(staticcall(gas(), 0x7, 0x62e0, 0x60, 0x62e0, 0x40), 1), success)
            mstore(0x6340, mload(0x6260))
            mstore(0x6360, mload(0x6280))
            mstore(0x6380, mload(0x62e0))
            mstore(0x63a0, mload(0x6300))
            success := and(eq(staticcall(gas(), 0x6, 0x6340, 0x80, 0x6340, 0x40), 1), success)
            mstore(0x63c0, mload(0x3e0))
            mstore(0x63e0, mload(0x400))
            mstore(0x6400, mload(0x4c20))
            success := and(eq(staticcall(gas(), 0x7, 0x63c0, 0x60, 0x63c0, 0x40), 1), success)
            mstore(0x6420, mload(0x6340))
            mstore(0x6440, mload(0x6360))
            mstore(0x6460, mload(0x63c0))
            mstore(0x6480, mload(0x63e0))
            success := and(eq(staticcall(gas(), 0x6, 0x6420, 0x80, 0x6420, 0x40), 1), success)
            mstore(0x64a0, mload(0x420))
            mstore(0x64c0, mload(0x440))
            mstore(0x64e0, mload(0x4c40))
            success := and(eq(staticcall(gas(), 0x7, 0x64a0, 0x60, 0x64a0, 0x40), 1), success)
            mstore(0x6500, mload(0x6420))
            mstore(0x6520, mload(0x6440))
            mstore(0x6540, mload(0x64a0))
            mstore(0x6560, mload(0x64c0))
            success := and(eq(staticcall(gas(), 0x6, 0x6500, 0x80, 0x6500, 0x40), 1), success)
            mstore(0x6580, mload(0x460))
            mstore(0x65a0, mload(0x480))
            mstore(0x65c0, mload(0x4c60))
            success := and(eq(staticcall(gas(), 0x7, 0x6580, 0x60, 0x6580, 0x40), 1), success)
            mstore(0x65e0, mload(0x6500))
            mstore(0x6600, mload(0x6520))
            mstore(0x6620, mload(0x6580))
            mstore(0x6640, mload(0x65a0))
            success := and(eq(staticcall(gas(), 0x6, 0x65e0, 0x80, 0x65e0, 0x40), 1), success)
            mstore(0x6660, mload(0x4a0))
            mstore(0x6680, mload(0x4c0))
            mstore(0x66a0, mload(0x4e00))
            success := and(eq(staticcall(gas(), 0x7, 0x6660, 0x60, 0x6660, 0x40), 1), success)
            mstore(0x66c0, mload(0x65e0))
            mstore(0x66e0, mload(0x6600))
            mstore(0x6700, mload(0x6660))
            mstore(0x6720, mload(0x6680))
            success := and(eq(staticcall(gas(), 0x6, 0x66c0, 0x80, 0x66c0, 0x40), 1), success)
            mstore(0x6740, mload(0x4e0))
            mstore(0x6760, mload(0x500))
            mstore(0x6780, mload(0x4e20))
            success := and(eq(staticcall(gas(), 0x7, 0x6740, 0x60, 0x6740, 0x40), 1), success)
            mstore(0x67a0, mload(0x66c0))
            mstore(0x67c0, mload(0x66e0))
            mstore(0x67e0, mload(0x6740))
            mstore(0x6800, mload(0x6760))
            success := and(eq(staticcall(gas(), 0x6, 0x67a0, 0x80, 0x67a0, 0x40), 1), success)
            mstore(0x6820, mload(0x2a0))
            mstore(0x6840, mload(0x2c0))
            mstore(0x6860, mload(0x4f20))
            success := and(eq(staticcall(gas(), 0x7, 0x6820, 0x60, 0x6820, 0x40), 1), success)
            mstore(0x6880, mload(0x67a0))
            mstore(0x68a0, mload(0x67c0))
            mstore(0x68c0, mload(0x6820))
            mstore(0x68e0, mload(0x6840))
            success := and(eq(staticcall(gas(), 0x6, 0x6880, 0x80, 0x6880, 0x40), 1), success)
            mstore(0x6900, mload(0xd80))
            mstore(0x6920, mload(0xda0))
            mstore(0x6940, sub(f_q, mload(0x4f60)))
            success := and(eq(staticcall(gas(), 0x7, 0x6900, 0x60, 0x6900, 0x40), 1), success)
            mstore(0x6960, mload(0x6880))
            mstore(0x6980, mload(0x68a0))
            mstore(0x69a0, mload(0x6900))
            mstore(0x69c0, mload(0x6920))
            success := and(eq(staticcall(gas(), 0x6, 0x6960, 0x80, 0x6960, 0x40), 1), success)
            mstore(0x69e0, mload(0xe20))
            mstore(0x6a00, mload(0xe40))
            mstore(0x6a20, mload(0x4f80))
            success := and(eq(staticcall(gas(), 0x7, 0x69e0, 0x60, 0x69e0, 0x40), 1), success)
            mstore(0x6a40, mload(0x6960))
            mstore(0x6a60, mload(0x6980))
            mstore(0x6a80, mload(0x69e0))
            mstore(0x6aa0, mload(0x6a00))
            success := and(eq(staticcall(gas(), 0x6, 0x6a40, 0x80, 0x6a40, 0x40), 1), success)
            mstore(0x6ac0, mload(0x6a40))
            mstore(0x6ae0, mload(0x6a60))
            mstore(0x6b00, 0x198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2)
            mstore(0x6b20, 0x1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed)
            mstore(0x6b40, 0x090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b)
            mstore(0x6b60, 0x12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa)
            mstore(0x6b80, mload(0xe20))
            mstore(0x6ba0, mload(0xe40))
            mstore(0x6bc0, 0x0181624e80f3d6ae28df7e01eaeab1c0e919877a3b8a6b7fbc69a6817d596ea2)
            mstore(0x6be0, 0x1783d30dcb12d259bb89098addf6280fa4b653be7a152542a28f7b926e27e648)
            mstore(0x6c00, 0x00ae44489d41a0d179e2dfdc03bddd883b7109f8b6ae316a59e815c1a6b35304)
            mstore(0x6c20, 0x0b2147ab62a386bd63e6de1522109b8c9588ab466f5aadfde8c41ca3749423ee)
            success := and(eq(staticcall(gas(), 0x8, 0x6ac0, 0x180, 0x6ac0, 0x20), 1), success)
            success := and(eq(mload(0x6ac0), 1), success)

            // Revert if anything fails
            if iszero(success) { revert(0, 0) }

            // Return empty bytes on success
            return(0, 0)
        }
    }
}
