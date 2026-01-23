
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
                    let x_lt_p := lt(x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let y_lt_p := lt(y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    valid := and(x_lt_p, y_lt_p)
                }
                {
                    let y_square := mulmod(y, y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_square := mulmod(x, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_cube := mulmod(x_square, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let x_cube_plus_3 := addmod(x_cube, 3, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                    let is_affine := eq(x_cube_plus_3, y_square)
                    valid := and(valid, is_affine)
                }
            }
            mstore(0xa0, mod(calldataload(0x0), f_q))
mstore(0xc0, mod(calldataload(0x20), f_q))
mstore(0xe0, mod(calldataload(0x40), f_q))
mstore(0x100, mod(calldataload(0x60), f_q))
mstore(0x80, 3475706060284763112378093767481683093961835714470762159020831708194270463945)

        {
            let x := calldataload(0x80)
            mstore(0x120, x)
            let y := calldataload(0xa0)
            mstore(0x140, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0xc0)
            mstore(0x160, x)
            let y := calldataload(0xe0)
            mstore(0x180, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x1a0, keccak256(0x80, 288))
{
            let hash := mload(0x1a0)
            mstore(0x1c0, mod(hash, f_q))
            mstore(0x1e0, hash)
        }
mstore8(512, 1)
mstore(0x200, keccak256(0x1e0, 33))
{
            let hash := mload(0x200)
            mstore(0x220, mod(hash, f_q))
            mstore(0x240, hash)
        }
mstore8(608, 1)
mstore(0x260, keccak256(0x240, 33))
{
            let hash := mload(0x260)
            mstore(0x280, mod(hash, f_q))
            mstore(0x2a0, hash)
        }

        {
            let x := calldataload(0x100)
            mstore(0x2c0, x)
            let y := calldataload(0x120)
            mstore(0x2e0, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x140)
            mstore(0x300, x)
            let y := calldataload(0x160)
            mstore(0x320, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x180)
            mstore(0x340, x)
            let y := calldataload(0x1a0)
            mstore(0x360, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x1c0)
            mstore(0x380, x)
            let y := calldataload(0x1e0)
            mstore(0x3a0, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x3c0, keccak256(0x2a0, 288))
{
            let hash := mload(0x3c0)
            mstore(0x3e0, mod(hash, f_q))
            mstore(0x400, hash)
        }

        {
            let x := calldataload(0x200)
            mstore(0x420, x)
            let y := calldataload(0x220)
            mstore(0x440, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x240)
            mstore(0x460, x)
            let y := calldataload(0x260)
            mstore(0x480, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x4a0, keccak256(0x400, 160))
{
            let hash := mload(0x4a0)
            mstore(0x4c0, mod(hash, f_q))
            mstore(0x4e0, hash)
        }
mstore(0x500, mod(calldataload(0x280), f_q))
mstore(0x520, mod(calldataload(0x2a0), f_q))
mstore(0x540, mod(calldataload(0x2c0), f_q))
mstore(0x560, mod(calldataload(0x2e0), f_q))
mstore(0x580, mod(calldataload(0x300), f_q))
mstore(0x5a0, mod(calldataload(0x320), f_q))
mstore(0x5c0, mod(calldataload(0x340), f_q))
mstore(0x5e0, mod(calldataload(0x360), f_q))
mstore(0x600, mod(calldataload(0x380), f_q))
mstore(0x620, mod(calldataload(0x3a0), f_q))
mstore(0x640, mod(calldataload(0x3c0), f_q))
mstore(0x660, mod(calldataload(0x3e0), f_q))
mstore(0x680, mod(calldataload(0x400), f_q))
mstore(0x6a0, mod(calldataload(0x420), f_q))
mstore(0x6c0, mod(calldataload(0x440), f_q))
mstore(0x6e0, keccak256(0x4e0, 512))
{
            let hash := mload(0x6e0)
            mstore(0x700, mod(hash, f_q))
            mstore(0x720, hash)
        }
mstore8(1856, 1)
mstore(0x740, keccak256(0x720, 33))
{
            let hash := mload(0x740)
            mstore(0x760, mod(hash, f_q))
            mstore(0x780, hash)
        }

        {
            let x := calldataload(0x460)
            mstore(0x7a0, x)
            let y := calldataload(0x480)
            mstore(0x7c0, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x7e0, keccak256(0x780, 96))
{
            let hash := mload(0x7e0)
            mstore(0x800, mod(hash, f_q))
            mstore(0x820, hash)
        }

        {
            let x := calldataload(0x4a0)
            mstore(0x840, x)
            let y := calldataload(0x4c0)
            mstore(0x860, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x880, mulmod(mload(0x4c0), mload(0x4c0), f_q))
mstore(0x8a0, mulmod(mload(0x880), mload(0x880), f_q))
mstore(0x8c0, mulmod(mload(0x8a0), mload(0x8a0), f_q))
mstore(0x8e0, mulmod(mload(0x8c0), mload(0x8c0), f_q))
mstore(0x900, mulmod(mload(0x8e0), mload(0x8e0), f_q))
mstore(0x920, mulmod(mload(0x900), mload(0x900), f_q))
mstore(0x940, mulmod(mload(0x920), mload(0x920), f_q))
mstore(0x960, mulmod(mload(0x940), mload(0x940), f_q))
mstore(0x980, mulmod(mload(0x960), mload(0x960), f_q))
mstore(0x9a0, mulmod(mload(0x980), mload(0x980), f_q))
mstore(0x9c0, addmod(mload(0x9a0), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q))
mstore(0x9e0, mulmod(mload(0x9c0), 21866867634659744680037180739646672280844703888306253060159436409049855557633, f_q))
mstore(0xa00, mulmod(mload(0x9e0), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
mstore(0xa20, addmod(mload(0x4c0), 11952173244228085703417150075019950819261217979144509790385672149647937438939, f_q))
mstore(0xa40, mulmod(mload(0x9e0), 1680739780407307830605919050682431078078760076686599579086116998224280619988, f_q))
mstore(0xa60, addmod(mload(0x4c0), 20207503091431967391640486694574844010469604323729434764612087188351527875629, f_q))
mstore(0xa80, mulmod(mload(0x9e0), 14158528901797138466244491986759313854666262535363044392173788062030301470987, f_q))
mstore(0xaa0, addmod(mload(0x4c0), 7729713970042136756001913758497961233882101865052989951524416124545507024630, f_q))
mstore(0xac0, mulmod(mload(0x9e0), 15699029810934084314820646074566828280617789951162923449200398535581206172418, f_q))
mstore(0xae0, addmod(mload(0x4c0), 6189213060905190907425759670690446807930574449253110894497805650994602323199, f_q))
mstore(0xb00, mulmod(mload(0x9e0), 4260969412351770314333984243767775737437927068151180798236715529158398853173, f_q))
mstore(0xb20, addmod(mload(0x4c0), 17627273459487504907912421501489499351110437332264853545461488657417409642444, f_q))
mstore(0xb40, mulmod(mload(0x9e0), 4925592601992654644734291590386747644864797672605745962807370354577123815907, f_q))
mstore(0xb60, addmod(mload(0x4c0), 16962650269846620577512114154870527443683566727810288380890833831998684679710, f_q))
mstore(0xb80, mulmod(mload(0x9e0), 1, f_q))
mstore(0xba0, addmod(mload(0x4c0), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q))
mstore(0xbc0, mulmod(mload(0x9e0), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
mstore(0xbe0, addmod(mload(0x4c0), 2507682784038009475131574039120954579123549720846755509306663987687515178116, f_q))
mstore(0xc00, mulmod(mload(0x9e0), 6252951856119339508807713076978770803512896272623217303779254502899773638908, f_q))
mstore(0xc20, addmod(mload(0x4c0), 15635291015719935713438692668278504285035468127792817039918949683676034856709, f_q))
mstore(0xc40, mulmod(mload(0x9e0), 15554008185779528788857340196607833777388478343360168149406749724843247080062, f_q))
mstore(0xc60, addmod(mload(0x4c0), 6334234686059746433389065548649441311159886057055866194291454461732561415555, f_q))
{
            let prod := mload(0xa20)

                prod := mulmod(mload(0xa60), prod, f_q)
                mstore(0xc80, prod)
            
                prod := mulmod(mload(0xaa0), prod, f_q)
                mstore(0xca0, prod)
            
                prod := mulmod(mload(0xae0), prod, f_q)
                mstore(0xcc0, prod)
            
                prod := mulmod(mload(0xb20), prod, f_q)
                mstore(0xce0, prod)
            
                prod := mulmod(mload(0xb60), prod, f_q)
                mstore(0xd00, prod)
            
                prod := mulmod(mload(0xba0), prod, f_q)
                mstore(0xd20, prod)
            
                prod := mulmod(mload(0xbe0), prod, f_q)
                mstore(0xd40, prod)
            
                prod := mulmod(mload(0xc20), prod, f_q)
                mstore(0xd60, prod)
            
                prod := mulmod(mload(0xc60), prod, f_q)
                mstore(0xd80, prod)
            
                prod := mulmod(mload(0x9c0), prod, f_q)
                mstore(0xda0, prod)
            
        }
mstore(0xde0, 32)
mstore(0xe00, 32)
mstore(0xe20, 32)
mstore(0xe40, mload(0xda0))
mstore(0xe60, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0xe80, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0xde0, 0xc0, 0xdc0, 0x20), 1), success)
{
            
            let inv := mload(0xdc0)
            let v
        
                    v := mload(0x9c0)
                    mstore(2496, mulmod(mload(0xd80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc60)
                    mstore(3168, mulmod(mload(0xd60), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc20)
                    mstore(3104, mulmod(mload(0xd40), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xbe0)
                    mstore(3040, mulmod(mload(0xd20), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xba0)
                    mstore(2976, mulmod(mload(0xd00), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xb60)
                    mstore(2912, mulmod(mload(0xce0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xb20)
                    mstore(2848, mulmod(mload(0xcc0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xae0)
                    mstore(2784, mulmod(mload(0xca0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xaa0)
                    mstore(2720, mulmod(mload(0xc80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xa60)
                    mstore(2656, mulmod(mload(0xa20), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0xa20, inv)

        }
mstore(0xea0, mulmod(mload(0xa00), mload(0xa20), f_q))
mstore(0xec0, mulmod(mload(0xa40), mload(0xa60), f_q))
mstore(0xee0, mulmod(mload(0xa80), mload(0xaa0), f_q))
mstore(0xf00, mulmod(mload(0xac0), mload(0xae0), f_q))
mstore(0xf20, mulmod(mload(0xb00), mload(0xb20), f_q))
mstore(0xf40, mulmod(mload(0xb40), mload(0xb60), f_q))
mstore(0xf60, mulmod(mload(0xb80), mload(0xba0), f_q))
mstore(0xf80, mulmod(mload(0xbc0), mload(0xbe0), f_q))
mstore(0xfa0, mulmod(mload(0xc00), mload(0xc20), f_q))
mstore(0xfc0, mulmod(mload(0xc40), mload(0xc60), f_q))
{
            let result := mulmod(mload(0xf60), mload(0xa0), f_q)
result := addmod(mulmod(mload(0xf80), mload(0xc0), f_q), result, f_q)
result := addmod(mulmod(mload(0xfa0), mload(0xe0), f_q), result, f_q)
result := addmod(mulmod(mload(0xfc0), mload(0x100), f_q), result, f_q)
mstore(4064, result)
        }
mstore(0x1000, addmod(1, sub(f_q, mload(0x5e0)), f_q))
mstore(0x1020, mulmod(mload(0x1000), mload(0xf60), f_q))
mstore(0x1040, mulmod(mload(0x3e0), mload(0x1020), f_q))
mstore(0x1060, mulmod(mload(0x6a0), mload(0x6a0), f_q))
mstore(0x1080, addmod(mload(0x1060), sub(f_q, mload(0x6a0)), f_q))
mstore(0x10a0, mulmod(mload(0x1080), mload(0xea0), f_q))
mstore(0x10c0, addmod(mload(0x1040), mload(0x10a0), f_q))
mstore(0x10e0, mulmod(mload(0x3e0), mload(0x10c0), f_q))
mstore(0x1100, addmod(mload(0x640), sub(f_q, mload(0x620)), f_q))
mstore(0x1120, mulmod(mload(0x1100), mload(0xf60), f_q))
mstore(0x1140, addmod(mload(0x10e0), mload(0x1120), f_q))
mstore(0x1160, mulmod(mload(0x3e0), mload(0x1140), f_q))
mstore(0x1180, addmod(mload(0x6a0), sub(f_q, mload(0x680)), f_q))
mstore(0x11a0, mulmod(mload(0x1180), mload(0xf60), f_q))
mstore(0x11c0, addmod(mload(0x1160), mload(0x11a0), f_q))
mstore(0x11e0, mulmod(mload(0x3e0), mload(0x11c0), f_q))
mstore(0x1200, addmod(1, sub(f_q, mload(0xea0)), f_q))
mstore(0x1220, addmod(mload(0xec0), mload(0xee0), f_q))
mstore(0x1240, addmod(mload(0x1220), mload(0xf00), f_q))
mstore(0x1260, addmod(mload(0x1240), mload(0xf20), f_q))
mstore(0x1280, addmod(mload(0x1260), mload(0xf40), f_q))
mstore(0x12a0, addmod(mload(0x1200), sub(f_q, mload(0x1280)), f_q))
mstore(0x12c0, mulmod(mload(0x580), mload(0x220), f_q))
mstore(0x12e0, addmod(mload(0xfe0), mload(0x12c0), f_q))
mstore(0x1300, addmod(mload(0x12e0), mload(0x280), f_q))
mstore(0x1320, mulmod(mload(0x1300), mload(0x600), f_q))
mstore(0x1340, mulmod(1, mload(0x220), f_q))
mstore(0x1360, mulmod(mload(0x4c0), mload(0x1340), f_q))
mstore(0x1380, addmod(mload(0xfe0), mload(0x1360), f_q))
mstore(0x13a0, addmod(mload(0x1380), mload(0x280), f_q))
mstore(0x13c0, mulmod(mload(0x13a0), mload(0x5e0), f_q))
mstore(0x13e0, addmod(mload(0x1320), sub(f_q, mload(0x13c0)), f_q))
mstore(0x1400, mulmod(mload(0x13e0), mload(0x12a0), f_q))
mstore(0x1420, addmod(mload(0x11e0), mload(0x1400), f_q))
mstore(0x1440, mulmod(mload(0x3e0), mload(0x1420), f_q))
mstore(0x1460, mulmod(mload(0x5a0), mload(0x220), f_q))
mstore(0x1480, addmod(mload(0x500), mload(0x1460), f_q))
mstore(0x14a0, addmod(mload(0x1480), mload(0x280), f_q))
mstore(0x14c0, mulmod(mload(0x14a0), mload(0x660), f_q))
mstore(0x14e0, mulmod(4131629893567559867359510883348571134090853742863529169391034518566172092834, mload(0x220), f_q))
mstore(0x1500, mulmod(mload(0x4c0), mload(0x14e0), f_q))
mstore(0x1520, addmod(mload(0x500), mload(0x1500), f_q))
mstore(0x1540, addmod(mload(0x1520), mload(0x280), f_q))
mstore(0x1560, mulmod(mload(0x1540), mload(0x640), f_q))
mstore(0x1580, addmod(mload(0x14c0), sub(f_q, mload(0x1560)), f_q))
mstore(0x15a0, mulmod(mload(0x1580), mload(0x12a0), f_q))
mstore(0x15c0, addmod(mload(0x1440), mload(0x15a0), f_q))
mstore(0x15e0, mulmod(mload(0x3e0), mload(0x15c0), f_q))
mstore(0x1600, mulmod(mload(0x5c0), mload(0x220), f_q))
mstore(0x1620, addmod(mload(0x520), mload(0x1600), f_q))
mstore(0x1640, addmod(mload(0x1620), mload(0x280), f_q))
mstore(0x1660, mulmod(mload(0x1640), mload(0x6c0), f_q))
mstore(0x1680, mulmod(8910878055287538404433155982483128285667088683464058436815641868457422632747, mload(0x220), f_q))
mstore(0x16a0, mulmod(mload(0x4c0), mload(0x1680), f_q))
mstore(0x16c0, addmod(mload(0x520), mload(0x16a0), f_q))
mstore(0x16e0, addmod(mload(0x16c0), mload(0x280), f_q))
mstore(0x1700, mulmod(mload(0x16e0), mload(0x6a0), f_q))
mstore(0x1720, addmod(mload(0x1660), sub(f_q, mload(0x1700)), f_q))
mstore(0x1740, mulmod(mload(0x1720), mload(0x12a0), f_q))
mstore(0x1760, addmod(mload(0x15e0), mload(0x1740), f_q))
mstore(0x1780, mulmod(mload(0x9a0), mload(0x9a0), f_q))
mstore(0x17a0, mulmod(1, mload(0x9a0), f_q))
mstore(0x17c0, mulmod(mload(0x1760), mload(0x9c0), f_q))
mstore(0x17e0, mulmod(mload(0x880), mload(0x4c0), f_q))
mstore(0x1800, mulmod(mload(0x4c0), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
mstore(0x1820, addmod(mload(0x800), sub(f_q, mload(0x1800)), f_q))
mstore(0x1840, mulmod(mload(0x4c0), 1, f_q))
mstore(0x1860, addmod(mload(0x800), sub(f_q, mload(0x1840)), f_q))
mstore(0x1880, mulmod(mload(0x4c0), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
mstore(0x18a0, addmod(mload(0x800), sub(f_q, mload(0x1880)), f_q))
{
            let result := mulmod(mload(0x800), 1, f_q)
result := addmod(mulmod(mload(0x4c0), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q), result, f_q)
mstore(6336, result)
        }
mstore(0x18e0, mulmod(1, mload(0x1860), f_q))
mstore(0x1900, mulmod(16140595808673403009154643164823336476463527776677864878778453135559733237044, mload(0x880), f_q))
mstore(0x1920, mulmod(mload(0x1900), 1, f_q))
{
            let result := mulmod(mload(0x800), mload(0x1900), f_q)
result := addmod(mulmod(mload(0x4c0), sub(f_q, mload(0x1920)), f_q), result, f_q)
mstore(6464, result)
        }
mstore(0x1960, mulmod(17015964487361230672162623735654618573844832338054897787312333529290879253714, mload(0x880), f_q))
mstore(0x1980, mulmod(mload(0x1960), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
{
            let result := mulmod(mload(0x800), mload(0x1960), f_q)
result := addmod(mulmod(mload(0x4c0), sub(f_q, mload(0x1980)), f_q), result, f_q)
mstore(6560, result)
        }
mstore(0x19c0, mulmod(19187508498431587163140984396833674282302409422288044257471288693049179355069, mload(0x880), f_q))
mstore(0x19e0, mulmod(mload(0x19c0), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
{
            let result := mulmod(mload(0x800), mload(0x19c0), f_q)
result := addmod(mulmod(mload(0x4c0), sub(f_q, mload(0x19e0)), f_q), result, f_q)
mstore(6656, result)
        }
mstore(0x1a20, mulmod(mload(0x18e0), mload(0x18a0), f_q))
mstore(0x1a40, mulmod(mload(0x1a20), mload(0x1820), f_q))
mstore(0x1a60, mulmod(2507682784038009475131574039120954579123549720846755509306663987687515178117, mload(0x4c0), f_q))
mstore(0x1a80, mulmod(mload(0x1a60), 1, f_q))
{
            let result := mulmod(mload(0x800), mload(0x1a60), f_q)
result := addmod(mulmod(mload(0x4c0), sub(f_q, mload(0x1a80)), f_q), result, f_q)
mstore(6816, result)
        }
mstore(0x1ac0, mulmod(19380560087801265747114831706136320509424814679569278834391540198888293317500, mload(0x4c0), f_q))
mstore(0x1ae0, mulmod(mload(0x1ac0), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
{
            let result := mulmod(mload(0x800), mload(0x1ac0), f_q)
result := addmod(mulmod(mload(0x4c0), sub(f_q, mload(0x1ae0)), f_q), result, f_q)
mstore(6912, result)
        }
{
            let prod := mload(0x18c0)

                prod := mulmod(mload(0x1940), prod, f_q)
                mstore(0x1b20, prod)
            
                prod := mulmod(mload(0x19a0), prod, f_q)
                mstore(0x1b40, prod)
            
                prod := mulmod(mload(0x1a00), prod, f_q)
                mstore(0x1b60, prod)
            
                prod := mulmod(mload(0x1a40), prod, f_q)
                mstore(0x1b80, prod)
            
                prod := mulmod(mload(0x1aa0), prod, f_q)
                mstore(0x1ba0, prod)
            
                prod := mulmod(mload(0x1b00), prod, f_q)
                mstore(0x1bc0, prod)
            
                prod := mulmod(mload(0x1a20), prod, f_q)
                mstore(0x1be0, prod)
            
        }
mstore(0x1c20, 32)
mstore(0x1c40, 32)
mstore(0x1c60, 32)
mstore(0x1c80, mload(0x1be0))
mstore(0x1ca0, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0x1cc0, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0x1c20, 0xc0, 0x1c00, 0x20), 1), success)
{
            
            let inv := mload(0x1c00)
            let v
        
                    v := mload(0x1a20)
                    mstore(6688, mulmod(mload(0x1bc0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1b00)
                    mstore(6912, mulmod(mload(0x1ba0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1aa0)
                    mstore(6816, mulmod(mload(0x1b80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1a40)
                    mstore(6720, mulmod(mload(0x1b60), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1a00)
                    mstore(6656, mulmod(mload(0x1b40), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x19a0)
                    mstore(6560, mulmod(mload(0x1b20), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1940)
                    mstore(6464, mulmod(mload(0x18c0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0x18c0, inv)

        }
{
            let result := mload(0x18c0)
mstore(7392, result)
        }
mstore(0x1d00, mulmod(mload(0x18e0), mload(0x1a40), f_q))
{
            let result := mload(0x1940)
result := addmod(mload(0x19a0), result, f_q)
result := addmod(mload(0x1a00), result, f_q)
mstore(7456, result)
        }
mstore(0x1d40, mulmod(mload(0x18e0), mload(0x1a20), f_q))
{
            let result := mload(0x1aa0)
result := addmod(mload(0x1b00), result, f_q)
mstore(7520, result)
        }
{
            let prod := mload(0x1ce0)

                prod := mulmod(mload(0x1d20), prod, f_q)
                mstore(0x1d80, prod)
            
                prod := mulmod(mload(0x1d60), prod, f_q)
                mstore(0x1da0, prod)
            
        }
mstore(0x1de0, 32)
mstore(0x1e00, 32)
mstore(0x1e20, 32)
mstore(0x1e40, mload(0x1da0))
mstore(0x1e60, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0x1e80, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0x1de0, 0xc0, 0x1dc0, 0x20), 1), success)
{
            
            let inv := mload(0x1dc0)
            let v
        
                    v := mload(0x1d60)
                    mstore(7520, mulmod(mload(0x1d80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1d20)
                    mstore(7456, mulmod(mload(0x1ce0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0x1ce0, inv)

        }
mstore(0x1ea0, mulmod(mload(0x1d00), mload(0x1d20), f_q))
mstore(0x1ec0, mulmod(mload(0x1d40), mload(0x1d60), f_q))
mstore(0x1ee0, mulmod(mload(0x700), mload(0x700), f_q))
mstore(0x1f00, mulmod(mload(0x1ee0), mload(0x700), f_q))
mstore(0x1f20, mulmod(mload(0x1f00), mload(0x700), f_q))
mstore(0x1f40, mulmod(mload(0x1f20), mload(0x700), f_q))
mstore(0x1f60, mulmod(mload(0x1f40), mload(0x700), f_q))
mstore(0x1f80, mulmod(mload(0x1f60), mload(0x700), f_q))
mstore(0x1fa0, mulmod(mload(0x1f80), mload(0x700), f_q))
mstore(0x1fc0, mulmod(mload(0x760), mload(0x760), f_q))
mstore(0x1fe0, mulmod(mload(0x1fc0), mload(0x760), f_q))
{
            let result := mulmod(mload(0x500), mload(0x18c0), f_q)
mstore(8192, result)
        }
mstore(0x2020, mulmod(mload(0x2000), mload(0x1ce0), f_q))
mstore(0x2040, mulmod(sub(f_q, mload(0x2020)), 1, f_q))
{
            let result := mulmod(mload(0x520), mload(0x18c0), f_q)
mstore(8288, result)
        }
mstore(0x2080, mulmod(mload(0x2060), mload(0x1ce0), f_q))
mstore(0x20a0, mulmod(sub(f_q, mload(0x2080)), mload(0x700), f_q))
mstore(0x20c0, mulmod(1, mload(0x700), f_q))
mstore(0x20e0, addmod(mload(0x2040), mload(0x20a0), f_q))
{
            let result := mulmod(mload(0x540), mload(0x18c0), f_q)
mstore(8448, result)
        }
mstore(0x2120, mulmod(mload(0x2100), mload(0x1ce0), f_q))
mstore(0x2140, mulmod(sub(f_q, mload(0x2120)), mload(0x1ee0), f_q))
mstore(0x2160, mulmod(1, mload(0x1ee0), f_q))
mstore(0x2180, addmod(mload(0x20e0), mload(0x2140), f_q))
{
            let result := mulmod(mload(0x580), mload(0x18c0), f_q)
mstore(8608, result)
        }
mstore(0x21c0, mulmod(mload(0x21a0), mload(0x1ce0), f_q))
mstore(0x21e0, mulmod(sub(f_q, mload(0x21c0)), mload(0x1f00), f_q))
mstore(0x2200, mulmod(1, mload(0x1f00), f_q))
mstore(0x2220, addmod(mload(0x2180), mload(0x21e0), f_q))
{
            let result := mulmod(mload(0x5a0), mload(0x18c0), f_q)
mstore(8768, result)
        }
mstore(0x2260, mulmod(mload(0x2240), mload(0x1ce0), f_q))
mstore(0x2280, mulmod(sub(f_q, mload(0x2260)), mload(0x1f20), f_q))
mstore(0x22a0, mulmod(1, mload(0x1f20), f_q))
mstore(0x22c0, addmod(mload(0x2220), mload(0x2280), f_q))
{
            let result := mulmod(mload(0x5c0), mload(0x18c0), f_q)
mstore(8928, result)
        }
mstore(0x2300, mulmod(mload(0x22e0), mload(0x1ce0), f_q))
mstore(0x2320, mulmod(sub(f_q, mload(0x2300)), mload(0x1f40), f_q))
mstore(0x2340, mulmod(1, mload(0x1f40), f_q))
mstore(0x2360, addmod(mload(0x22c0), mload(0x2320), f_q))
{
            let result := mulmod(mload(0x17c0), mload(0x18c0), f_q)
mstore(9088, result)
        }
mstore(0x23a0, mulmod(mload(0x2380), mload(0x1ce0), f_q))
mstore(0x23c0, mulmod(sub(f_q, mload(0x23a0)), mload(0x1f60), f_q))
mstore(0x23e0, mulmod(1, mload(0x1f60), f_q))
mstore(0x2400, mulmod(mload(0x17a0), mload(0x1f60), f_q))
mstore(0x2420, addmod(mload(0x2360), mload(0x23c0), f_q))
{
            let result := mulmod(mload(0x560), mload(0x18c0), f_q)
mstore(9280, result)
        }
mstore(0x2460, mulmod(mload(0x2440), mload(0x1ce0), f_q))
mstore(0x2480, mulmod(sub(f_q, mload(0x2460)), mload(0x1f80), f_q))
mstore(0x24a0, mulmod(1, mload(0x1f80), f_q))
mstore(0x24c0, addmod(mload(0x2420), mload(0x2480), f_q))
mstore(0x24e0, mulmod(mload(0x24c0), 1, f_q))
mstore(0x2500, mulmod(mload(0x20c0), 1, f_q))
mstore(0x2520, mulmod(mload(0x2160), 1, f_q))
mstore(0x2540, mulmod(mload(0x2200), 1, f_q))
mstore(0x2560, mulmod(mload(0x22a0), 1, f_q))
mstore(0x2580, mulmod(mload(0x2340), 1, f_q))
mstore(0x25a0, mulmod(mload(0x23e0), 1, f_q))
mstore(0x25c0, mulmod(mload(0x2400), 1, f_q))
mstore(0x25e0, mulmod(mload(0x24a0), 1, f_q))
mstore(0x2600, mulmod(1, mload(0x1d00), f_q))
{
            let result := mulmod(mload(0x5e0), mload(0x1940), f_q)
result := addmod(mulmod(mload(0x600), mload(0x19a0), f_q), result, f_q)
result := addmod(mulmod(mload(0x620), mload(0x1a00), f_q), result, f_q)
mstore(9760, result)
        }
mstore(0x2640, mulmod(mload(0x2620), mload(0x1ea0), f_q))
mstore(0x2660, mulmod(sub(f_q, mload(0x2640)), 1, f_q))
mstore(0x2680, mulmod(mload(0x2600), 1, f_q))
{
            let result := mulmod(mload(0x640), mload(0x1940), f_q)
result := addmod(mulmod(mload(0x660), mload(0x19a0), f_q), result, f_q)
result := addmod(mulmod(mload(0x680), mload(0x1a00), f_q), result, f_q)
mstore(9888, result)
        }
mstore(0x26c0, mulmod(mload(0x26a0), mload(0x1ea0), f_q))
mstore(0x26e0, mulmod(sub(f_q, mload(0x26c0)), mload(0x700), f_q))
mstore(0x2700, mulmod(mload(0x2600), mload(0x700), f_q))
mstore(0x2720, addmod(mload(0x2660), mload(0x26e0), f_q))
mstore(0x2740, mulmod(mload(0x2720), mload(0x760), f_q))
mstore(0x2760, mulmod(mload(0x2680), mload(0x760), f_q))
mstore(0x2780, mulmod(mload(0x2700), mload(0x760), f_q))
mstore(0x27a0, addmod(mload(0x24e0), mload(0x2740), f_q))
mstore(0x27c0, mulmod(1, mload(0x1d40), f_q))
{
            let result := mulmod(mload(0x6a0), mload(0x1aa0), f_q)
result := addmod(mulmod(mload(0x6c0), mload(0x1b00), f_q), result, f_q)
mstore(10208, result)
        }
mstore(0x2800, mulmod(mload(0x27e0), mload(0x1ec0), f_q))
mstore(0x2820, mulmod(sub(f_q, mload(0x2800)), 1, f_q))
mstore(0x2840, mulmod(mload(0x27c0), 1, f_q))
mstore(0x2860, mulmod(mload(0x2820), mload(0x1fc0), f_q))
mstore(0x2880, mulmod(mload(0x2840), mload(0x1fc0), f_q))
mstore(0x28a0, addmod(mload(0x27a0), mload(0x2860), f_q))
mstore(0x28c0, mulmod(1, mload(0x18e0), f_q))
mstore(0x28e0, mulmod(1, mload(0x800), f_q))
mstore(0x2900, 0x0000000000000000000000000000000000000000000000000000000000000001)
                    mstore(0x2920, 0x0000000000000000000000000000000000000000000000000000000000000002)
mstore(0x2940, mload(0x28a0))
success := and(eq(staticcall(gas(), 0x7, 0x2900, 0x60, 0x2900, 0x40), 1), success)
mstore(0x2960, mload(0x2900))
                    mstore(0x2980, mload(0x2920))
mstore(0x29a0, mload(0x120))
                    mstore(0x29c0, mload(0x140))
success := and(eq(staticcall(gas(), 0x6, 0x2960, 0x80, 0x2960, 0x40), 1), success)
mstore(0x29e0, mload(0x160))
                    mstore(0x2a00, mload(0x180))
mstore(0x2a20, mload(0x2500))
success := and(eq(staticcall(gas(), 0x7, 0x29e0, 0x60, 0x29e0, 0x40), 1), success)
mstore(0x2a40, mload(0x2960))
                    mstore(0x2a60, mload(0x2980))
mstore(0x2a80, mload(0x29e0))
                    mstore(0x2aa0, mload(0x2a00))
success := and(eq(staticcall(gas(), 0x6, 0x2a40, 0x80, 0x2a40, 0x40), 1), success)
mstore(0x2ac0, 0x0000000000000000000000000000000000000000000000000000000000000000)
                    mstore(0x2ae0, 0x0000000000000000000000000000000000000000000000000000000000000000)
mstore(0x2b00, mload(0x2520))
success := and(eq(staticcall(gas(), 0x7, 0x2ac0, 0x60, 0x2ac0, 0x40), 1), success)
mstore(0x2b20, mload(0x2a40))
                    mstore(0x2b40, mload(0x2a60))
mstore(0x2b60, mload(0x2ac0))
                    mstore(0x2b80, mload(0x2ae0))
success := and(eq(staticcall(gas(), 0x6, 0x2b20, 0x80, 0x2b20, 0x40), 1), success)
mstore(0x2ba0, 0x187c8d07904f870af40ca36bf90ac3147b4217a2ec551c003fad5ece6d59390a)
                    mstore(0x2bc0, 0x297b5acdc43475aa7d0964564430d16ba64f8070dbf2779707f523203d038448)
mstore(0x2be0, mload(0x2540))
success := and(eq(staticcall(gas(), 0x7, 0x2ba0, 0x60, 0x2ba0, 0x40), 1), success)
mstore(0x2c00, mload(0x2b20))
                    mstore(0x2c20, mload(0x2b40))
mstore(0x2c40, mload(0x2ba0))
                    mstore(0x2c60, mload(0x2bc0))
success := and(eq(staticcall(gas(), 0x6, 0x2c00, 0x80, 0x2c00, 0x40), 1), success)
mstore(0x2c80, 0x0b51ce6f5f4c95fba7f9d38e24bb6be9802ae77a5bd5c8b0bac7aca06d649519)
                    mstore(0x2ca0, 0x0c807bd46cc0a46766d69331a4fc1720e5ea772093579dad692e7289b98a2c37)
mstore(0x2cc0, mload(0x2560))
success := and(eq(staticcall(gas(), 0x7, 0x2c80, 0x60, 0x2c80, 0x40), 1), success)
mstore(0x2ce0, mload(0x2c00))
                    mstore(0x2d00, mload(0x2c20))
mstore(0x2d20, mload(0x2c80))
                    mstore(0x2d40, mload(0x2ca0))
success := and(eq(staticcall(gas(), 0x6, 0x2ce0, 0x80, 0x2ce0, 0x40), 1), success)
mstore(0x2d60, 0x10d9aec78f85af2d55a513619475d43fbc04d0735a50e5d09915b3075a7195a6)
                    mstore(0x2d80, 0x08b27223bea9c499dee4dc5f34e5754902b17a7b18c87c92af2a35ef5c69f504)
mstore(0x2da0, mload(0x2580))
success := and(eq(staticcall(gas(), 0x7, 0x2d60, 0x60, 0x2d60, 0x40), 1), success)
mstore(0x2dc0, mload(0x2ce0))
                    mstore(0x2de0, mload(0x2d00))
mstore(0x2e00, mload(0x2d60))
                    mstore(0x2e20, mload(0x2d80))
success := and(eq(staticcall(gas(), 0x6, 0x2dc0, 0x80, 0x2dc0, 0x40), 1), success)
mstore(0x2e40, mload(0x420))
                    mstore(0x2e60, mload(0x440))
mstore(0x2e80, mload(0x25a0))
success := and(eq(staticcall(gas(), 0x7, 0x2e40, 0x60, 0x2e40, 0x40), 1), success)
mstore(0x2ea0, mload(0x2dc0))
                    mstore(0x2ec0, mload(0x2de0))
mstore(0x2ee0, mload(0x2e40))
                    mstore(0x2f00, mload(0x2e60))
success := and(eq(staticcall(gas(), 0x6, 0x2ea0, 0x80, 0x2ea0, 0x40), 1), success)
mstore(0x2f20, mload(0x460))
                    mstore(0x2f40, mload(0x480))
mstore(0x2f60, mload(0x25c0))
success := and(eq(staticcall(gas(), 0x7, 0x2f20, 0x60, 0x2f20, 0x40), 1), success)
mstore(0x2f80, mload(0x2ea0))
                    mstore(0x2fa0, mload(0x2ec0))
mstore(0x2fc0, mload(0x2f20))
                    mstore(0x2fe0, mload(0x2f40))
success := and(eq(staticcall(gas(), 0x6, 0x2f80, 0x80, 0x2f80, 0x40), 1), success)
mstore(0x3000, mload(0x380))
                    mstore(0x3020, mload(0x3a0))
mstore(0x3040, mload(0x25e0))
success := and(eq(staticcall(gas(), 0x7, 0x3000, 0x60, 0x3000, 0x40), 1), success)
mstore(0x3060, mload(0x2f80))
                    mstore(0x3080, mload(0x2fa0))
mstore(0x30a0, mload(0x3000))
                    mstore(0x30c0, mload(0x3020))
success := and(eq(staticcall(gas(), 0x6, 0x3060, 0x80, 0x3060, 0x40), 1), success)
mstore(0x30e0, mload(0x2c0))
                    mstore(0x3100, mload(0x2e0))
mstore(0x3120, mload(0x2760))
success := and(eq(staticcall(gas(), 0x7, 0x30e0, 0x60, 0x30e0, 0x40), 1), success)
mstore(0x3140, mload(0x3060))
                    mstore(0x3160, mload(0x3080))
mstore(0x3180, mload(0x30e0))
                    mstore(0x31a0, mload(0x3100))
success := and(eq(staticcall(gas(), 0x6, 0x3140, 0x80, 0x3140, 0x40), 1), success)
mstore(0x31c0, mload(0x300))
                    mstore(0x31e0, mload(0x320))
mstore(0x3200, mload(0x2780))
success := and(eq(staticcall(gas(), 0x7, 0x31c0, 0x60, 0x31c0, 0x40), 1), success)
mstore(0x3220, mload(0x3140))
                    mstore(0x3240, mload(0x3160))
mstore(0x3260, mload(0x31c0))
                    mstore(0x3280, mload(0x31e0))
success := and(eq(staticcall(gas(), 0x6, 0x3220, 0x80, 0x3220, 0x40), 1), success)
mstore(0x32a0, mload(0x340))
                    mstore(0x32c0, mload(0x360))
mstore(0x32e0, mload(0x2880))
success := and(eq(staticcall(gas(), 0x7, 0x32a0, 0x60, 0x32a0, 0x40), 1), success)
mstore(0x3300, mload(0x3220))
                    mstore(0x3320, mload(0x3240))
mstore(0x3340, mload(0x32a0))
                    mstore(0x3360, mload(0x32c0))
success := and(eq(staticcall(gas(), 0x6, 0x3300, 0x80, 0x3300, 0x40), 1), success)
mstore(0x3380, mload(0x7a0))
                    mstore(0x33a0, mload(0x7c0))
mstore(0x33c0, sub(f_q, mload(0x28c0)))
success := and(eq(staticcall(gas(), 0x7, 0x3380, 0x60, 0x3380, 0x40), 1), success)
mstore(0x33e0, mload(0x3300))
                    mstore(0x3400, mload(0x3320))
mstore(0x3420, mload(0x3380))
                    mstore(0x3440, mload(0x33a0))
success := and(eq(staticcall(gas(), 0x6, 0x33e0, 0x80, 0x33e0, 0x40), 1), success)
mstore(0x3460, mload(0x840))
                    mstore(0x3480, mload(0x860))
mstore(0x34a0, mload(0x28e0))
success := and(eq(staticcall(gas(), 0x7, 0x3460, 0x60, 0x3460, 0x40), 1), success)
mstore(0x34c0, mload(0x33e0))
                    mstore(0x34e0, mload(0x3400))
mstore(0x3500, mload(0x3460))
                    mstore(0x3520, mload(0x3480))
success := and(eq(staticcall(gas(), 0x6, 0x34c0, 0x80, 0x34c0, 0x40), 1), success)
mstore(0x3540, mload(0x34c0))
                    mstore(0x3560, mload(0x34e0))
mstore(0x3580, 0x198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2)
            mstore(0x35a0, 0x1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed)
            mstore(0x35c0, 0x090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b)
            mstore(0x35e0, 0x12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa)
mstore(0x3600, mload(0x840))
                    mstore(0x3620, mload(0x860))
mstore(0x3640, 0x0181624e80f3d6ae28df7e01eaeab1c0e919877a3b8a6b7fbc69a6817d596ea2)
            mstore(0x3660, 0x1783d30dcb12d259bb89098addf6280fa4b653be7a152542a28f7b926e27e648)
            mstore(0x3680, 0x00ae44489d41a0d179e2dfdc03bddd883b7109f8b6ae316a59e815c1a6b35304)
            mstore(0x36a0, 0x0b2147ab62a386bd63e6de1522109b8c9588ab466f5aadfde8c41ca3749423ee)
success := and(eq(staticcall(gas(), 0x8, 0x3540, 0x180, 0x3540, 0x20), 1), success)
success := and(eq(mload(0x3540), 1), success)

            // Revert if anything fails
            if iszero(success) { revert(0, 0) }

            // Return empty bytes on success
            return(0, 0)

        }
    }
}
        