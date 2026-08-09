//! tokio's synchronization primitives under real contention.
//!
//! These are pure Rust on top of the scheduler, so what's actually being
//! checked is that tasks park and wake correctly on this runtime rather than
//! spinning or losing a wakeup.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{Barrier, Mutex, Notify, RwLock, Semaphore, broadcast, mpsc, oneshot, watch};

const TASKS: usize = 64;
const BUMPS: usize = 100;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        mutex().await;
        rwlock().await;
        semaphore().await;
        notify().await;
        channels().await;
        barrier().await;
        select_and_join().await;
    });

    println!("\ntokio sync ok");
}

async fn mutex() {
    let counter = Arc::new(Mutex::new(0usize));
    let mut tasks = Vec::new();
    for _ in 0..TASKS {
        let counter = counter.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..BUMPS {
                *counter.lock().await += 1;
            }
        }));
    }
    for t in tasks {
        t.await.expect("task");
    }
    let got = *counter.lock().await;
    assert_eq!(got, TASKS * BUMPS, "lost increments under contention");
    println!("mutex: {got} increments across {TASKS} tasks");
}

/// Readers have to overlap and the writer has to see them all gone.
async fn rwlock() {
    let lock = Arc::new(RwLock::new(0usize));
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let mut readers = Vec::new();
    for _ in 0..8 {
        let (lock, live, peak) = (lock.clone(), live.clone(), peak.clone());
        readers.push(tokio::spawn(async move {
            let guard = lock.read().await;
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            live.fetch_sub(1, Ordering::SeqCst);
            *guard
        }));
    }
    for r in readers {
        r.await.expect("reader");
    }
    let overlap = peak.load(Ordering::SeqCst);
    assert!(overlap > 1, "readers never overlapped, peak was {overlap}");

    *lock.write().await = 42;
    assert_eq!(*lock.read().await, 42);
    println!("rwlock: {overlap} readers held it at once, writer got through");
}

/// A permit count of 4 has to be a hard ceiling no matter how many tasks pile in.
async fn semaphore() {
    let sem = Arc::new(Semaphore::new(4));
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let mut tasks = Vec::new();
    for _ in 0..32 {
        let (sem, live, peak) = (sem.clone(), live.clone(), peak.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("acquire");
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            live.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for t in tasks {
        t.await.expect("task");
    }
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak <= 4, "{peak} tasks got in past a 4-permit semaphore");
    assert!(peak > 1, "semaphore serialized everything, peak was {peak}");
    println!("semaphore: peak concurrency {peak} of 4 permits");
}

async fn notify() {
    let n = Arc::new(Notify::new());
    let woken = Arc::new(AtomicUsize::new(0));

    // notify_waiters only reaches tasks already waiting, so let them get there.
    let mut waiters = Vec::new();
    for _ in 0..4 {
        let (n, woken) = (n.clone(), woken.clone());
        waiters.push(tokio::spawn(async move {
            n.notified().await;
            woken.fetch_add(1, Ordering::SeqCst);
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    n.notify_waiters();
    for w in waiters {
        w.await.expect("waiter");
    }
    assert_eq!(woken.load(Ordering::SeqCst), 4);

    // A permit stored before anyone waits still gets picked up.
    n.notify_one();
    n.notified().await;
    println!("notify: 4 waiters woken together, stored permit consumed");
}

async fn channels() {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move { tx.send("one shot").expect("oneshot send") });
    assert_eq!(rx.await.expect("oneshot recv"), "one shot");

    // A capacity of 4 against 200 messages means send() has to block and resume.
    let (tx, mut rx) = mpsc::channel::<usize>(4);
    let producer = tokio::spawn(async move {
        for i in 0..200 {
            tx.send(i).await.expect("mpsc send");
        }
    });
    let mut got = 0usize;
    while let Some(i) = rx.recv().await {
        assert_eq!(i, got, "mpsc reordered");
        got += 1;
    }
    producer.await.expect("producer");
    assert_eq!(got, 200);

    let (tx, _) = broadcast::channel::<usize>(16);
    let mut subs: Vec<_> = (0..4).map(|_| tx.subscribe()).collect();
    for i in 0..8 {
        tx.send(i).expect("broadcast send");
    }
    for sub in &mut subs {
        for i in 0..8 {
            assert_eq!(sub.recv().await.expect("broadcast recv"), i);
        }
    }

    let (tx, mut rx) = watch::channel(0usize);
    tokio::spawn(async move {
        for i in 1..=5 {
            tx.send(i).expect("watch send");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let mut last = 0;
    while rx.changed().await.is_ok() {
        last = *rx.borrow();
    }
    assert_eq!(last, 5, "watch ended on {last}");

    println!("channels: oneshot, mpsc with backpressure, broadcast to 4, watch");
}

async fn barrier() {
    let barrier = Arc::new(Barrier::new(TASKS));
    let arrived = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..TASKS {
        let (barrier, arrived) = (barrier.clone(), arrived.clone());
        tasks.push(tokio::spawn(async move {
            arrived.fetch_add(1, Ordering::SeqCst);
            let r = barrier.wait().await;
            // Everyone is past the barrier, so all of them must have arrived.
            assert_eq!(arrived.load(Ordering::SeqCst), TASKS);
            r.is_leader()
        }));
    }
    let mut leaders = 0;
    for t in tasks {
        if t.await.expect("task") {
            leaders += 1;
        }
    }
    assert_eq!(leaders, 1, "{leaders} tasks thought they were the leader");
    println!("barrier: {TASKS} tasks met, exactly one leader");
}

async fn select_and_join() {
    let (tx, rx) = oneshot::channel::<&str>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ = tx.send("channel");
    });

    let winner = tokio::select! {
        v = rx => v.expect("oneshot"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => "timer",
    };
    assert_eq!(winner, "channel", "select! picked the wrong branch");

    let (a, b, c) = tokio::join!(
        async { 1 },
        async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            2
        },
        async { 3 },
    );
    assert_eq!(a + b + c, 6);

    let r: Result<(u8, u8), &str> = tokio::try_join!(async { Ok(1) }, async { Err("nope") });
    assert!(r.is_err(), "try_join! swallowed an error");

    println!("select!, join! and try_join! all behave");
}
