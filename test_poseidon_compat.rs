// Test Poseidon compatibility between pse-poseidon and poseidon-solidity
use pse_poseidon::Poseidon;
use halo2curves_axiom::bn256::Fr;
use halo2curves_axiom::ff::PrimeField;

fn main() {
    // Test with the same inputs as the Solidity test
    let input1 = Fr::from(12345u64);
    let input2 = Fr::from(67890u64);
    
    // Compute hash using pse-poseidon
    let mut poseidon = Poseidon::<Fr, 3, 2>::new(8, 57);
    poseidon.update(&[input1, input2]);
    let hash = poseidon.squeeze();
    
    println!("Input 1: {}", input1);
    println!("Input 2: {}", input2);
    println!("Hash (decimal): {}", hash);
    println!("Hash (hex): 0x{:064x}", hash);
    
    // Expected from Solidity: 0x1914879b2a4e7f9555f3eb55837243cefb1366a692794a7e5b5b3181fb14b49b
    // = 11344094074881186137859743404234365978119253787583526441303892667757095072923
    
    let expected_hex = "0x1914879b2a4e7f9555f3eb55837243cefb1366a692794a7e5b5b3181fb14b49b";
    println!("\nExpected from Solidity: {}", expected_hex);
    println!("Expected (decimal): 11344094074881186137859743404234365978119253787583526441303892667757095072923");
}

