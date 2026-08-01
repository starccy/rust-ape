//! Clocks: monotonic time, wall time, and sleeping.

use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let now = SystemTime::now();
    let since_epoch = now.duration_since(UNIX_EPOCH).expect("clock is before 1970");
    println!("unix time: {}s", since_epoch.as_secs());
    assert!(
        since_epoch.as_secs() > 1_700_000_000,
        "wall clock reads {}s, which is before 2023. Wrong epoch or units?",
        since_epoch.as_secs()
    );

    // Monotonic clock: must not go backwards.
    let start = Instant::now();
    let mut last = start;
    for _ in 0..1000 {
        let now = Instant::now();
        assert!(now >= last, "monotonic clock went backwards");
        last = now;
    }
    println!("1000 Instant::now() reads took {:?}", start.elapsed());

    for ms in [10u64, 50, 100] {
        let started = Instant::now();
        thread::sleep(Duration::from_millis(ms));
        let slept = started.elapsed();
        println!("slept {ms}ms, measured {slept:?}");
        assert!(
            slept >= Duration::from_millis(ms) - Duration::from_millis(2),
            "sleep({ms}ms) returned after only {slept:?}"
        );
        assert!(slept < Duration::from_secs(5), "sleep({ms}ms) took {slept:?}");
    }

    // The two clocks should agree on how much time passed.
    let wall_start = SystemTime::now();
    let mono_start = Instant::now();
    thread::sleep(Duration::from_millis(200));
    let wall = SystemTime::now().duration_since(wall_start).expect("wall clock went backwards");
    let mono = mono_start.elapsed();
    println!("wall {wall:?} vs monotonic {mono:?}");
    let skew = wall.abs_diff(mono);
    assert!(skew < Duration::from_millis(100), "clocks disagree by {skew:?}");

    println!("\ntime clock ok");
}
