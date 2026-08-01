//! Threads and the synchronization primitives built on them.
//!
//! Naming a thread goes through `pthread_setname_np`, which returns
//! ENOSYS off Linux.
//! library.patch makes that non-fatal, so a name coming back here means
//! the thread was named and a missing one is still fine.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const WORKERS: usize = 4;
const PER_WORKER: usize = 1000;

fn main() {
    println!("available parallelism: {:?}", thread::available_parallelism());

    // A named thread that reports its own name back.
    let name = thread::Builder::new()
        .name("cosmo-worker".into())
        .spawn(|| thread::current().name().map(str::to_owned))
        .expect("spawn named")
        .join()
        .expect("join named");
    assert_eq!(name.as_deref(), Some("cosmo-worker"), "thread name did not stick");
    println!("named thread reported {:?}", name.unwrap());

    // Mutex and atomic under contention.
    let counter = Arc::new(Mutex::new(0usize));
    let atomic = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(WORKERS));

    let mut handles = Vec::new();
    for id in 0..WORKERS {
        let (counter, atomic, barrier) = (counter.clone(), atomic.clone(), barrier.clone());
        handles.push(thread::spawn(move || {
            // Start together, so the locking actually overlaps.
            barrier.wait();
            for _ in 0..PER_WORKER {
                *counter.lock().expect("lock") += 1;
                atomic.fetch_add(1, Ordering::Relaxed);
            }
            id
        }));
    }
    let ids: Vec<usize> = handles.into_iter().map(|h| h.join().expect("join")).collect();
    assert_eq!(ids.len(), WORKERS, "not every worker returned");

    let total = WORKERS * PER_WORKER;
    assert_eq!(*counter.lock().expect("lock"), total, "mutex lost increments");
    assert_eq!(atomic.load(Ordering::Relaxed), total, "atomic lost increments");
    println!("{WORKERS} threads x {PER_WORKER} increments = {total}");

    // Channels.
    let (tx, rx) = mpsc::channel();
    for id in 0..WORKERS {
        let tx = tx.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10 * id as u64));
            tx.send(id).expect("send");
        });
    }
    drop(tx);
    let mut received: Vec<usize> = rx.iter().collect();
    received.sort();
    assert_eq!(received, (0..WORKERS).collect::<Vec<_>>(), "channel lost messages");
    println!("channel delivered {received:?}");

    // Scoped threads, borrowing a local without an Arc.
    let data = vec![1u64, 2, 3, 4, 5, 6, 7, 8];
    let sum: u64 = thread::scope(|s| {
        let halves: Vec<_> = data
            .chunks(4)
            .map(|chunk| s.spawn(move || chunk.iter().sum::<u64>()))
            .collect();
        halves.into_iter().map(|h| h.join().expect("join scoped")).sum()
    });
    assert_eq!(sum, data.iter().sum::<u64>(), "scoped threads summed wrong");
    println!("scoped threads summed to {sum}");

    println!("\nthreads ok");
}
