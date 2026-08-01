//! Randomness, from the OS entropy source up through the `rand` crate.

use rand::Rng;
use std::collections::HashSet;

const DRAWS: usize = 1000;

fn main() {
    // std's own consumer of the entropy source: HashMap's seed.
    let seeds: HashSet<u64> = (0..8)
        .map(|_| {
            use std::hash::{BuildHasher, Hasher};
            std::hash::RandomState::new().build_hasher().finish()
        })
        .collect();
    assert!(seeds.len() > 1, "RandomState handed back the same seed every time");
    println!("std RandomState: {} distinct seeds out of 8", seeds.len());

    // The rand crate, which sources its seed through getrandom.
    let mut rng = rand::thread_rng();

    let mut buf = [0u8; 32];
    rng.fill(&mut buf[..]);
    assert!(buf.iter().any(|&b| b != 0), "filled buffer came back all zeros");
    println!("32 random bytes: {}", hex(&buf[..8]));

    // A range, drawn often enough that a stuck generator would show.
    let mut seen = HashSet::new();
    for _ in 0..DRAWS {
        let n: u32 = rng.gen_range(0..100);
        assert!(n < 100, "gen_range went out of bounds");
        seen.insert(n);
    }
    assert!(seen.len() > 50, "only {} distinct values in {DRAWS} draws", seen.len());
    println!("{DRAWS} draws from 0..100 hit {} distinct values", seen.len());

    // Two generators shouldn't agree.
    let a: [u64; 4] = rand::random();
    let b: [u64; 4] = rand::random();
    assert_ne!(a, b, "two generators produced identical output");

    println!("\nrandom ok");
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
