
// SPDX-License-Identifier: MIT

pragma solidity 0.8.19;

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
mstore(0x80, 17265481047076767654423401945807482871883345805897467940795998301725701241379)

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

        {
            let x := calldataload(0x100)
            mstore(0x1a0, x)
            let y := calldataload(0x120)
            mstore(0x1c0, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x1e0, keccak256(0x80, 352))
{
            let hash := mload(0x1e0)
            mstore(0x200, mod(hash, f_q))
            mstore(0x220, hash)
        }
mstore8(576, 1)
mstore(0x240, keccak256(0x220, 33))
{
            let hash := mload(0x240)
            mstore(0x260, mod(hash, f_q))
            mstore(0x280, hash)
        }
mstore8(672, 1)
mstore(0x2a0, keccak256(0x280, 33))
{
            let hash := mload(0x2a0)
            mstore(0x2c0, mod(hash, f_q))
            mstore(0x2e0, hash)
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

        {
            let x := calldataload(0x200)
            mstore(0x3c0, x)
            let y := calldataload(0x220)
            mstore(0x3e0, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x240)
            mstore(0x400, x)
            let y := calldataload(0x260)
            mstore(0x420, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x440, keccak256(0x2e0, 352))
{
            let hash := mload(0x440)
            mstore(0x460, mod(hash, f_q))
            mstore(0x480, hash)
        }

        {
            let x := calldataload(0x280)
            mstore(0x4a0, x)
            let y := calldataload(0x2a0)
            mstore(0x4c0, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x2c0)
            mstore(0x4e0, x)
            let y := calldataload(0x2e0)
            mstore(0x500, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x520, keccak256(0x480, 160))
{
            let hash := mload(0x520)
            mstore(0x540, mod(hash, f_q))
            mstore(0x560, hash)
        }
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
mstore(0x6e0, mod(calldataload(0x460), f_q))
mstore(0x700, mod(calldataload(0x480), f_q))
mstore(0x720, mod(calldataload(0x4a0), f_q))
mstore(0x740, mod(calldataload(0x4c0), f_q))
mstore(0x760, mod(calldataload(0x4e0), f_q))
mstore(0x780, mod(calldataload(0x500), f_q))
mstore(0x7a0, mod(calldataload(0x520), f_q))
mstore(0x7c0, mod(calldataload(0x540), f_q))
mstore(0x7e0, mod(calldataload(0x560), f_q))
mstore(0x800, keccak256(0x560, 672))
{
            let hash := mload(0x800)
            mstore(0x820, mod(hash, f_q))
            mstore(0x840, hash)
        }
mstore8(2144, 1)
mstore(0x860, keccak256(0x840, 33))
{
            let hash := mload(0x860)
            mstore(0x880, mod(hash, f_q))
            mstore(0x8a0, hash)
        }

        {
            let x := calldataload(0x580)
            mstore(0x8c0, x)
            let y := calldataload(0x5a0)
            mstore(0x8e0, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x900, keccak256(0x8a0, 96))
{
            let hash := mload(0x900)
            mstore(0x920, mod(hash, f_q))
            mstore(0x940, hash)
        }

        {
            let x := calldataload(0x5c0)
            mstore(0x960, x)
            let y := calldataload(0x5e0)
            mstore(0x980, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x9a0, mulmod(mload(0x540), mload(0x540), f_q))
mstore(0x9c0, mulmod(mload(0x9a0), mload(0x9a0), f_q))
mstore(0x9e0, mulmod(mload(0x9c0), mload(0x9c0), f_q))
mstore(0xa00, mulmod(mload(0x9e0), mload(0x9e0), f_q))
mstore(0xa20, mulmod(mload(0xa00), mload(0xa00), f_q))
mstore(0xa40, mulmod(mload(0xa20), mload(0xa20), f_q))
mstore(0xa60, mulmod(mload(0xa40), mload(0xa40), f_q))
mstore(0xa80, mulmod(mload(0xa60), mload(0xa60), f_q))
mstore(0xaa0, mulmod(mload(0xa80), mload(0xa80), f_q))
mstore(0xac0, mulmod(mload(0xaa0), mload(0xaa0), f_q))
mstore(0xae0, addmod(mload(0xac0), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q))
mstore(0xb00, mulmod(mload(0xae0), 21866867634659744680037180739646672280844703888306253060159436409049855557633, f_q))
mstore(0xb20, mulmod(mload(0xb00), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
mstore(0xb40, addmod(mload(0x540), 11952173244228085703417150075019950819261217979144509790385672149647937438939, f_q))
mstore(0xb60, mulmod(mload(0xb00), 1680739780407307830605919050682431078078760076686599579086116998224280619988, f_q))
mstore(0xb80, addmod(mload(0x540), 20207503091431967391640486694574844010469604323729434764612087188351527875629, f_q))
mstore(0xba0, mulmod(mload(0xb00), 14158528901797138466244491986759313854666262535363044392173788062030301470987, f_q))
mstore(0xbc0, addmod(mload(0x540), 7729713970042136756001913758497961233882101865052989951524416124545507024630, f_q))
mstore(0xbe0, mulmod(mload(0xb00), 15699029810934084314820646074566828280617789951162923449200398535581206172418, f_q))
mstore(0xc00, addmod(mload(0x540), 6189213060905190907425759670690446807930574449253110894497805650994602323199, f_q))
mstore(0xc20, mulmod(mload(0xb00), 4260969412351770314333984243767775737437927068151180798236715529158398853173, f_q))
mstore(0xc40, addmod(mload(0x540), 17627273459487504907912421501489499351110437332264853545461488657417409642444, f_q))
mstore(0xc60, mulmod(mload(0xb00), 4925592601992654644734291590386747644864797672605745962807370354577123815907, f_q))
mstore(0xc80, addmod(mload(0x540), 16962650269846620577512114154870527443683566727810288380890833831998684679710, f_q))
mstore(0xca0, mulmod(mload(0xb00), 1, f_q))
mstore(0xcc0, addmod(mload(0x540), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q))
mstore(0xce0, mulmod(mload(0xb00), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
mstore(0xd00, addmod(mload(0x540), 2507682784038009475131574039120954579123549720846755509306663987687515178116, f_q))
mstore(0xd20, mulmod(mload(0xb00), 6252951856119339508807713076978770803512896272623217303779254502899773638908, f_q))
mstore(0xd40, addmod(mload(0x540), 15635291015719935713438692668278504285035468127792817039918949683676034856709, f_q))
mstore(0xd60, mulmod(mload(0xb00), 15554008185779528788857340196607833777388478343360168149406749724843247080062, f_q))
mstore(0xd80, addmod(mload(0x540), 6334234686059746433389065548649441311159886057055866194291454461732561415555, f_q))
{
            let prod := mload(0xb40)

                prod := mulmod(mload(0xb80), prod, f_q)
                mstore(0xda0, prod)
            
                prod := mulmod(mload(0xbc0), prod, f_q)
                mstore(0xdc0, prod)
            
                prod := mulmod(mload(0xc00), prod, f_q)
                mstore(0xde0, prod)
            
                prod := mulmod(mload(0xc40), prod, f_q)
                mstore(0xe00, prod)
            
                prod := mulmod(mload(0xc80), prod, f_q)
                mstore(0xe20, prod)
            
                prod := mulmod(mload(0xcc0), prod, f_q)
                mstore(0xe40, prod)
            
                prod := mulmod(mload(0xd00), prod, f_q)
                mstore(0xe60, prod)
            
                prod := mulmod(mload(0xd40), prod, f_q)
                mstore(0xe80, prod)
            
                prod := mulmod(mload(0xd80), prod, f_q)
                mstore(0xea0, prod)
            
                prod := mulmod(mload(0xae0), prod, f_q)
                mstore(0xec0, prod)
            
        }
mstore(0xf00, 32)
mstore(0xf20, 32)
mstore(0xf40, 32)
mstore(0xf60, mload(0xec0))
mstore(0xf80, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0xfa0, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0xf00, 0xc0, 0xee0, 0x20), 1), success)
{
            
            let inv := mload(0xee0)
            let v
        
                    v := mload(0xae0)
                    mstore(2784, mulmod(mload(0xea0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xd80)
                    mstore(3456, mulmod(mload(0xe80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xd40)
                    mstore(3392, mulmod(mload(0xe60), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xd00)
                    mstore(3328, mulmod(mload(0xe40), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xcc0)
                    mstore(3264, mulmod(mload(0xe20), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc80)
                    mstore(3200, mulmod(mload(0xe00), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc40)
                    mstore(3136, mulmod(mload(0xde0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc00)
                    mstore(3072, mulmod(mload(0xdc0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xbc0)
                    mstore(3008, mulmod(mload(0xda0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xb80)
                    mstore(2944, mulmod(mload(0xb40), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0xb40, inv)

        }
mstore(0xfc0, mulmod(mload(0xb20), mload(0xb40), f_q))
mstore(0xfe0, mulmod(mload(0xb60), mload(0xb80), f_q))
mstore(0x1000, mulmod(mload(0xba0), mload(0xbc0), f_q))
mstore(0x1020, mulmod(mload(0xbe0), mload(0xc00), f_q))
mstore(0x1040, mulmod(mload(0xc20), mload(0xc40), f_q))
mstore(0x1060, mulmod(mload(0xc60), mload(0xc80), f_q))
mstore(0x1080, mulmod(mload(0xca0), mload(0xcc0), f_q))
mstore(0x10a0, mulmod(mload(0xce0), mload(0xd00), f_q))
mstore(0x10c0, mulmod(mload(0xd20), mload(0xd40), f_q))
mstore(0x10e0, mulmod(mload(0xd60), mload(0xd80), f_q))
{
            let result := mulmod(mload(0x1080), mload(0xa0), f_q)
result := addmod(mulmod(mload(0x10a0), mload(0xc0), f_q), result, f_q)
result := addmod(mulmod(mload(0x10c0), mload(0xe0), f_q), result, f_q)
result := addmod(mulmod(mload(0x10e0), mload(0x100), f_q), result, f_q)
mstore(4352, result)
        }
mstore(0x1120, addmod(1, sub(f_q, mload(0x6a0)), f_q))
mstore(0x1140, mulmod(mload(0x1120), mload(0x1080), f_q))
mstore(0x1160, mulmod(mload(0x460), mload(0x1140), f_q))
mstore(0x1180, mulmod(mload(0x7c0), mload(0x7c0), f_q))
mstore(0x11a0, addmod(mload(0x1180), sub(f_q, mload(0x7c0)), f_q))
mstore(0x11c0, mulmod(mload(0x11a0), mload(0xfc0), f_q))
mstore(0x11e0, addmod(mload(0x1160), mload(0x11c0), f_q))
mstore(0x1200, mulmod(mload(0x460), mload(0x11e0), f_q))
mstore(0x1220, addmod(mload(0x700), sub(f_q, mload(0x6e0)), f_q))
mstore(0x1240, mulmod(mload(0x1220), mload(0x1080), f_q))
mstore(0x1260, addmod(mload(0x1200), mload(0x1240), f_q))
mstore(0x1280, mulmod(mload(0x460), mload(0x1260), f_q))
mstore(0x12a0, addmod(mload(0x760), sub(f_q, mload(0x740)), f_q))
mstore(0x12c0, mulmod(mload(0x12a0), mload(0x1080), f_q))
mstore(0x12e0, addmod(mload(0x1280), mload(0x12c0), f_q))
mstore(0x1300, mulmod(mload(0x460), mload(0x12e0), f_q))
mstore(0x1320, addmod(mload(0x7c0), sub(f_q, mload(0x7a0)), f_q))
mstore(0x1340, mulmod(mload(0x1320), mload(0x1080), f_q))
mstore(0x1360, addmod(mload(0x1300), mload(0x1340), f_q))
mstore(0x1380, mulmod(mload(0x460), mload(0x1360), f_q))
mstore(0x13a0, addmod(1, sub(f_q, mload(0xfc0)), f_q))
mstore(0x13c0, addmod(mload(0xfe0), mload(0x1000), f_q))
mstore(0x13e0, addmod(mload(0x13c0), mload(0x1020), f_q))
mstore(0x1400, addmod(mload(0x13e0), mload(0x1040), f_q))
mstore(0x1420, addmod(mload(0x1400), mload(0x1060), f_q))
mstore(0x1440, addmod(mload(0x13a0), sub(f_q, mload(0x1420)), f_q))
mstore(0x1460, mulmod(mload(0x620), mload(0x260), f_q))
mstore(0x1480, addmod(mload(0x1100), mload(0x1460), f_q))
mstore(0x14a0, addmod(mload(0x1480), mload(0x2c0), f_q))
mstore(0x14c0, mulmod(mload(0x14a0), mload(0x6c0), f_q))
mstore(0x14e0, mulmod(1, mload(0x260), f_q))
mstore(0x1500, mulmod(mload(0x540), mload(0x14e0), f_q))
mstore(0x1520, addmod(mload(0x1100), mload(0x1500), f_q))
mstore(0x1540, addmod(mload(0x1520), mload(0x2c0), f_q))
mstore(0x1560, mulmod(mload(0x1540), mload(0x6a0), f_q))
mstore(0x1580, addmod(mload(0x14c0), sub(f_q, mload(0x1560)), f_q))
mstore(0x15a0, mulmod(mload(0x1580), mload(0x1440), f_q))
mstore(0x15c0, addmod(mload(0x1380), mload(0x15a0), f_q))
mstore(0x15e0, mulmod(mload(0x460), mload(0x15c0), f_q))
mstore(0x1600, mulmod(mload(0x640), mload(0x260), f_q))
mstore(0x1620, addmod(mload(0x580), mload(0x1600), f_q))
mstore(0x1640, addmod(mload(0x1620), mload(0x2c0), f_q))
mstore(0x1660, mulmod(mload(0x1640), mload(0x720), f_q))
mstore(0x1680, mulmod(4131629893567559867359510883348571134090853742863529169391034518566172092834, mload(0x260), f_q))
mstore(0x16a0, mulmod(mload(0x540), mload(0x1680), f_q))
mstore(0x16c0, addmod(mload(0x580), mload(0x16a0), f_q))
mstore(0x16e0, addmod(mload(0x16c0), mload(0x2c0), f_q))
mstore(0x1700, mulmod(mload(0x16e0), mload(0x700), f_q))
mstore(0x1720, addmod(mload(0x1660), sub(f_q, mload(0x1700)), f_q))
mstore(0x1740, mulmod(mload(0x1720), mload(0x1440), f_q))
mstore(0x1760, addmod(mload(0x15e0), mload(0x1740), f_q))
mstore(0x1780, mulmod(mload(0x460), mload(0x1760), f_q))
mstore(0x17a0, mulmod(mload(0x660), mload(0x260), f_q))
mstore(0x17c0, addmod(mload(0x5a0), mload(0x17a0), f_q))
mstore(0x17e0, addmod(mload(0x17c0), mload(0x2c0), f_q))
mstore(0x1800, mulmod(mload(0x17e0), mload(0x780), f_q))
mstore(0x1820, mulmod(8910878055287538404433155982483128285667088683464058436815641868457422632747, mload(0x260), f_q))
mstore(0x1840, mulmod(mload(0x540), mload(0x1820), f_q))
mstore(0x1860, addmod(mload(0x5a0), mload(0x1840), f_q))
mstore(0x1880, addmod(mload(0x1860), mload(0x2c0), f_q))
mstore(0x18a0, mulmod(mload(0x1880), mload(0x760), f_q))
mstore(0x18c0, addmod(mload(0x1800), sub(f_q, mload(0x18a0)), f_q))
mstore(0x18e0, mulmod(mload(0x18c0), mload(0x1440), f_q))
mstore(0x1900, addmod(mload(0x1780), mload(0x18e0), f_q))
mstore(0x1920, mulmod(mload(0x460), mload(0x1900), f_q))
mstore(0x1940, mulmod(mload(0x680), mload(0x260), f_q))
mstore(0x1960, addmod(mload(0x5c0), mload(0x1940), f_q))
mstore(0x1980, addmod(mload(0x1960), mload(0x2c0), f_q))
mstore(0x19a0, mulmod(mload(0x1980), mload(0x7e0), f_q))
mstore(0x19c0, mulmod(11166246659983828508719468090013646171463329086121580628794302409516816350802, mload(0x260), f_q))
mstore(0x19e0, mulmod(mload(0x540), mload(0x19c0), f_q))
mstore(0x1a00, addmod(mload(0x5c0), mload(0x19e0), f_q))
mstore(0x1a20, addmod(mload(0x1a00), mload(0x2c0), f_q))
mstore(0x1a40, mulmod(mload(0x1a20), mload(0x7c0), f_q))
mstore(0x1a60, addmod(mload(0x19a0), sub(f_q, mload(0x1a40)), f_q))
mstore(0x1a80, mulmod(mload(0x1a60), mload(0x1440), f_q))
mstore(0x1aa0, addmod(mload(0x1920), mload(0x1a80), f_q))
mstore(0x1ac0, mulmod(mload(0xac0), mload(0xac0), f_q))
mstore(0x1ae0, mulmod(1, mload(0xac0), f_q))
mstore(0x1b00, mulmod(mload(0x1aa0), mload(0xae0), f_q))
mstore(0x1b20, mulmod(mload(0x9a0), mload(0x540), f_q))
mstore(0x1b40, mulmod(mload(0x540), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
mstore(0x1b60, addmod(mload(0x920), sub(f_q, mload(0x1b40)), f_q))
mstore(0x1b80, mulmod(mload(0x540), 1, f_q))
mstore(0x1ba0, addmod(mload(0x920), sub(f_q, mload(0x1b80)), f_q))
mstore(0x1bc0, mulmod(mload(0x540), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
mstore(0x1be0, addmod(mload(0x920), sub(f_q, mload(0x1bc0)), f_q))
{
            let result := mulmod(mload(0x920), 1, f_q)
result := addmod(mulmod(mload(0x540), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q), result, f_q)
mstore(7168, result)
        }
mstore(0x1c20, mulmod(1, mload(0x1ba0), f_q))
mstore(0x1c40, mulmod(16140595808673403009154643164823336476463527776677864878778453135559733237044, mload(0x9a0), f_q))
mstore(0x1c60, mulmod(mload(0x1c40), 1, f_q))
{
            let result := mulmod(mload(0x920), mload(0x1c40), f_q)
result := addmod(mulmod(mload(0x540), sub(f_q, mload(0x1c60)), f_q), result, f_q)
mstore(7296, result)
        }
mstore(0x1ca0, mulmod(17015964487361230672162623735654618573844832338054897787312333529290879253714, mload(0x9a0), f_q))
mstore(0x1cc0, mulmod(mload(0x1ca0), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
{
            let result := mulmod(mload(0x920), mload(0x1ca0), f_q)
result := addmod(mulmod(mload(0x540), sub(f_q, mload(0x1cc0)), f_q), result, f_q)
mstore(7392, result)
        }
mstore(0x1d00, mulmod(19187508498431587163140984396833674282302409422288044257471288693049179355069, mload(0x9a0), f_q))
mstore(0x1d20, mulmod(mload(0x1d00), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
{
            let result := mulmod(mload(0x920), mload(0x1d00), f_q)
result := addmod(mulmod(mload(0x540), sub(f_q, mload(0x1d20)), f_q), result, f_q)
mstore(7488, result)
        }
mstore(0x1d60, mulmod(mload(0x1c20), mload(0x1be0), f_q))
mstore(0x1d80, mulmod(mload(0x1d60), mload(0x1b60), f_q))
mstore(0x1da0, mulmod(2507682784038009475131574039120954579123549720846755509306663987687515178117, mload(0x540), f_q))
mstore(0x1dc0, mulmod(mload(0x1da0), 1, f_q))
{
            let result := mulmod(mload(0x920), mload(0x1da0), f_q)
result := addmod(mulmod(mload(0x540), sub(f_q, mload(0x1dc0)), f_q), result, f_q)
mstore(7648, result)
        }
mstore(0x1e00, mulmod(19380560087801265747114831706136320509424814679569278834391540198888293317500, mload(0x540), f_q))
mstore(0x1e20, mulmod(mload(0x1e00), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
{
            let result := mulmod(mload(0x920), mload(0x1e00), f_q)
result := addmod(mulmod(mload(0x540), sub(f_q, mload(0x1e20)), f_q), result, f_q)
mstore(7744, result)
        }
{
            let prod := mload(0x1c00)

                prod := mulmod(mload(0x1c80), prod, f_q)
                mstore(0x1e60, prod)
            
                prod := mulmod(mload(0x1ce0), prod, f_q)
                mstore(0x1e80, prod)
            
                prod := mulmod(mload(0x1d40), prod, f_q)
                mstore(0x1ea0, prod)
            
                prod := mulmod(mload(0x1d80), prod, f_q)
                mstore(0x1ec0, prod)
            
                prod := mulmod(mload(0x1de0), prod, f_q)
                mstore(0x1ee0, prod)
            
                prod := mulmod(mload(0x1e40), prod, f_q)
                mstore(0x1f00, prod)
            
                prod := mulmod(mload(0x1d60), prod, f_q)
                mstore(0x1f20, prod)
            
        }
mstore(0x1f60, 32)
mstore(0x1f80, 32)
mstore(0x1fa0, 32)
mstore(0x1fc0, mload(0x1f20))
mstore(0x1fe0, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0x2000, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0x1f60, 0xc0, 0x1f40, 0x20), 1), success)
{
            
            let inv := mload(0x1f40)
            let v
        
                    v := mload(0x1d60)
                    mstore(7520, mulmod(mload(0x1f00), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1e40)
                    mstore(7744, mulmod(mload(0x1ee0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1de0)
                    mstore(7648, mulmod(mload(0x1ec0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1d80)
                    mstore(7552, mulmod(mload(0x1ea0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1d40)
                    mstore(7488, mulmod(mload(0x1e80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1ce0)
                    mstore(7392, mulmod(mload(0x1e60), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1c80)
                    mstore(7296, mulmod(mload(0x1c00), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0x1c00, inv)

        }
{
            let result := mload(0x1c00)
mstore(8224, result)
        }
mstore(0x2040, mulmod(mload(0x1c20), mload(0x1d80), f_q))
{
            let result := mload(0x1c80)
result := addmod(mload(0x1ce0), result, f_q)
result := addmod(mload(0x1d40), result, f_q)
mstore(8288, result)
        }
mstore(0x2080, mulmod(mload(0x1c20), mload(0x1d60), f_q))
{
            let result := mload(0x1de0)
result := addmod(mload(0x1e40), result, f_q)
mstore(8352, result)
        }
{
            let prod := mload(0x2020)

                prod := mulmod(mload(0x2060), prod, f_q)
                mstore(0x20c0, prod)
            
                prod := mulmod(mload(0x20a0), prod, f_q)
                mstore(0x20e0, prod)
            
        }
mstore(0x2120, 32)
mstore(0x2140, 32)
mstore(0x2160, 32)
mstore(0x2180, mload(0x20e0))
mstore(0x21a0, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0x21c0, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0x2120, 0xc0, 0x2100, 0x20), 1), success)
{
            
            let inv := mload(0x2100)
            let v
        
                    v := mload(0x20a0)
                    mstore(8352, mulmod(mload(0x20c0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x2060)
                    mstore(8288, mulmod(mload(0x2020), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0x2020, inv)

        }
mstore(0x21e0, mulmod(mload(0x2040), mload(0x2060), f_q))
mstore(0x2200, mulmod(mload(0x2080), mload(0x20a0), f_q))
mstore(0x2220, mulmod(mload(0x820), mload(0x820), f_q))
mstore(0x2240, mulmod(mload(0x2220), mload(0x820), f_q))
mstore(0x2260, mulmod(mload(0x2240), mload(0x820), f_q))
mstore(0x2280, mulmod(mload(0x2260), mload(0x820), f_q))
mstore(0x22a0, mulmod(mload(0x2280), mload(0x820), f_q))
mstore(0x22c0, mulmod(mload(0x22a0), mload(0x820), f_q))
mstore(0x22e0, mulmod(mload(0x22c0), mload(0x820), f_q))
mstore(0x2300, mulmod(mload(0x22e0), mload(0x820), f_q))
mstore(0x2320, mulmod(mload(0x2300), mload(0x820), f_q))
mstore(0x2340, mulmod(mload(0x880), mload(0x880), f_q))
mstore(0x2360, mulmod(mload(0x2340), mload(0x880), f_q))
{
            let result := mulmod(mload(0x580), mload(0x1c00), f_q)
mstore(9088, result)
        }
mstore(0x23a0, mulmod(mload(0x2380), mload(0x2020), f_q))
mstore(0x23c0, mulmod(sub(f_q, mload(0x23a0)), 1, f_q))
{
            let result := mulmod(mload(0x5a0), mload(0x1c00), f_q)
mstore(9184, result)
        }
mstore(0x2400, mulmod(mload(0x23e0), mload(0x2020), f_q))
mstore(0x2420, mulmod(sub(f_q, mload(0x2400)), mload(0x820), f_q))
mstore(0x2440, mulmod(1, mload(0x820), f_q))
mstore(0x2460, addmod(mload(0x23c0), mload(0x2420), f_q))
{
            let result := mulmod(mload(0x5c0), mload(0x1c00), f_q)
mstore(9344, result)
        }
mstore(0x24a0, mulmod(mload(0x2480), mload(0x2020), f_q))
mstore(0x24c0, mulmod(sub(f_q, mload(0x24a0)), mload(0x2220), f_q))
mstore(0x24e0, mulmod(1, mload(0x2220), f_q))
mstore(0x2500, addmod(mload(0x2460), mload(0x24c0), f_q))
{
            let result := mulmod(mload(0x5e0), mload(0x1c00), f_q)
mstore(9504, result)
        }
mstore(0x2540, mulmod(mload(0x2520), mload(0x2020), f_q))
mstore(0x2560, mulmod(sub(f_q, mload(0x2540)), mload(0x2240), f_q))
mstore(0x2580, mulmod(1, mload(0x2240), f_q))
mstore(0x25a0, addmod(mload(0x2500), mload(0x2560), f_q))
{
            let result := mulmod(mload(0x620), mload(0x1c00), f_q)
mstore(9664, result)
        }
mstore(0x25e0, mulmod(mload(0x25c0), mload(0x2020), f_q))
mstore(0x2600, mulmod(sub(f_q, mload(0x25e0)), mload(0x2260), f_q))
mstore(0x2620, mulmod(1, mload(0x2260), f_q))
mstore(0x2640, addmod(mload(0x25a0), mload(0x2600), f_q))
{
            let result := mulmod(mload(0x640), mload(0x1c00), f_q)
mstore(9824, result)
        }
mstore(0x2680, mulmod(mload(0x2660), mload(0x2020), f_q))
mstore(0x26a0, mulmod(sub(f_q, mload(0x2680)), mload(0x2280), f_q))
mstore(0x26c0, mulmod(1, mload(0x2280), f_q))
mstore(0x26e0, addmod(mload(0x2640), mload(0x26a0), f_q))
{
            let result := mulmod(mload(0x660), mload(0x1c00), f_q)
mstore(9984, result)
        }
mstore(0x2720, mulmod(mload(0x2700), mload(0x2020), f_q))
mstore(0x2740, mulmod(sub(f_q, mload(0x2720)), mload(0x22a0), f_q))
mstore(0x2760, mulmod(1, mload(0x22a0), f_q))
mstore(0x2780, addmod(mload(0x26e0), mload(0x2740), f_q))
{
            let result := mulmod(mload(0x680), mload(0x1c00), f_q)
mstore(10144, result)
        }
mstore(0x27c0, mulmod(mload(0x27a0), mload(0x2020), f_q))
mstore(0x27e0, mulmod(sub(f_q, mload(0x27c0)), mload(0x22c0), f_q))
mstore(0x2800, mulmod(1, mload(0x22c0), f_q))
mstore(0x2820, addmod(mload(0x2780), mload(0x27e0), f_q))
{
            let result := mulmod(mload(0x1b00), mload(0x1c00), f_q)
mstore(10304, result)
        }
mstore(0x2860, mulmod(mload(0x2840), mload(0x2020), f_q))
mstore(0x2880, mulmod(sub(f_q, mload(0x2860)), mload(0x22e0), f_q))
mstore(0x28a0, mulmod(1, mload(0x22e0), f_q))
mstore(0x28c0, mulmod(mload(0x1ae0), mload(0x22e0), f_q))
mstore(0x28e0, addmod(mload(0x2820), mload(0x2880), f_q))
{
            let result := mulmod(mload(0x600), mload(0x1c00), f_q)
mstore(10496, result)
        }
mstore(0x2920, mulmod(mload(0x2900), mload(0x2020), f_q))
mstore(0x2940, mulmod(sub(f_q, mload(0x2920)), mload(0x2300), f_q))
mstore(0x2960, mulmod(1, mload(0x2300), f_q))
mstore(0x2980, addmod(mload(0x28e0), mload(0x2940), f_q))
mstore(0x29a0, mulmod(mload(0x2980), 1, f_q))
mstore(0x29c0, mulmod(mload(0x2440), 1, f_q))
mstore(0x29e0, mulmod(mload(0x24e0), 1, f_q))
mstore(0x2a00, mulmod(mload(0x2580), 1, f_q))
mstore(0x2a20, mulmod(mload(0x2620), 1, f_q))
mstore(0x2a40, mulmod(mload(0x26c0), 1, f_q))
mstore(0x2a60, mulmod(mload(0x2760), 1, f_q))
mstore(0x2a80, mulmod(mload(0x2800), 1, f_q))
mstore(0x2aa0, mulmod(mload(0x28a0), 1, f_q))
mstore(0x2ac0, mulmod(mload(0x28c0), 1, f_q))
mstore(0x2ae0, mulmod(mload(0x2960), 1, f_q))
mstore(0x2b00, mulmod(1, mload(0x2040), f_q))
{
            let result := mulmod(mload(0x6a0), mload(0x1c80), f_q)
result := addmod(mulmod(mload(0x6c0), mload(0x1ce0), f_q), result, f_q)
result := addmod(mulmod(mload(0x6e0), mload(0x1d40), f_q), result, f_q)
mstore(11040, result)
        }
mstore(0x2b40, mulmod(mload(0x2b20), mload(0x21e0), f_q))
mstore(0x2b60, mulmod(sub(f_q, mload(0x2b40)), 1, f_q))
mstore(0x2b80, mulmod(mload(0x2b00), 1, f_q))
{
            let result := mulmod(mload(0x700), mload(0x1c80), f_q)
result := addmod(mulmod(mload(0x720), mload(0x1ce0), f_q), result, f_q)
result := addmod(mulmod(mload(0x740), mload(0x1d40), f_q), result, f_q)
mstore(11168, result)
        }
mstore(0x2bc0, mulmod(mload(0x2ba0), mload(0x21e0), f_q))
mstore(0x2be0, mulmod(sub(f_q, mload(0x2bc0)), mload(0x820), f_q))
mstore(0x2c00, mulmod(mload(0x2b00), mload(0x820), f_q))
mstore(0x2c20, addmod(mload(0x2b60), mload(0x2be0), f_q))
{
            let result := mulmod(mload(0x760), mload(0x1c80), f_q)
result := addmod(mulmod(mload(0x780), mload(0x1ce0), f_q), result, f_q)
result := addmod(mulmod(mload(0x7a0), mload(0x1d40), f_q), result, f_q)
mstore(11328, result)
        }
mstore(0x2c60, mulmod(mload(0x2c40), mload(0x21e0), f_q))
mstore(0x2c80, mulmod(sub(f_q, mload(0x2c60)), mload(0x2220), f_q))
mstore(0x2ca0, mulmod(mload(0x2b00), mload(0x2220), f_q))
mstore(0x2cc0, addmod(mload(0x2c20), mload(0x2c80), f_q))
mstore(0x2ce0, mulmod(mload(0x2cc0), mload(0x880), f_q))
mstore(0x2d00, mulmod(mload(0x2b80), mload(0x880), f_q))
mstore(0x2d20, mulmod(mload(0x2c00), mload(0x880), f_q))
mstore(0x2d40, mulmod(mload(0x2ca0), mload(0x880), f_q))
mstore(0x2d60, addmod(mload(0x29a0), mload(0x2ce0), f_q))
mstore(0x2d80, mulmod(1, mload(0x2080), f_q))
{
            let result := mulmod(mload(0x7c0), mload(0x1de0), f_q)
result := addmod(mulmod(mload(0x7e0), mload(0x1e40), f_q), result, f_q)
mstore(11680, result)
        }
mstore(0x2dc0, mulmod(mload(0x2da0), mload(0x2200), f_q))
mstore(0x2de0, mulmod(sub(f_q, mload(0x2dc0)), 1, f_q))
mstore(0x2e00, mulmod(mload(0x2d80), 1, f_q))
mstore(0x2e20, mulmod(mload(0x2de0), mload(0x2340), f_q))
mstore(0x2e40, mulmod(mload(0x2e00), mload(0x2340), f_q))
mstore(0x2e60, addmod(mload(0x2d60), mload(0x2e20), f_q))
mstore(0x2e80, mulmod(1, mload(0x1c20), f_q))
mstore(0x2ea0, mulmod(1, mload(0x920), f_q))
mstore(0x2ec0, 0x0000000000000000000000000000000000000000000000000000000000000001)
                    mstore(0x2ee0, 0x0000000000000000000000000000000000000000000000000000000000000002)
mstore(0x2f00, mload(0x2e60))
success := and(eq(staticcall(gas(), 0x7, 0x2ec0, 0x60, 0x2ec0, 0x40), 1), success)
mstore(0x2f20, mload(0x2ec0))
                    mstore(0x2f40, mload(0x2ee0))
mstore(0x2f60, mload(0x120))
                    mstore(0x2f80, mload(0x140))
success := and(eq(staticcall(gas(), 0x6, 0x2f20, 0x80, 0x2f20, 0x40), 1), success)
mstore(0x2fa0, mload(0x160))
                    mstore(0x2fc0, mload(0x180))
mstore(0x2fe0, mload(0x29c0))
success := and(eq(staticcall(gas(), 0x7, 0x2fa0, 0x60, 0x2fa0, 0x40), 1), success)
mstore(0x3000, mload(0x2f20))
                    mstore(0x3020, mload(0x2f40))
mstore(0x3040, mload(0x2fa0))
                    mstore(0x3060, mload(0x2fc0))
success := and(eq(staticcall(gas(), 0x6, 0x3000, 0x80, 0x3000, 0x40), 1), success)
mstore(0x3080, mload(0x1a0))
                    mstore(0x30a0, mload(0x1c0))
mstore(0x30c0, mload(0x29e0))
success := and(eq(staticcall(gas(), 0x7, 0x3080, 0x60, 0x3080, 0x40), 1), success)
mstore(0x30e0, mload(0x3000))
                    mstore(0x3100, mload(0x3020))
mstore(0x3120, mload(0x3080))
                    mstore(0x3140, mload(0x30a0))
success := and(eq(staticcall(gas(), 0x6, 0x30e0, 0x80, 0x30e0, 0x40), 1), success)
mstore(0x3160, 0x0000000000000000000000000000000000000000000000000000000000000000)
                    mstore(0x3180, 0x0000000000000000000000000000000000000000000000000000000000000000)
mstore(0x31a0, mload(0x2a00))
success := and(eq(staticcall(gas(), 0x7, 0x3160, 0x60, 0x3160, 0x40), 1), success)
mstore(0x31c0, mload(0x30e0))
                    mstore(0x31e0, mload(0x3100))
mstore(0x3200, mload(0x3160))
                    mstore(0x3220, mload(0x3180))
success := and(eq(staticcall(gas(), 0x6, 0x31c0, 0x80, 0x31c0, 0x40), 1), success)
mstore(0x3240, 0x20375f617e49ab845410901d46917456e732f4d037f3082f6f567b4da5ffc2be)
                    mstore(0x3260, 0x0d0d577c992ddd83c6463c8fcbb897db9e6f985bcf247cfa450afe9f21c4a27b)
mstore(0x3280, mload(0x2a20))
success := and(eq(staticcall(gas(), 0x7, 0x3240, 0x60, 0x3240, 0x40), 1), success)
mstore(0x32a0, mload(0x31c0))
                    mstore(0x32c0, mload(0x31e0))
mstore(0x32e0, mload(0x3240))
                    mstore(0x3300, mload(0x3260))
success := and(eq(staticcall(gas(), 0x6, 0x32a0, 0x80, 0x32a0, 0x40), 1), success)
mstore(0x3320, 0x041a51c39c5bc50cff30ef976c7cda7a37cb5fd3251c28c1ac93d94e0ea093a9)
                    mstore(0x3340, 0x04c242de4fe438f713b52fd0aeaf91ac874bcb0a92cd4ddf3e779dd4cf36f7ce)
mstore(0x3360, mload(0x2a40))
success := and(eq(staticcall(gas(), 0x7, 0x3320, 0x60, 0x3320, 0x40), 1), success)
mstore(0x3380, mload(0x32a0))
                    mstore(0x33a0, mload(0x32c0))
mstore(0x33c0, mload(0x3320))
                    mstore(0x33e0, mload(0x3340))
success := and(eq(staticcall(gas(), 0x6, 0x3380, 0x80, 0x3380, 0x40), 1), success)
mstore(0x3400, 0x18c412fca17f879aea03580631a5dd1476337ad46a48606f787ec9b742cbbb82)
                    mstore(0x3420, 0x0ae220f8156d861559acc78f02923d5285230c8cd2a5be1d00bb8a4033b402fd)
mstore(0x3440, mload(0x2a60))
success := and(eq(staticcall(gas(), 0x7, 0x3400, 0x60, 0x3400, 0x40), 1), success)
mstore(0x3460, mload(0x3380))
                    mstore(0x3480, mload(0x33a0))
mstore(0x34a0, mload(0x3400))
                    mstore(0x34c0, mload(0x3420))
success := and(eq(staticcall(gas(), 0x6, 0x3460, 0x80, 0x3460, 0x40), 1), success)
mstore(0x34e0, 0x2e552a5a50ffd1d3ce67a25a080afd77d06984192fd1ec6521c25620c2be30ec)
                    mstore(0x3500, 0x295aa615fefab5fcecd65db4771f079f38f9d31560d246c7a9f15b0ca864e5c8)
mstore(0x3520, mload(0x2a80))
success := and(eq(staticcall(gas(), 0x7, 0x34e0, 0x60, 0x34e0, 0x40), 1), success)
mstore(0x3540, mload(0x3460))
                    mstore(0x3560, mload(0x3480))
mstore(0x3580, mload(0x34e0))
                    mstore(0x35a0, mload(0x3500))
success := and(eq(staticcall(gas(), 0x6, 0x3540, 0x80, 0x3540, 0x40), 1), success)
mstore(0x35c0, mload(0x4a0))
                    mstore(0x35e0, mload(0x4c0))
mstore(0x3600, mload(0x2aa0))
success := and(eq(staticcall(gas(), 0x7, 0x35c0, 0x60, 0x35c0, 0x40), 1), success)
mstore(0x3620, mload(0x3540))
                    mstore(0x3640, mload(0x3560))
mstore(0x3660, mload(0x35c0))
                    mstore(0x3680, mload(0x35e0))
success := and(eq(staticcall(gas(), 0x6, 0x3620, 0x80, 0x3620, 0x40), 1), success)
mstore(0x36a0, mload(0x4e0))
                    mstore(0x36c0, mload(0x500))
mstore(0x36e0, mload(0x2ac0))
success := and(eq(staticcall(gas(), 0x7, 0x36a0, 0x60, 0x36a0, 0x40), 1), success)
mstore(0x3700, mload(0x3620))
                    mstore(0x3720, mload(0x3640))
mstore(0x3740, mload(0x36a0))
                    mstore(0x3760, mload(0x36c0))
success := and(eq(staticcall(gas(), 0x6, 0x3700, 0x80, 0x3700, 0x40), 1), success)
mstore(0x3780, mload(0x400))
                    mstore(0x37a0, mload(0x420))
mstore(0x37c0, mload(0x2ae0))
success := and(eq(staticcall(gas(), 0x7, 0x3780, 0x60, 0x3780, 0x40), 1), success)
mstore(0x37e0, mload(0x3700))
                    mstore(0x3800, mload(0x3720))
mstore(0x3820, mload(0x3780))
                    mstore(0x3840, mload(0x37a0))
success := and(eq(staticcall(gas(), 0x6, 0x37e0, 0x80, 0x37e0, 0x40), 1), success)
mstore(0x3860, mload(0x300))
                    mstore(0x3880, mload(0x320))
mstore(0x38a0, mload(0x2d00))
success := and(eq(staticcall(gas(), 0x7, 0x3860, 0x60, 0x3860, 0x40), 1), success)
mstore(0x38c0, mload(0x37e0))
                    mstore(0x38e0, mload(0x3800))
mstore(0x3900, mload(0x3860))
                    mstore(0x3920, mload(0x3880))
success := and(eq(staticcall(gas(), 0x6, 0x38c0, 0x80, 0x38c0, 0x40), 1), success)
mstore(0x3940, mload(0x340))
                    mstore(0x3960, mload(0x360))
mstore(0x3980, mload(0x2d20))
success := and(eq(staticcall(gas(), 0x7, 0x3940, 0x60, 0x3940, 0x40), 1), success)
mstore(0x39a0, mload(0x38c0))
                    mstore(0x39c0, mload(0x38e0))
mstore(0x39e0, mload(0x3940))
                    mstore(0x3a00, mload(0x3960))
success := and(eq(staticcall(gas(), 0x6, 0x39a0, 0x80, 0x39a0, 0x40), 1), success)
mstore(0x3a20, mload(0x380))
                    mstore(0x3a40, mload(0x3a0))
mstore(0x3a60, mload(0x2d40))
success := and(eq(staticcall(gas(), 0x7, 0x3a20, 0x60, 0x3a20, 0x40), 1), success)
mstore(0x3a80, mload(0x39a0))
                    mstore(0x3aa0, mload(0x39c0))
mstore(0x3ac0, mload(0x3a20))
                    mstore(0x3ae0, mload(0x3a40))
success := and(eq(staticcall(gas(), 0x6, 0x3a80, 0x80, 0x3a80, 0x40), 1), success)
mstore(0x3b00, mload(0x3c0))
                    mstore(0x3b20, mload(0x3e0))
mstore(0x3b40, mload(0x2e40))
success := and(eq(staticcall(gas(), 0x7, 0x3b00, 0x60, 0x3b00, 0x40), 1), success)
mstore(0x3b60, mload(0x3a80))
                    mstore(0x3b80, mload(0x3aa0))
mstore(0x3ba0, mload(0x3b00))
                    mstore(0x3bc0, mload(0x3b20))
success := and(eq(staticcall(gas(), 0x6, 0x3b60, 0x80, 0x3b60, 0x40), 1), success)
mstore(0x3be0, mload(0x8c0))
                    mstore(0x3c00, mload(0x8e0))
mstore(0x3c20, sub(f_q, mload(0x2e80)))
success := and(eq(staticcall(gas(), 0x7, 0x3be0, 0x60, 0x3be0, 0x40), 1), success)
mstore(0x3c40, mload(0x3b60))
                    mstore(0x3c60, mload(0x3b80))
mstore(0x3c80, mload(0x3be0))
                    mstore(0x3ca0, mload(0x3c00))
success := and(eq(staticcall(gas(), 0x6, 0x3c40, 0x80, 0x3c40, 0x40), 1), success)
mstore(0x3cc0, mload(0x960))
                    mstore(0x3ce0, mload(0x980))
mstore(0x3d00, mload(0x2ea0))
success := and(eq(staticcall(gas(), 0x7, 0x3cc0, 0x60, 0x3cc0, 0x40), 1), success)
mstore(0x3d20, mload(0x3c40))
                    mstore(0x3d40, mload(0x3c60))
mstore(0x3d60, mload(0x3cc0))
                    mstore(0x3d80, mload(0x3ce0))
success := and(eq(staticcall(gas(), 0x6, 0x3d20, 0x80, 0x3d20, 0x40), 1), success)
mstore(0x3da0, mload(0x3d20))
                    mstore(0x3dc0, mload(0x3d40))
mstore(0x3de0, 0x198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2)
            mstore(0x3e00, 0x1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed)
            mstore(0x3e20, 0x090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b)
            mstore(0x3e40, 0x12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa)
mstore(0x3e60, mload(0x960))
                    mstore(0x3e80, mload(0x980))
mstore(0x3ea0, 0x0181624e80f3d6ae28df7e01eaeab1c0e919877a3b8a6b7fbc69a6817d596ea2)
            mstore(0x3ec0, 0x1783d30dcb12d259bb89098addf6280fa4b653be7a152542a28f7b926e27e648)
            mstore(0x3ee0, 0x00ae44489d41a0d179e2dfdc03bddd883b7109f8b6ae316a59e815c1a6b35304)
            mstore(0x3f00, 0x0b2147ab62a386bd63e6de1522109b8c9588ab466f5aadfde8c41ca3749423ee)
success := and(eq(staticcall(gas(), 0x8, 0x3da0, 0x180, 0x3da0, 0x20), 1), success)
success := and(eq(mload(0x3da0), 1), success)

            // Revert if anything fails
            if iszero(success) { revert(0, 0) }

            // Return empty bytes on success
            return(0, 0)

        }
    }
}
        