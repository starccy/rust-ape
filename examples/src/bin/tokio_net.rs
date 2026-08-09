//! tokio's multi-threaded runtime, with shim/epoll.c answering mio's epoll calls.
//!
//! Covers the four things the IO driver needs to get right here: readiness on
//! sockets, the timer wheel, waking the driver from a thread it doesn't own,
//! and plain task scheduling.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;

const CLIENTS: usize = 64;
const ROUNDS: usize = 8;
const TASKS: usize = 2000;

fn main() {
    let rt = Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        echo_roundtrips().await;
        timers().await;
        wake_from_os_thread().await;
        many_tasks().await;
    });

    println!("\ntokio net ok");
}

/// 64 connections, each doing 8 request/response pairs with a payload only that
/// client could have sent, so a crossed response can't slip through.
async fn echo_roundtrips() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    println!("listening on {addr}");

    let served = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    let server = tokio::spawn(async move {
        for _ in 0..CLIENTS {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let counter = counter.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                loop {
                    let n = sock.read(&mut buf).await.expect("server read");
                    if n == 0 {
                        break;
                    }
                    sock.write_all(&buf[..n]).await.expect("server write");
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    let started = Instant::now();
    let clients: Vec<_> = (0..CLIENTS)
        .map(|id| {
            tokio::spawn(async move {
                let mut sock = TcpStream::connect(addr).await.expect("connect");
                sock.set_nodelay(true).expect("nodelay");
                for round in 0..ROUNDS {
                    let msg = format!("c{id}r{round}");
                    sock.write_all(msg.as_bytes()).await.expect("client write");
                    let mut back = vec![0u8; msg.len()];
                    sock.read_exact(&mut back).await.expect("client read");
                    assert_eq!(back, msg.as_bytes(), "client {id} got someone else's reply");
                }
            })
        })
        .collect();

    for c in clients {
        c.await.expect("client task");
    }
    server.await.expect("server task");

    let total = CLIENTS * ROUNDS;
    println!("{total} roundtrips over {CLIENTS} connections in {:?}", started.elapsed());
    assert!(served.load(Ordering::Relaxed) >= total, "server lost messages");
}

/// The timer wheel has no socket involved, so it fails independently of the
/// selector: a driver that never parks with a deadline would return instantly.
async fn timers() {
    let started = Instant::now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let slept = started.elapsed();
    println!("sleep(300ms) took {slept:?}");
    assert!(slept >= Duration::from_millis(295), "sleep returned early: {slept:?}");
    assert!(slept < Duration::from_secs(5), "sleep overshot badly: {slept:?}");

    let pending = tokio::time::sleep(Duration::from_secs(30));
    let r = tokio::time::timeout(Duration::from_millis(100), pending).await;
    assert!(r.is_err(), "timeout should have fired");
    println!("timeout fired");
}

/// mio's waker is a pipe here rather than an eventfd. This is what forces it:
/// an OS thread outside the runtime hands work to the driver while it is parked
/// in poll(), and the driver has to come back out.
async fn wake_from_os_thread() {
    let handle = tokio::runtime::Handle::current();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<usize>(8);

    let joiner = std::thread::spawn(move || {
        for i in 0..4 {
            std::thread::sleep(Duration::from_millis(20));
            let tx = tx.clone();
            handle.spawn(async move { tx.send(i).await.expect("send") });
        }
    });

    let mut got = Vec::new();
    while let Some(i) = rx.recv().await {
        got.push(i);
    }
    joiner.join().expect("os thread");

    got.sort_unstable();
    assert_eq!(got, [0, 1, 2, 3], "lost a wakeup from outside the runtime");
    println!("woken from an OS thread {} times", got.len());
}

async fn many_tasks() {
    let tasks: Vec<_> = (0..TASKS).map(|i| tokio::spawn(async move { i })).collect();
    let mut sum = 0usize;
    for t in tasks {
        sum += t.await.expect("task");
    }
    assert_eq!(sum, TASKS * (TASKS - 1) / 2);
    println!("{TASKS} tasks joined");
}
