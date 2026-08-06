//! Mutex/Condvar/park, sitting on the futex shim.
//!
//! std's futex path is a raw syscall(SYS_futex), and cosmo's syscall() stub
//! answers ENOSYS, which upstream error handling treated as a spurious
//! wakeup. Everything still worked, but every blocking wait became a spin
//! loop burning a full core, on every platform. shim/futex.c reroutes to
//! cosmo's real futex; this exercises the three shapes of waiting and, on
//! Linux, asserts the spin is gone by comparing CPU time against wall time.

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn cpu_time() -> Option<Duration> {
    // CLOCK_PROCESS_CPUTIME_ID by its Linux number; only trustworthy there.
    if !ape::is_linux() {
        return None;
    }
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    (unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) } == 0)
        .then(|| Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

fn main() {
    // Contended mutex: 8 threads, small critical sections.
    let counter = Arc::new(Mutex::new(0u64));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let counter = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..10_000 {
                *counter.lock().unwrap() += 1;
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*counter.lock().unwrap(), 80_000, "mutex lost updates");
    println!("contended mutex ok");

    // Condvar handoff: the waiter must actually wake when notified.
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let waiter = {
        let pair = Arc::clone(&pair);
        thread::spawn(move || {
            let (lock, cvar) = &*pair;
            let mut ready = lock.lock().unwrap();
            while !*ready {
                ready = cvar.wait(ready).unwrap();
            }
        })
    };
    thread::sleep(Duration::from_millis(50));
    let (lock, cvar) = &*pair;
    *lock.lock().unwrap() = true;
    cvar.notify_one();
    waiter.join().unwrap();
    println!("condvar notify ok");

    // Timed wait with nobody notifying: must report the timeout, roughly on
    // time, and — the actual regression — without spinning through it.
    let cpu_before = cpu_time();
    let (lock, cvar) = &*pair;
    let begin = Instant::now();
    let (_guard, res) = cvar
        .wait_timeout_while(lock.lock().unwrap(), Duration::from_millis(500), |_| true)
        .unwrap();
    let wall = begin.elapsed();
    assert!(res.timed_out(), "timed wait claims it was woken");
    assert!(wall >= Duration::from_millis(450), "timed wait returned early: {wall:?}");
    if let (Some(a), Some(b)) = (cpu_before, cpu_time()) {
        let spent = b - a;
        assert!(
            spent < Duration::from_millis(100),
            "half a second of waiting burned {spent:?} of CPU — the futex is spinning again"
        );
        println!("condvar timeout ok ({wall:?} wall, {spent:?} cpu)");
    } else {
        println!("condvar timeout ok ({wall:?} wall; cpu check is Linux-only)");
    }

    // Channel + park/unpark round trip.
    let (tx, rx) = mpsc::channel();
    let sender = thread::spawn(move || {
        for i in 0..1000u32 {
            tx.send(i).unwrap();
        }
    });
    let sum: u64 = rx.iter().map(u64::from).sum();
    sender.join().unwrap();
    assert_eq!(sum, 499_500, "channel dropped messages");
    println!("mpsc channel ok");

    println!("\nsync primitives ok");
}
