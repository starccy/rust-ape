//! Tasks and timers on smol's executor, with no IO involved.

use smol::Timer;
use std::time::{Duration, Instant};

const TASKS: usize = 16;

fn main() {
    smol::block_on(async {
        // Tasks run concurrently, not one after another: staggered sleeps that
        // would take 16 x 20ms in sequence should finish in about 320ms's worth
        // of the longest one.
        let started = Instant::now();
        let tasks: Vec<_> = (0..TASKS)
            .map(|id| {
                smol::spawn(async move {
                    Timer::after(Duration::from_millis(20 * (id as u64 % 4 + 1))).await;
                    id * id
                })
            })
            .collect();

        let mut results = Vec::new();
        for t in tasks {
            results.push(t.await);
        }
        let elapsed = started.elapsed();

        let expected: Vec<usize> = (0..TASKS).map(|i| i * i).collect();
        assert_eq!(results, expected, "tasks returned the wrong values");
        println!("{TASKS} tasks finished in {elapsed:?}");

        let sequential = Duration::from_millis(20 * (0..TASKS).map(|i| i as u64 % 4 + 1).sum::<u64>());
        assert!(
            elapsed < sequential,
            "took {elapsed:?}, which is no better than running them in sequence ({sequential:?})"
        );

        // A timer that actually has to wait.
        let started = Instant::now();
        Timer::after(Duration::from_millis(150)).await;
        let slept = started.elapsed();
        assert!(slept >= Duration::from_millis(140), "timer fired early after {slept:?}");
        println!("timer slept {slept:?}");

        // Moving blocking work off the executor.
        let sum = smol::unblock(|| (1u64..=1_000_000).sum::<u64>()).await;
        assert_eq!(sum, 500_000_500_000, "unblocked work computed the wrong sum");
        println!("unblock computed {sum}");

        println!("\nasync tasks ok");
    });
}
