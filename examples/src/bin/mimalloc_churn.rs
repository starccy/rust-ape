//! mimalloc as the global allocator under sustained churn.
//!
//! ruff and biome pick mimalloc on Windows targets, and mimalloc talks to the
//! OS the Linux way: it over-allocates for alignment and unmaps the ends,
//! commits and decommits with MAP_FIXED, purges with madvise. NT can't
//! release part of a VirtualAlloc region, so the fork punches holes with
//! MEM_DECOMMIT instead. This churns large and small blocks across threads
//! and watches commit charge, which is where the old trim leak showed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// (data segment, resident) in KB, from /proc/self/status on every host:
/// the fork fills VmData from NT's private commit charge and from the
/// region walk on macOS, so the same two lines answer everywhere.
fn memory() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (field("VmData:"), field("VmRSS:"))
}

fn churn(stop: Arc<AtomicBool>, seed: u64) -> u64 {
    let mut x = seed | 1;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut live: Vec<Vec<u8>> = Vec::new();
    let mut ops = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let r = next();
        // Mostly small, a quarter medium, a few percent large enough for
        // their own extents.
        let size = match r % 100 {
            0..=69 => 16 + (r >> 8) as usize % 1024,
            70..=94 => 4096 + (r >> 8) as usize % (256 * 1024),
            _ => 1 << 20 | (r >> 8) as usize % (4 << 20),
        };
        let mut v = vec![0u8; size];
        v[0] = r as u8;
        v[size - 1] = (r >> 8) as u8;
        if live.len() < 64 {
            live.push(v);
        } else {
            live[(r >> 32) as usize % 64] = v;
        }
        if r % 977 == 0 {
            live.clear();
        }
        ops += 1;
    }
    ops
}

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(20);
    let threads = 8;
    let (c0, r0) = memory();
    println!("start: commit {c0} KB, resident {r0} KB");

    let stop = Arc::new(AtomicBool::new(false));
    let workers: Vec<_> = (0..threads)
        .map(|i| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                churn(stop, 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(i as u64 + 1))
            })
        })
        .collect();

    let t0 = Instant::now();
    let mut peak = 0;
    let mut samples = Vec::new();
    while t0.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_secs(2));
        let (c, r) = memory();
        peak = peak.max(c);
        samples.push(c);
        println!(
            "{:>4}s: commit {c} KB, resident {r} KB",
            t0.elapsed().as_secs()
        );
    }
    stop.store(true, Ordering::Relaxed);
    let ops: u64 = workers.into_iter().map(|w| w.join().unwrap()).sum();

    // Let decay purge, then look again.
    std::thread::sleep(Duration::from_secs(3));
    let (c1, r1) = memory();
    println!(
        "end: commit {c1} KB, resident {r1} KB, peak commit {peak} KB, {} Mops",
        ops / 1_000_000
    );

    // A leak shows as commit still climbing in the second half of the run.
    let half = samples.len() / 2;
    let first = samples[..half].iter().max().copied().unwrap_or(0);
    let last = samples[half..].iter().max().copied().unwrap_or(0);
    if last > first + first / 2 + 64 * 1024 {
        println!("FAIL commit kept growing: {first} KB -> {last} KB");
        std::process::exit(1);
    }
    println!("ok");
}
