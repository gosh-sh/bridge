// Test Poseidon compatibility between scroll-tech/poseidon and poseidon-solidity
use poseidon_base::primitives::{ConstantLength, Hash as PoseidonHash, P128Pow5T3Compact};
use halo2_base::halo2_proofs::halo2curves::bn256::Fr;
use halo2_base::halo2_proofs::halo2curves::ff::PrimeField;

fn main() {
    // Test with the same inputs as the Solidity test
    let input1 = Fr::from(12345u64);
    let input2 = Fr::from(67890u64);

    // Compute hash using scroll-tech/poseidon with T=3, RATE=2
    let hash = PoseidonHash::<Fr, P128Pow5T3Compact<Fr>, ConstantLength<2>, 3, 2>::init()
        .hash([input1, input2]);

    // Convert to hex string for display
    let hash_bytes = hash.to_repr();
    let hash_hex = hex::encode(hash_bytes.as_ref());

    println!("Input 1: 12345");
    println!("Input 2: 67890");
    println!("Hash (hex): 0x{}", hash_hex);

    // Expected from Solidity: 0x1914879b2a4e7f9555f3eb55837243cefb1366a692794a7e5b5b3181fb14b49b
    // = 11344094074881186137859743404234365978119253787583526441303892667757095072923

    let expected_hex = "1914879b2a4e7f9555f3eb55837243cefb1366a692794a7e5b5b3181fb14b49b";
    println!("\nExpected from Solidity: 0x{}", expected_hex);

    // Check if they match
    if hash_hex == expected_hex {
        println!("\n✅ MATCH! Poseidon implementations are compatible!");
    } else {
        println!("\n❌ MISMATCH! Poseidon implementations are NOT compatible!");
        println!("This means scroll-tech/poseidon and poseidon-solidity use different parameters or implementations.");
    }
}

