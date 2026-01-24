
        object "plonk_verifier" {
            code {
                function allocate(size) -> ptr {
                    ptr := mload(0x40)
                    if eq(ptr, 0) { ptr := 0x60 }
                    mstore(0x40, add(ptr, size))
                }
                let size := datasize("Runtime")
                let offset := allocate(size)
                datacopy(offset, dataoffset("Runtime"), size)
                return(offset, size)
            }
            object "Runtime" {
                code {
                    let success:bool := true
                    let f_p := 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47
                    let f_q := 0x30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001
                    function validate_ec_point(x, y) -> valid:bool {
                        {
                            let x_lt_p:bool := lt(x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                            let y_lt_p:bool := lt(y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                            valid := and(x_lt_p, y_lt_p)
                        }
                        {
                            let x_is_zero:bool := eq(x, 0)
                            let y_is_zero:bool := eq(y, 0)
                            let x_or_y_is_zero:bool := or(x_is_zero, y_is_zero)
                            let x_and_y_is_not_zero:bool := not(x_or_y_is_zero)
                            valid := and(x_and_y_is_not_zero, valid)
                        }
                        {
                            let y_square := mulmod(y, y, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                            let x_square := mulmod(x, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                            let x_cube := mulmod(x_square, x, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                            let x_cube_plus_3 := addmod(x_cube, 3, 0x30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47)
                            let y_square_eq_x_cube_plus_3:bool := eq(x_cube_plus_3, y_square)
                            valid := and(y_square_eq_x_cube_plus_3, valid)
                        }
                    }
                    mstore(0x20, mod(calldataload(0x0), f_q))
mstore(0x40, mod(calldataload(0x20), f_q))
mstore(0x60, mod(calldataload(0x40), f_q))
mstore(0x80, mod(calldataload(0x60), f_q))
mstore(0x0, 4987935197635414493554277576043034400217952935733177414011355009708761610180)

        {
            let x := calldataload(0x80)
            mstore(0xa0, x)
            let y := calldataload(0xa0)
            mstore(0xc0, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0xc0)
            mstore(0xe0, x)
            let y := calldataload(0xe0)
            mstore(0x100, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x100)
            mstore(0x120, x)
            let y := calldataload(0x120)
            mstore(0x140, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x160, keccak256(0x0, 352))
{
            let hash := mload(0x160)
            mstore(0x180, mod(hash, f_q))
            mstore(0x1a0, hash)
        }
mstore8(448, 1)
mstore(0x1c0, keccak256(0x1a0, 33))
{
            let hash := mload(0x1c0)
            mstore(0x1e0, mod(hash, f_q))
            mstore(0x200, hash)
        }
mstore8(544, 1)
mstore(0x220, keccak256(0x200, 33))
{
            let hash := mload(0x220)
            mstore(0x240, mod(hash, f_q))
            mstore(0x260, hash)
        }

        {
            let x := calldataload(0x140)
            mstore(0x280, x)
            let y := calldataload(0x160)
            mstore(0x2a0, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x180)
            mstore(0x2c0, x)
            let y := calldataload(0x1a0)
            mstore(0x2e0, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x1c0)
            mstore(0x300, x)
            let y := calldataload(0x1e0)
            mstore(0x320, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x200)
            mstore(0x340, x)
            let y := calldataload(0x220)
            mstore(0x360, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x240)
            mstore(0x380, x)
            let y := calldataload(0x260)
            mstore(0x3a0, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x3c0, keccak256(0x260, 352))
{
            let hash := mload(0x3c0)
            mstore(0x3e0, mod(hash, f_q))
            mstore(0x400, hash)
        }

        {
            let x := calldataload(0x280)
            mstore(0x420, x)
            let y := calldataload(0x2a0)
            mstore(0x440, y)
            success := and(validate_ec_point(x, y), success)
        }

        {
            let x := calldataload(0x2c0)
            mstore(0x460, x)
            let y := calldataload(0x2e0)
            mstore(0x480, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x4a0, keccak256(0x400, 160))
{
            let hash := mload(0x4a0)
            mstore(0x4c0, mod(hash, f_q))
            mstore(0x4e0, hash)
        }
mstore(0x500, mod(calldataload(0x300), f_q))
mstore(0x520, mod(calldataload(0x320), f_q))
mstore(0x540, mod(calldataload(0x340), f_q))
mstore(0x560, mod(calldataload(0x360), f_q))
mstore(0x580, mod(calldataload(0x380), f_q))
mstore(0x5a0, mod(calldataload(0x3a0), f_q))
mstore(0x5c0, mod(calldataload(0x3c0), f_q))
mstore(0x5e0, mod(calldataload(0x3e0), f_q))
mstore(0x600, mod(calldataload(0x400), f_q))
mstore(0x620, mod(calldataload(0x420), f_q))
mstore(0x640, mod(calldataload(0x440), f_q))
mstore(0x660, mod(calldataload(0x460), f_q))
mstore(0x680, mod(calldataload(0x480), f_q))
mstore(0x6a0, mod(calldataload(0x4a0), f_q))
mstore(0x6c0, mod(calldataload(0x4c0), f_q))
mstore(0x6e0, mod(calldataload(0x4e0), f_q))
mstore(0x700, mod(calldataload(0x500), f_q))
mstore(0x720, mod(calldataload(0x520), f_q))
mstore(0x740, mod(calldataload(0x540), f_q))
mstore(0x760, mod(calldataload(0x560), f_q))
mstore(0x780, keccak256(0x4e0, 672))
{
            let hash := mload(0x780)
            mstore(0x7a0, mod(hash, f_q))
            mstore(0x7c0, hash)
        }
mstore8(2016, 1)
mstore(0x7e0, keccak256(0x7c0, 33))
{
            let hash := mload(0x7e0)
            mstore(0x800, mod(hash, f_q))
            mstore(0x820, hash)
        }

        {
            let x := calldataload(0x580)
            mstore(0x840, x)
            let y := calldataload(0x5a0)
            mstore(0x860, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x880, keccak256(0x820, 96))
{
            let hash := mload(0x880)
            mstore(0x8a0, mod(hash, f_q))
            mstore(0x8c0, hash)
        }

        {
            let x := calldataload(0x5c0)
            mstore(0x8e0, x)
            let y := calldataload(0x5e0)
            mstore(0x900, y)
            success := and(validate_ec_point(x, y), success)
        }
mstore(0x920, mulmod(mload(0x4c0), mload(0x4c0), f_q))
mstore(0x940, mulmod(mload(0x920), mload(0x920), f_q))
mstore(0x960, mulmod(mload(0x940), mload(0x940), f_q))
mstore(0x980, mulmod(mload(0x960), mload(0x960), f_q))
mstore(0x9a0, mulmod(mload(0x980), mload(0x980), f_q))
mstore(0x9c0, mulmod(mload(0x9a0), mload(0x9a0), f_q))
mstore(0x9e0, mulmod(mload(0x9c0), mload(0x9c0), f_q))
mstore(0xa00, mulmod(mload(0x9e0), mload(0x9e0), f_q))
mstore(0xa20, mulmod(mload(0xa00), mload(0xa00), f_q))
mstore(0xa40, mulmod(mload(0xa20), mload(0xa20), f_q))
mstore(0xa60, addmod(mload(0xa40), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q))
mstore(0xa80, mulmod(mload(0xa60), 21866867634659744680037180739646672280844703888306253060159436409049855557633, f_q))
mstore(0xaa0, mulmod(mload(0xa80), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
mstore(0xac0, addmod(mload(0x4c0), 11952173244228085703417150075019950819261217979144509790385672149647937438939, f_q))
mstore(0xae0, mulmod(mload(0xa80), 1680739780407307830605919050682431078078760076686599579086116998224280619988, f_q))
mstore(0xb00, addmod(mload(0x4c0), 20207503091431967391640486694574844010469604323729434764612087188351527875629, f_q))
mstore(0xb20, mulmod(mload(0xa80), 14158528901797138466244491986759313854666262535363044392173788062030301470987, f_q))
mstore(0xb40, addmod(mload(0x4c0), 7729713970042136756001913758497961233882101865052989951524416124545507024630, f_q))
mstore(0xb60, mulmod(mload(0xa80), 15699029810934084314820646074566828280617789951162923449200398535581206172418, f_q))
mstore(0xb80, addmod(mload(0x4c0), 6189213060905190907425759670690446807930574449253110894497805650994602323199, f_q))
mstore(0xba0, mulmod(mload(0xa80), 4260969412351770314333984243767775737437927068151180798236715529158398853173, f_q))
mstore(0xbc0, addmod(mload(0x4c0), 17627273459487504907912421501489499351110437332264853545461488657417409642444, f_q))
mstore(0xbe0, mulmod(mload(0xa80), 4925592601992654644734291590386747644864797672605745962807370354577123815907, f_q))
mstore(0xc00, addmod(mload(0x4c0), 16962650269846620577512114154870527443683566727810288380890833831998684679710, f_q))
mstore(0xc20, mulmod(mload(0xa80), 1, f_q))
mstore(0xc40, addmod(mload(0x4c0), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q))
mstore(0xc60, mulmod(mload(0xa80), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
mstore(0xc80, addmod(mload(0x4c0), 2507682784038009475131574039120954579123549720846755509306663987687515178116, f_q))
mstore(0xca0, mulmod(mload(0xa80), 6252951856119339508807713076978770803512896272623217303779254502899773638908, f_q))
mstore(0xcc0, addmod(mload(0x4c0), 15635291015719935713438692668278504285035468127792817039918949683676034856709, f_q))
mstore(0xce0, mulmod(mload(0xa80), 15554008185779528788857340196607833777388478343360168149406749724843247080062, f_q))
mstore(0xd00, addmod(mload(0x4c0), 6334234686059746433389065548649441311159886057055866194291454461732561415555, f_q))
{
            let prod := mload(0xac0)

                prod := mulmod(mload(0xb00), prod, f_q)
                mstore(0xd20, prod)
            
                prod := mulmod(mload(0xb40), prod, f_q)
                mstore(0xd40, prod)
            
                prod := mulmod(mload(0xb80), prod, f_q)
                mstore(0xd60, prod)
            
                prod := mulmod(mload(0xbc0), prod, f_q)
                mstore(0xd80, prod)
            
                prod := mulmod(mload(0xc00), prod, f_q)
                mstore(0xda0, prod)
            
                prod := mulmod(mload(0xc40), prod, f_q)
                mstore(0xdc0, prod)
            
                prod := mulmod(mload(0xc80), prod, f_q)
                mstore(0xde0, prod)
            
                prod := mulmod(mload(0xcc0), prod, f_q)
                mstore(0xe00, prod)
            
                prod := mulmod(mload(0xd00), prod, f_q)
                mstore(0xe20, prod)
            
                prod := mulmod(mload(0xa60), prod, f_q)
                mstore(0xe40, prod)
            
        }
mstore(0xe80, 32)
mstore(0xea0, 32)
mstore(0xec0, 32)
mstore(0xee0, mload(0xe40))
mstore(0xf00, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0xf20, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0xe80, 0xc0, 0xe60, 0x20), 1), success)
{
            
            let inv := mload(0xe60)
            let v
        
                    v := mload(0xa60)
                    mstore(2656, mulmod(mload(0xe20), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xd00)
                    mstore(3328, mulmod(mload(0xe00), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xcc0)
                    mstore(3264, mulmod(mload(0xde0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc80)
                    mstore(3200, mulmod(mload(0xdc0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc40)
                    mstore(3136, mulmod(mload(0xda0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xc00)
                    mstore(3072, mulmod(mload(0xd80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xbc0)
                    mstore(3008, mulmod(mload(0xd60), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xb80)
                    mstore(2944, mulmod(mload(0xd40), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xb40)
                    mstore(2880, mulmod(mload(0xd20), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0xb00)
                    mstore(2816, mulmod(mload(0xac0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0xac0, inv)

        }
mstore(0xf40, mulmod(mload(0xaa0), mload(0xac0), f_q))
mstore(0xf60, mulmod(mload(0xae0), mload(0xb00), f_q))
mstore(0xf80, mulmod(mload(0xb20), mload(0xb40), f_q))
mstore(0xfa0, mulmod(mload(0xb60), mload(0xb80), f_q))
mstore(0xfc0, mulmod(mload(0xba0), mload(0xbc0), f_q))
mstore(0xfe0, mulmod(mload(0xbe0), mload(0xc00), f_q))
mstore(0x1000, mulmod(mload(0xc20), mload(0xc40), f_q))
mstore(0x1020, mulmod(mload(0xc60), mload(0xc80), f_q))
mstore(0x1040, mulmod(mload(0xca0), mload(0xcc0), f_q))
mstore(0x1060, mulmod(mload(0xce0), mload(0xd00), f_q))
{
            let result := mulmod(mload(0x1000), mload(0x20), f_q)
result := addmod(mulmod(mload(0x1020), mload(0x40), f_q), result, f_q)
result := addmod(mulmod(mload(0x1040), mload(0x60), f_q), result, f_q)
result := addmod(mulmod(mload(0x1060), mload(0x80), f_q), result, f_q)
mstore(4224, result)
        }
mstore(0x10a0, addmod(1, sub(f_q, mload(0x620)), f_q))
mstore(0x10c0, mulmod(mload(0x10a0), mload(0x1000), f_q))
mstore(0x10e0, mulmod(mload(0x3e0), mload(0x10c0), f_q))
mstore(0x1100, mulmod(mload(0x740), mload(0x740), f_q))
mstore(0x1120, addmod(mload(0x1100), sub(f_q, mload(0x740)), f_q))
mstore(0x1140, mulmod(mload(0x1120), mload(0xf40), f_q))
mstore(0x1160, addmod(mload(0x10e0), mload(0x1140), f_q))
mstore(0x1180, mulmod(mload(0x3e0), mload(0x1160), f_q))
mstore(0x11a0, addmod(mload(0x680), sub(f_q, mload(0x660)), f_q))
mstore(0x11c0, mulmod(mload(0x11a0), mload(0x1000), f_q))
mstore(0x11e0, addmod(mload(0x1180), mload(0x11c0), f_q))
mstore(0x1200, mulmod(mload(0x3e0), mload(0x11e0), f_q))
mstore(0x1220, addmod(mload(0x6e0), sub(f_q, mload(0x6c0)), f_q))
mstore(0x1240, mulmod(mload(0x1220), mload(0x1000), f_q))
mstore(0x1260, addmod(mload(0x1200), mload(0x1240), f_q))
mstore(0x1280, mulmod(mload(0x3e0), mload(0x1260), f_q))
mstore(0x12a0, addmod(mload(0x740), sub(f_q, mload(0x720)), f_q))
mstore(0x12c0, mulmod(mload(0x12a0), mload(0x1000), f_q))
mstore(0x12e0, addmod(mload(0x1280), mload(0x12c0), f_q))
mstore(0x1300, mulmod(mload(0x3e0), mload(0x12e0), f_q))
mstore(0x1320, addmod(1, sub(f_q, mload(0xf40)), f_q))
mstore(0x1340, addmod(mload(0xf60), mload(0xf80), f_q))
mstore(0x1360, addmod(mload(0x1340), mload(0xfa0), f_q))
mstore(0x1380, addmod(mload(0x1360), mload(0xfc0), f_q))
mstore(0x13a0, addmod(mload(0x1380), mload(0xfe0), f_q))
mstore(0x13c0, addmod(mload(0x1320), sub(f_q, mload(0x13a0)), f_q))
mstore(0x13e0, mulmod(mload(0x5a0), mload(0x1e0), f_q))
mstore(0x1400, addmod(mload(0x1080), mload(0x13e0), f_q))
mstore(0x1420, addmod(mload(0x1400), mload(0x240), f_q))
mstore(0x1440, mulmod(mload(0x1420), mload(0x640), f_q))
mstore(0x1460, mulmod(1, mload(0x1e0), f_q))
mstore(0x1480, mulmod(mload(0x4c0), mload(0x1460), f_q))
mstore(0x14a0, addmod(mload(0x1080), mload(0x1480), f_q))
mstore(0x14c0, addmod(mload(0x14a0), mload(0x240), f_q))
mstore(0x14e0, mulmod(mload(0x14c0), mload(0x620), f_q))
mstore(0x1500, addmod(mload(0x1440), sub(f_q, mload(0x14e0)), f_q))
mstore(0x1520, mulmod(mload(0x1500), mload(0x13c0), f_q))
mstore(0x1540, addmod(mload(0x1300), mload(0x1520), f_q))
mstore(0x1560, mulmod(mload(0x3e0), mload(0x1540), f_q))
mstore(0x1580, mulmod(mload(0x5c0), mload(0x1e0), f_q))
mstore(0x15a0, addmod(mload(0x500), mload(0x1580), f_q))
mstore(0x15c0, addmod(mload(0x15a0), mload(0x240), f_q))
mstore(0x15e0, mulmod(mload(0x15c0), mload(0x6a0), f_q))
mstore(0x1600, mulmod(4131629893567559867359510883348571134090853742863529169391034518566172092834, mload(0x1e0), f_q))
mstore(0x1620, mulmod(mload(0x4c0), mload(0x1600), f_q))
mstore(0x1640, addmod(mload(0x500), mload(0x1620), f_q))
mstore(0x1660, addmod(mload(0x1640), mload(0x240), f_q))
mstore(0x1680, mulmod(mload(0x1660), mload(0x680), f_q))
mstore(0x16a0, addmod(mload(0x15e0), sub(f_q, mload(0x1680)), f_q))
mstore(0x16c0, mulmod(mload(0x16a0), mload(0x13c0), f_q))
mstore(0x16e0, addmod(mload(0x1560), mload(0x16c0), f_q))
mstore(0x1700, mulmod(mload(0x3e0), mload(0x16e0), f_q))
mstore(0x1720, mulmod(mload(0x5e0), mload(0x1e0), f_q))
mstore(0x1740, addmod(mload(0x520), mload(0x1720), f_q))
mstore(0x1760, addmod(mload(0x1740), mload(0x240), f_q))
mstore(0x1780, mulmod(mload(0x1760), mload(0x700), f_q))
mstore(0x17a0, mulmod(8910878055287538404433155982483128285667088683464058436815641868457422632747, mload(0x1e0), f_q))
mstore(0x17c0, mulmod(mload(0x4c0), mload(0x17a0), f_q))
mstore(0x17e0, addmod(mload(0x520), mload(0x17c0), f_q))
mstore(0x1800, addmod(mload(0x17e0), mload(0x240), f_q))
mstore(0x1820, mulmod(mload(0x1800), mload(0x6e0), f_q))
mstore(0x1840, addmod(mload(0x1780), sub(f_q, mload(0x1820)), f_q))
mstore(0x1860, mulmod(mload(0x1840), mload(0x13c0), f_q))
mstore(0x1880, addmod(mload(0x1700), mload(0x1860), f_q))
mstore(0x18a0, mulmod(mload(0x3e0), mload(0x1880), f_q))
mstore(0x18c0, mulmod(mload(0x600), mload(0x1e0), f_q))
mstore(0x18e0, addmod(mload(0x540), mload(0x18c0), f_q))
mstore(0x1900, addmod(mload(0x18e0), mload(0x240), f_q))
mstore(0x1920, mulmod(mload(0x1900), mload(0x760), f_q))
mstore(0x1940, mulmod(11166246659983828508719468090013646171463329086121580628794302409516816350802, mload(0x1e0), f_q))
mstore(0x1960, mulmod(mload(0x4c0), mload(0x1940), f_q))
mstore(0x1980, addmod(mload(0x540), mload(0x1960), f_q))
mstore(0x19a0, addmod(mload(0x1980), mload(0x240), f_q))
mstore(0x19c0, mulmod(mload(0x19a0), mload(0x740), f_q))
mstore(0x19e0, addmod(mload(0x1920), sub(f_q, mload(0x19c0)), f_q))
mstore(0x1a00, mulmod(mload(0x19e0), mload(0x13c0), f_q))
mstore(0x1a20, addmod(mload(0x18a0), mload(0x1a00), f_q))
mstore(0x1a40, mulmod(mload(0xa40), mload(0xa40), f_q))
mstore(0x1a60, mulmod(1, mload(0xa40), f_q))
mstore(0x1a80, mulmod(mload(0x1a20), mload(0xa60), f_q))
mstore(0x1aa0, mulmod(mload(0x4c0), 1, f_q))
mstore(0x1ac0, addmod(mload(0x8a0), sub(f_q, mload(0x1aa0)), f_q))
mstore(0x1ae0, mulmod(mload(0x4c0), 9936069627611189518829255670237324269287146421271524553312532036927871056678, f_q))
mstore(0x1b00, addmod(mload(0x8a0), sub(f_q, mload(0x1ae0)), f_q))
mstore(0x1b20, mulmod(mload(0x4c0), 19380560087801265747114831706136320509424814679569278834391540198888293317501, f_q))
mstore(0x1b40, addmod(mload(0x8a0), sub(f_q, mload(0x1b20)), f_q))
{
            let result := mulmod(mload(0x8a0), 1, f_q)
result := addmod(mulmod(mload(0x4c0), 21888242871839275222246405745257275088548364400416034343698204186575808495616, f_q), result, f_q)
mstore(7008, result)
        }
mstore(0x1b80, mulmod(1, mload(0x1ac0), f_q))
{
            let result := mulmod(mload(0x8a0), 16140595808673403009154643164823336476463527776677864878778453135559733237044, f_q)
result := addmod(mulmod(mload(0x4c0), 5747647063165872213091762580433938612084836623738169464919751051016075258573, f_q), result, f_q)
mstore(7072, result)
        }
{
            let result := mulmod(mload(0x8a0), 17015964487361230672162623735654618573844832338054897787312333529290879253714, f_q)
result := addmod(mulmod(mload(0x4c0), 3176732791729641355588945816447819802711920387939493967460175841862547409845, f_q), result, f_q)
mstore(7104, result)
        }
{
            let result := mulmod(mload(0x8a0), 19187508498431587163140984396833674282302409422288044257471288693049179355069, f_q)
result := addmod(mulmod(mload(0x4c0), 17472297497993506786357772047295541913219081564856233764575529621311665103799, f_q), result, f_q)
mstore(7136, result)
        }
mstore(0x1c00, mulmod(mload(0x1b80), mload(0x1b40), f_q))
mstore(0x1c20, mulmod(mload(0x1c00), mload(0x1b00), f_q))
{
            let result := mulmod(mload(0x8a0), 2507682784038009475131574039120954579123549720846755509306663987687515178117, f_q)
result := addmod(mulmod(mload(0x4c0), 19380560087801265747114831706136320509424814679569278834391540198888293317500, f_q), result, f_q)
mstore(7232, result)
        }
{
            let result := mulmod(mload(0x8a0), 19380560087801265747114831706136320509424814679569278834391540198888293317500, f_q)
result := addmod(mulmod(mload(0x4c0), 13127608231681926238307118629157549705911918406946061530612285695988519678593, f_q), result, f_q)
mstore(7264, result)
        }
{
            let prod := mload(0x1b60)

                prod := mulmod(mload(0x1ba0), prod, f_q)
                mstore(0x1c80, prod)
            
                prod := mulmod(mload(0x1bc0), prod, f_q)
                mstore(0x1ca0, prod)
            
                prod := mulmod(mload(0x1be0), prod, f_q)
                mstore(0x1cc0, prod)
            
                prod := mulmod(mload(0x1c20), prod, f_q)
                mstore(0x1ce0, prod)
            
                prod := mulmod(mload(0x1c40), prod, f_q)
                mstore(0x1d00, prod)
            
                prod := mulmod(mload(0x1c60), prod, f_q)
                mstore(0x1d20, prod)
            
                prod := mulmod(mload(0x1c00), prod, f_q)
                mstore(0x1d40, prod)
            
        }
mstore(0x1d80, 32)
mstore(0x1da0, 32)
mstore(0x1dc0, 32)
mstore(0x1de0, mload(0x1d40))
mstore(0x1e00, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0x1e20, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0x1d80, 0xc0, 0x1d60, 0x20), 1), success)
{
            
            let inv := mload(0x1d60)
            let v
        
                    v := mload(0x1c00)
                    mstore(7168, mulmod(mload(0x1d20), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1c60)
                    mstore(7264, mulmod(mload(0x1d00), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1c40)
                    mstore(7232, mulmod(mload(0x1ce0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1c20)
                    mstore(7200, mulmod(mload(0x1cc0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1be0)
                    mstore(7136, mulmod(mload(0x1ca0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1bc0)
                    mstore(7104, mulmod(mload(0x1c80), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1ba0)
                    mstore(7072, mulmod(mload(0x1b60), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0x1b60, inv)

        }
{
            let result := mload(0x1b60)
mstore(7744, result)
        }
mstore(0x1e60, mulmod(mload(0x1b80), mload(0x1c20), f_q))
{
            let result := mload(0x1ba0)
result := addmod(mload(0x1bc0), result, f_q)
result := addmod(mload(0x1be0), result, f_q)
mstore(7808, result)
        }
mstore(0x1ea0, mulmod(mload(0x1b80), mload(0x1c00), f_q))
{
            let result := mload(0x1c40)
result := addmod(mload(0x1c60), result, f_q)
mstore(7872, result)
        }
{
            let prod := mload(0x1e40)

                prod := mulmod(mload(0x1e80), prod, f_q)
                mstore(0x1ee0, prod)
            
                prod := mulmod(mload(0x1ec0), prod, f_q)
                mstore(0x1f00, prod)
            
        }
mstore(0x1f40, 32)
mstore(0x1f60, 32)
mstore(0x1f80, 32)
mstore(0x1fa0, mload(0x1f00))
mstore(0x1fc0, 21888242871839275222246405745257275088548364400416034343698204186575808495615)
mstore(0x1fe0, 21888242871839275222246405745257275088548364400416034343698204186575808495617)
success := and(eq(staticcall(gas(), 0x5, 0x1f40, 0xc0, 0x1f20, 0x20), 1), success)
{
            
            let inv := mload(0x1f20)
            let v
        
                    v := mload(0x1ec0)
                    mstore(7872, mulmod(mload(0x1ee0), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                
                    v := mload(0x1e80)
                    mstore(7808, mulmod(mload(0x1e40), inv, f_q))
                    inv := mulmod(v, inv, f_q)
                mstore(0x1e40, inv)

        }
mstore(0x2000, mulmod(mload(0x1e60), mload(0x1e80), f_q))
mstore(0x2020, mulmod(mload(0x1ea0), mload(0x1ec0), f_q))
mstore(0x2040, mulmod(mload(0x7a0), mload(0x7a0), f_q))
mstore(0x2060, mulmod(mload(0x2040), mload(0x7a0), f_q))
mstore(0x2080, mulmod(mload(0x2060), mload(0x7a0), f_q))
mstore(0x20a0, mulmod(mload(0x2080), mload(0x7a0), f_q))
mstore(0x20c0, mulmod(mload(0x20a0), mload(0x7a0), f_q))
mstore(0x20e0, mulmod(mload(0x20c0), mload(0x7a0), f_q))
mstore(0x2100, mulmod(mload(0x20e0), mload(0x7a0), f_q))
mstore(0x2120, mulmod(mload(0x2100), mload(0x7a0), f_q))
mstore(0x2140, mulmod(mload(0x2120), mload(0x7a0), f_q))
mstore(0x2160, mulmod(mload(0x800), mload(0x800), f_q))
mstore(0x2180, mulmod(mload(0x2160), mload(0x800), f_q))
{
            let result := mulmod(mload(0x500), mload(0x1b60), f_q)
mstore(8608, result)
        }
mstore(0x21c0, mulmod(mload(0x21a0), mload(0x1e40), f_q))
mstore(0x21e0, mulmod(sub(f_q, mload(0x21c0)), 1, f_q))
{
            let result := mulmod(mload(0x520), mload(0x1b60), f_q)
mstore(8704, result)
        }
mstore(0x2220, mulmod(mload(0x2200), mload(0x1e40), f_q))
mstore(0x2240, mulmod(sub(f_q, mload(0x2220)), mload(0x7a0), f_q))
mstore(0x2260, mulmod(1, mload(0x7a0), f_q))
mstore(0x2280, addmod(mload(0x21e0), mload(0x2240), f_q))
{
            let result := mulmod(mload(0x540), mload(0x1b60), f_q)
mstore(8864, result)
        }
mstore(0x22c0, mulmod(mload(0x22a0), mload(0x1e40), f_q))
mstore(0x22e0, mulmod(sub(f_q, mload(0x22c0)), mload(0x2040), f_q))
mstore(0x2300, mulmod(1, mload(0x2040), f_q))
mstore(0x2320, addmod(mload(0x2280), mload(0x22e0), f_q))
{
            let result := mulmod(mload(0x560), mload(0x1b60), f_q)
mstore(9024, result)
        }
mstore(0x2360, mulmod(mload(0x2340), mload(0x1e40), f_q))
mstore(0x2380, mulmod(sub(f_q, mload(0x2360)), mload(0x2060), f_q))
mstore(0x23a0, mulmod(1, mload(0x2060), f_q))
mstore(0x23c0, addmod(mload(0x2320), mload(0x2380), f_q))
{
            let result := mulmod(mload(0x5a0), mload(0x1b60), f_q)
mstore(9184, result)
        }
mstore(0x2400, mulmod(mload(0x23e0), mload(0x1e40), f_q))
mstore(0x2420, mulmod(sub(f_q, mload(0x2400)), mload(0x2080), f_q))
mstore(0x2440, mulmod(1, mload(0x2080), f_q))
mstore(0x2460, addmod(mload(0x23c0), mload(0x2420), f_q))
{
            let result := mulmod(mload(0x5c0), mload(0x1b60), f_q)
mstore(9344, result)
        }
mstore(0x24a0, mulmod(mload(0x2480), mload(0x1e40), f_q))
mstore(0x24c0, mulmod(sub(f_q, mload(0x24a0)), mload(0x20a0), f_q))
mstore(0x24e0, mulmod(1, mload(0x20a0), f_q))
mstore(0x2500, addmod(mload(0x2460), mload(0x24c0), f_q))
{
            let result := mulmod(mload(0x5e0), mload(0x1b60), f_q)
mstore(9504, result)
        }
mstore(0x2540, mulmod(mload(0x2520), mload(0x1e40), f_q))
mstore(0x2560, mulmod(sub(f_q, mload(0x2540)), mload(0x20c0), f_q))
mstore(0x2580, mulmod(1, mload(0x20c0), f_q))
mstore(0x25a0, addmod(mload(0x2500), mload(0x2560), f_q))
{
            let result := mulmod(mload(0x600), mload(0x1b60), f_q)
mstore(9664, result)
        }
mstore(0x25e0, mulmod(mload(0x25c0), mload(0x1e40), f_q))
mstore(0x2600, mulmod(sub(f_q, mload(0x25e0)), mload(0x20e0), f_q))
mstore(0x2620, mulmod(1, mload(0x20e0), f_q))
mstore(0x2640, addmod(mload(0x25a0), mload(0x2600), f_q))
{
            let result := mulmod(mload(0x1a80), mload(0x1b60), f_q)
mstore(9824, result)
        }
mstore(0x2680, mulmod(mload(0x2660), mload(0x1e40), f_q))
mstore(0x26a0, mulmod(sub(f_q, mload(0x2680)), mload(0x2100), f_q))
mstore(0x26c0, mulmod(1, mload(0x2100), f_q))
mstore(0x26e0, mulmod(mload(0x1a60), mload(0x2100), f_q))
mstore(0x2700, addmod(mload(0x2640), mload(0x26a0), f_q))
{
            let result := mulmod(mload(0x580), mload(0x1b60), f_q)
mstore(10016, result)
        }
mstore(0x2740, mulmod(mload(0x2720), mload(0x1e40), f_q))
mstore(0x2760, mulmod(sub(f_q, mload(0x2740)), mload(0x2120), f_q))
mstore(0x2780, mulmod(1, mload(0x2120), f_q))
mstore(0x27a0, addmod(mload(0x2700), mload(0x2760), f_q))
mstore(0x27c0, mulmod(mload(0x27a0), 1, f_q))
mstore(0x27e0, mulmod(mload(0x2260), 1, f_q))
mstore(0x2800, mulmod(mload(0x2300), 1, f_q))
mstore(0x2820, mulmod(mload(0x23a0), 1, f_q))
mstore(0x2840, mulmod(mload(0x2440), 1, f_q))
mstore(0x2860, mulmod(mload(0x24e0), 1, f_q))
mstore(0x2880, mulmod(mload(0x2580), 1, f_q))
mstore(0x28a0, mulmod(mload(0x2620), 1, f_q))
mstore(0x28c0, mulmod(mload(0x26c0), 1, f_q))
mstore(0x28e0, mulmod(mload(0x26e0), 1, f_q))
mstore(0x2900, mulmod(mload(0x2780), 1, f_q))
mstore(0x2920, mulmod(1, mload(0x1e60), f_q))
{
            let result := mulmod(mload(0x620), mload(0x1ba0), f_q)
result := addmod(mulmod(mload(0x640), mload(0x1bc0), f_q), result, f_q)
result := addmod(mulmod(mload(0x660), mload(0x1be0), f_q), result, f_q)
mstore(10560, result)
        }
mstore(0x2960, mulmod(mload(0x2940), mload(0x2000), f_q))
mstore(0x2980, mulmod(sub(f_q, mload(0x2960)), 1, f_q))
mstore(0x29a0, mulmod(mload(0x2920), 1, f_q))
{
            let result := mulmod(mload(0x680), mload(0x1ba0), f_q)
result := addmod(mulmod(mload(0x6a0), mload(0x1bc0), f_q), result, f_q)
result := addmod(mulmod(mload(0x6c0), mload(0x1be0), f_q), result, f_q)
mstore(10688, result)
        }
mstore(0x29e0, mulmod(mload(0x29c0), mload(0x2000), f_q))
mstore(0x2a00, mulmod(sub(f_q, mload(0x29e0)), mload(0x7a0), f_q))
mstore(0x2a20, mulmod(mload(0x2920), mload(0x7a0), f_q))
mstore(0x2a40, addmod(mload(0x2980), mload(0x2a00), f_q))
{
            let result := mulmod(mload(0x6e0), mload(0x1ba0), f_q)
result := addmod(mulmod(mload(0x700), mload(0x1bc0), f_q), result, f_q)
result := addmod(mulmod(mload(0x720), mload(0x1be0), f_q), result, f_q)
mstore(10848, result)
        }
mstore(0x2a80, mulmod(mload(0x2a60), mload(0x2000), f_q))
mstore(0x2aa0, mulmod(sub(f_q, mload(0x2a80)), mload(0x2040), f_q))
mstore(0x2ac0, mulmod(mload(0x2920), mload(0x2040), f_q))
mstore(0x2ae0, addmod(mload(0x2a40), mload(0x2aa0), f_q))
mstore(0x2b00, mulmod(mload(0x2ae0), mload(0x800), f_q))
mstore(0x2b20, mulmod(mload(0x29a0), mload(0x800), f_q))
mstore(0x2b40, mulmod(mload(0x2a20), mload(0x800), f_q))
mstore(0x2b60, mulmod(mload(0x2ac0), mload(0x800), f_q))
mstore(0x2b80, addmod(mload(0x27c0), mload(0x2b00), f_q))
mstore(0x2ba0, mulmod(1, mload(0x1ea0), f_q))
{
            let result := mulmod(mload(0x740), mload(0x1c40), f_q)
result := addmod(mulmod(mload(0x760), mload(0x1c60), f_q), result, f_q)
mstore(11200, result)
        }
mstore(0x2be0, mulmod(mload(0x2bc0), mload(0x2020), f_q))
mstore(0x2c00, mulmod(sub(f_q, mload(0x2be0)), 1, f_q))
mstore(0x2c20, mulmod(mload(0x2ba0), 1, f_q))
mstore(0x2c40, mulmod(mload(0x2c00), mload(0x2160), f_q))
mstore(0x2c60, mulmod(mload(0x2c20), mload(0x2160), f_q))
mstore(0x2c80, addmod(mload(0x2b80), mload(0x2c40), f_q))
mstore(0x2ca0, mulmod(1, mload(0x1b80), f_q))
mstore(0x2cc0, mulmod(1, mload(0x8a0), f_q))
mstore(0x2ce0, 0x0000000000000000000000000000000000000000000000000000000000000001)
                    mstore(0x2d00, 0x0000000000000000000000000000000000000000000000000000000000000002)
mstore(0x2d20, mload(0x2c80))
success := and(eq(staticcall(gas(), 0x7, 0x2ce0, 0x60, 0x2ce0, 0x40), 1), success)
mstore(0x2d40, mload(0x2ce0))
                    mstore(0x2d60, mload(0x2d00))
mstore(0x2d80, mload(0xa0))
                    mstore(0x2da0, mload(0xc0))
success := and(eq(staticcall(gas(), 0x6, 0x2d40, 0x80, 0x2d40, 0x40), 1), success)
mstore(0x2dc0, mload(0xe0))
                    mstore(0x2de0, mload(0x100))
mstore(0x2e00, mload(0x27e0))
success := and(eq(staticcall(gas(), 0x7, 0x2dc0, 0x60, 0x2dc0, 0x40), 1), success)
mstore(0x2e20, mload(0x2d40))
                    mstore(0x2e40, mload(0x2d60))
mstore(0x2e60, mload(0x2dc0))
                    mstore(0x2e80, mload(0x2de0))
success := and(eq(staticcall(gas(), 0x6, 0x2e20, 0x80, 0x2e20, 0x40), 1), success)
mstore(0x2ea0, mload(0x120))
                    mstore(0x2ec0, mload(0x140))
mstore(0x2ee0, mload(0x2800))
success := and(eq(staticcall(gas(), 0x7, 0x2ea0, 0x60, 0x2ea0, 0x40), 1), success)
mstore(0x2f00, mload(0x2e20))
                    mstore(0x2f20, mload(0x2e40))
mstore(0x2f40, mload(0x2ea0))
                    mstore(0x2f60, mload(0x2ec0))
success := and(eq(staticcall(gas(), 0x6, 0x2f00, 0x80, 0x2f00, 0x40), 1), success)
mstore(0x2f80, 0x0000000000000000000000000000000000000000000000000000000000000000)
                    mstore(0x2fa0, 0x0000000000000000000000000000000000000000000000000000000000000000)
mstore(0x2fc0, mload(0x2820))
success := and(eq(staticcall(gas(), 0x7, 0x2f80, 0x60, 0x2f80, 0x40), 1), success)
mstore(0x2fe0, mload(0x2f00))
                    mstore(0x3000, mload(0x2f20))
mstore(0x3020, mload(0x2f80))
                    mstore(0x3040, mload(0x2fa0))
success := and(eq(staticcall(gas(), 0x6, 0x2fe0, 0x80, 0x2fe0, 0x40), 1), success)
mstore(0x3060, 0x20375f617e49ab845410901d46917456e732f4d037f3082f6f567b4da5ffc2be)
                    mstore(0x3080, 0x0d0d577c992ddd83c6463c8fcbb897db9e6f985bcf247cfa450afe9f21c4a27b)
mstore(0x30a0, mload(0x2840))
success := and(eq(staticcall(gas(), 0x7, 0x3060, 0x60, 0x3060, 0x40), 1), success)
mstore(0x30c0, mload(0x2fe0))
                    mstore(0x30e0, mload(0x3000))
mstore(0x3100, mload(0x3060))
                    mstore(0x3120, mload(0x3080))
success := and(eq(staticcall(gas(), 0x6, 0x30c0, 0x80, 0x30c0, 0x40), 1), success)
mstore(0x3140, 0x041a51c39c5bc50cff30ef976c7cda7a37cb5fd3251c28c1ac93d94e0ea093a9)
                    mstore(0x3160, 0x04c242de4fe438f713b52fd0aeaf91ac874bcb0a92cd4ddf3e779dd4cf36f7ce)
mstore(0x3180, mload(0x2860))
success := and(eq(staticcall(gas(), 0x7, 0x3140, 0x60, 0x3140, 0x40), 1), success)
mstore(0x31a0, mload(0x30c0))
                    mstore(0x31c0, mload(0x30e0))
mstore(0x31e0, mload(0x3140))
                    mstore(0x3200, mload(0x3160))
success := and(eq(staticcall(gas(), 0x6, 0x31a0, 0x80, 0x31a0, 0x40), 1), success)
mstore(0x3220, 0x18c412fca17f879aea03580631a5dd1476337ad46a48606f787ec9b742cbbb82)
                    mstore(0x3240, 0x0ae220f8156d861559acc78f02923d5285230c8cd2a5be1d00bb8a4033b402fd)
mstore(0x3260, mload(0x2880))
success := and(eq(staticcall(gas(), 0x7, 0x3220, 0x60, 0x3220, 0x40), 1), success)
mstore(0x3280, mload(0x31a0))
                    mstore(0x32a0, mload(0x31c0))
mstore(0x32c0, mload(0x3220))
                    mstore(0x32e0, mload(0x3240))
success := and(eq(staticcall(gas(), 0x6, 0x3280, 0x80, 0x3280, 0x40), 1), success)
mstore(0x3300, 0x2e552a5a50ffd1d3ce67a25a080afd77d06984192fd1ec6521c25620c2be30ec)
                    mstore(0x3320, 0x295aa615fefab5fcecd65db4771f079f38f9d31560d246c7a9f15b0ca864e5c8)
mstore(0x3340, mload(0x28a0))
success := and(eq(staticcall(gas(), 0x7, 0x3300, 0x60, 0x3300, 0x40), 1), success)
mstore(0x3360, mload(0x3280))
                    mstore(0x3380, mload(0x32a0))
mstore(0x33a0, mload(0x3300))
                    mstore(0x33c0, mload(0x3320))
success := and(eq(staticcall(gas(), 0x6, 0x3360, 0x80, 0x3360, 0x40), 1), success)
mstore(0x33e0, mload(0x420))
                    mstore(0x3400, mload(0x440))
mstore(0x3420, mload(0x28c0))
success := and(eq(staticcall(gas(), 0x7, 0x33e0, 0x60, 0x33e0, 0x40), 1), success)
mstore(0x3440, mload(0x3360))
                    mstore(0x3460, mload(0x3380))
mstore(0x3480, mload(0x33e0))
                    mstore(0x34a0, mload(0x3400))
success := and(eq(staticcall(gas(), 0x6, 0x3440, 0x80, 0x3440, 0x40), 1), success)
mstore(0x34c0, mload(0x460))
                    mstore(0x34e0, mload(0x480))
mstore(0x3500, mload(0x28e0))
success := and(eq(staticcall(gas(), 0x7, 0x34c0, 0x60, 0x34c0, 0x40), 1), success)
mstore(0x3520, mload(0x3440))
                    mstore(0x3540, mload(0x3460))
mstore(0x3560, mload(0x34c0))
                    mstore(0x3580, mload(0x34e0))
success := and(eq(staticcall(gas(), 0x6, 0x3520, 0x80, 0x3520, 0x40), 1), success)
mstore(0x35a0, mload(0x380))
                    mstore(0x35c0, mload(0x3a0))
mstore(0x35e0, mload(0x2900))
success := and(eq(staticcall(gas(), 0x7, 0x35a0, 0x60, 0x35a0, 0x40), 1), success)
mstore(0x3600, mload(0x3520))
                    mstore(0x3620, mload(0x3540))
mstore(0x3640, mload(0x35a0))
                    mstore(0x3660, mload(0x35c0))
success := and(eq(staticcall(gas(), 0x6, 0x3600, 0x80, 0x3600, 0x40), 1), success)
mstore(0x3680, mload(0x280))
                    mstore(0x36a0, mload(0x2a0))
mstore(0x36c0, mload(0x2b20))
success := and(eq(staticcall(gas(), 0x7, 0x3680, 0x60, 0x3680, 0x40), 1), success)
mstore(0x36e0, mload(0x3600))
                    mstore(0x3700, mload(0x3620))
mstore(0x3720, mload(0x3680))
                    mstore(0x3740, mload(0x36a0))
success := and(eq(staticcall(gas(), 0x6, 0x36e0, 0x80, 0x36e0, 0x40), 1), success)
mstore(0x3760, mload(0x2c0))
                    mstore(0x3780, mload(0x2e0))
mstore(0x37a0, mload(0x2b40))
success := and(eq(staticcall(gas(), 0x7, 0x3760, 0x60, 0x3760, 0x40), 1), success)
mstore(0x37c0, mload(0x36e0))
                    mstore(0x37e0, mload(0x3700))
mstore(0x3800, mload(0x3760))
                    mstore(0x3820, mload(0x3780))
success := and(eq(staticcall(gas(), 0x6, 0x37c0, 0x80, 0x37c0, 0x40), 1), success)
mstore(0x3840, mload(0x300))
                    mstore(0x3860, mload(0x320))
mstore(0x3880, mload(0x2b60))
success := and(eq(staticcall(gas(), 0x7, 0x3840, 0x60, 0x3840, 0x40), 1), success)
mstore(0x38a0, mload(0x37c0))
                    mstore(0x38c0, mload(0x37e0))
mstore(0x38e0, mload(0x3840))
                    mstore(0x3900, mload(0x3860))
success := and(eq(staticcall(gas(), 0x6, 0x38a0, 0x80, 0x38a0, 0x40), 1), success)
mstore(0x3920, mload(0x340))
                    mstore(0x3940, mload(0x360))
mstore(0x3960, mload(0x2c60))
success := and(eq(staticcall(gas(), 0x7, 0x3920, 0x60, 0x3920, 0x40), 1), success)
mstore(0x3980, mload(0x38a0))
                    mstore(0x39a0, mload(0x38c0))
mstore(0x39c0, mload(0x3920))
                    mstore(0x39e0, mload(0x3940))
success := and(eq(staticcall(gas(), 0x6, 0x3980, 0x80, 0x3980, 0x40), 1), success)
mstore(0x3a00, mload(0x840))
                    mstore(0x3a20, mload(0x860))
mstore(0x3a40, sub(f_q, mload(0x2ca0)))
success := and(eq(staticcall(gas(), 0x7, 0x3a00, 0x60, 0x3a00, 0x40), 1), success)
mstore(0x3a60, mload(0x3980))
                    mstore(0x3a80, mload(0x39a0))
mstore(0x3aa0, mload(0x3a00))
                    mstore(0x3ac0, mload(0x3a20))
success := and(eq(staticcall(gas(), 0x6, 0x3a60, 0x80, 0x3a60, 0x40), 1), success)
mstore(0x3ae0, mload(0x8e0))
                    mstore(0x3b00, mload(0x900))
mstore(0x3b20, mload(0x2cc0))
success := and(eq(staticcall(gas(), 0x7, 0x3ae0, 0x60, 0x3ae0, 0x40), 1), success)
mstore(0x3b40, mload(0x3a60))
                    mstore(0x3b60, mload(0x3a80))
mstore(0x3b80, mload(0x3ae0))
                    mstore(0x3ba0, mload(0x3b00))
success := and(eq(staticcall(gas(), 0x6, 0x3b40, 0x80, 0x3b40, 0x40), 1), success)
mstore(0x3bc0, mload(0x3b40))
                    mstore(0x3be0, mload(0x3b60))
mstore(0x3c00, 0x198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2)
            mstore(0x3c20, 0x1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed)
            mstore(0x3c40, 0x090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b)
            mstore(0x3c60, 0x12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa)
mstore(0x3c80, mload(0x8e0))
                    mstore(0x3ca0, mload(0x900))
mstore(0x3cc0, 0x0181624e80f3d6ae28df7e01eaeab1c0e919877a3b8a6b7fbc69a6817d596ea2)
            mstore(0x3ce0, 0x1783d30dcb12d259bb89098addf6280fa4b653be7a152542a28f7b926e27e648)
            mstore(0x3d00, 0x00ae44489d41a0d179e2dfdc03bddd883b7109f8b6ae316a59e815c1a6b35304)
            mstore(0x3d20, 0x0b2147ab62a386bd63e6de1522109b8c9588ab466f5aadfde8c41ca3749423ee)
success := and(eq(staticcall(gas(), 0x8, 0x3bc0, 0x180, 0x3bc0, 0x20), 1), success)
success := and(eq(mload(0x3bc0), 1), success)

            if not(success) { revert(0, 0) }
            return(0, 0)

                }
            }
        }