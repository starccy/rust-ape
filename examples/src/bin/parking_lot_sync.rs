//! parking_lot's thread parker issues raw `libc::syscall(SYS_futex, ...)`,
//! which no std patch can reach since it is a third-party crate, and cosmo's
//! own syscall() answers ENOSYS. The shim reroutes libc::syscall itself
//! (__ape_shim_syscall in shim/futex.c); this example verifies that path.

use std::sync::Arc;
use std::time::{Duration, Instant};

fn cpu_now() -> Duration {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

fn main() {
    // Contended mutex: parkers must sleep and get woken, and the count
    // proves mutual exclusion held.
    let m = Arc::new(parking_lot::Mutex::new(0u64));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let m = Arc::clone(&m);
            std::thread::spawn(move || {
                for _ in 0..10_000 {
                    *m.lock() += 1;
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*m.lock(), 40_000);

    // Timed condvar wait: the ETIMEDOUT path, and it must BLOCK — an
    // ENOSYS-swallowing or mis-timed futex either spins (CPU) or wakes
    // instantly (wall clock).
    let lock = parking_lot::Mutex::new(());
    let cv = parking_lot::Condvar::new();
    let cpu0 = cpu_now();
    let t0 = Instant::now();
    let mut guard = lock.lock();
    let timed_out = cv.wait_for(&mut guard, Duration::from_millis(300)).timed_out();
    drop(guard);
    assert!(timed_out, "nobody notified — this must time out");
    let waited = t0.elapsed();
    let spun = cpu_now() - cpu0;
    assert!(waited >= Duration::from_millis(280), "woke too early: {waited:?}");
    assert!(waited < Duration::from_secs(5), "timeout overshot: {waited:?}");
    assert!(
        spun < Duration::from_millis(100),
        "condvar wait burned {spun:?} of CPU — the futex is spinning"
    );

    // Notify round trip: a parked waiter must actually receive the wake.
    let pair = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
    let notifier = {
        let pair = Arc::clone(&pair);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let (l, c) = &*pair;
            *l.lock() = true;
            c.notify_one();
        })
    };
    let (l, c) = &*pair;
    let mut done = l.lock();
    while !*done {
        let r = c.wait_for(&mut done, Duration::from_secs(5));
        assert!(!r.timed_out(), "the notify never arrived");
    }
    drop(done);
    notifier.join().unwrap();

    println!("parking_lot_sync: all assertions passed");
}
