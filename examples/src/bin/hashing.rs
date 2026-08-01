//! Digests, and the runtime CPU dispatch underneath them.
//!
//! sha2 and blake3 choose a SIMD backend at runtime: CPUID on x86, AT_HWCAP on
//! aarch64. cosmo reports AT_HWCAP as 0 off Linux, so aarch64 there should fall
//! back to scalar. The expected digests below are fixed, so a wrongly picked
//! backend shows up directly as a wrong digest.

use md5::Digest;
use std::io::{Read, Write};

const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const ABC_MD5: &str = "900150983cd24fb0d6963f7d28e17f72";
const ABC_BLAKE3: &str = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";

// 100_000 bytes of `i % 251`, which is long enough to go wide.
const BIG_SHA256: &str = "cd2df694e424bc7968cc37f47751019e5ca0cd1bdf2e479ea537c3a1c32ee1aa";
const BIG_MD5: &str = "28cb595c158e9b74e34ae9e8da710fff";
const BIG_BLAKE3: &str = "d93c23eedaf165a7e0be908ba86f1a7a520d568d2d13cde787c8580c5c72cc54";

fn main() {
    // What the dispatch had to work with.
    println!("AT_HWCAP: {:#x}", ape::auxval(16));
    #[cfg(target_arch = "x86_64")]
    println!(
        "detected: sha={} avx2={} sse4.1={}",
        is_x86_feature_detected!("sha"),
        is_x86_feature_detected!("avx2"),
        is_x86_feature_detected!("sse4.1"),
    );

    let big: Vec<u8> = (0..100_000usize).map(|i| (i % 251) as u8).collect();

    for (label, data, sha, md5, b3) in [
        ("abc", b"abc".as_slice(), ABC_SHA256, ABC_MD5, ABC_BLAKE3),
        ("100k", &big, BIG_SHA256, BIG_MD5, BIG_BLAKE3),
    ] {
        let got_sha = hex(&sha2::Sha256::digest(data));
        let got_md5 = hex(&md5::Md5::digest(data));
        let got_b3 = blake3::hash(data).to_hex().to_string();
        println!("{label:>5}  sha256 {got_sha}");
        println!("{label:>5}  md5    {got_md5}");
        println!("{label:>5}  blake3 {got_b3}");
        assert_eq!(got_sha, sha, "sha256 of {label} differs from the host's");
        assert_eq!(got_md5, md5, "md5 of {label} differs from the host's");
        assert_eq!(got_b3, b3, "blake3 of {label} differs from the host's");
    }

    // Feeding it in pieces has to land in the same place. Chunk boundaries are
    // where a vectorized implementation gets it wrong if it's going to.
    let mut sha = sha2::Sha256::new();
    let mut b3 = blake3::Hasher::new();
    for chunk in big.chunks(1237) {
        sha.update(chunk);
        b3.update(chunk);
    }
    assert_eq!(hex(&sha.finalize()), BIG_SHA256, "incremental sha256 differs");
    assert_eq!(b3.finalize().to_hex().to_string(), BIG_BLAKE3, "incremental blake3 differs");
    println!("incremental in 1237-byte chunks matches");

    // The everyday use: digest a file you're streaming off disk.
    let path = std::env::temp_dir().join(format!("rust-ape-hashing-{}.bin", std::process::id()));
    std::fs::File::create(&path)
        .expect("create")
        .write_all(&big)
        .expect("write");

    let mut f = std::fs::File::open(&path).expect("open");
    let mut sha = sha2::Sha256::new();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = f.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    assert_eq!(hex(&sha.finalize()), BIG_SHA256, "sha256 of the file differs");
    std::fs::remove_file(&path).expect("remove");
    println!("streamed a {}-byte file to the same digest", big.len());

    println!("\nhashing ok");
}

/// sha2 0.11 hands back a hybrid_array::Array, which has no LowerHex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
