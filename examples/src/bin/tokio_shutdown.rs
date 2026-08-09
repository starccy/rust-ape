//! Tearing a tokio runtime down while sockets are still registered.
//!
//! This is the exit path, not the happy path. Sockets sit half-finished in the
//! selector, tasks get aborted mid-read, and the last runtime is deliberately
//! still alive when main returns, the same shape as a global runtime in a
//! `OnceLock`. Workers can still be polling sockets while the process exits,
//! which is the window shim/winsock.c holds Winsock open for, so the
//! interesting result is the exit code.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::{Builder, Runtime};

const ROUNDS: usize = 4;
const CONNS: usize = 32;

fn main() {
    for round in 0..ROUNDS {
        let rt = rt();
        rt.block_on(chaos());
        // Half the tasks are still mid-read and the listener is still
        // registered; this drop has to unwind all of it.
        drop(rt);
        println!("round {round} torn down");
    }

    // Same again, except nothing tears it down: the runtime outlives main, so
    // its workers are still in poll() while the process exits.
    let rt = Box::leak(Box::new(rt()));
    rt.block_on(chaos());
    println!("leaked runtime left running");

    println!("\ntokio shutdown ok");
}

fn rt() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime")
}

/// Leaves the runtime holding: connections blocked in a read that never gets an
/// answer, connections whose task was aborted out from under them, and a
/// half-written connection nobody is reading.
async fn chaos() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    // A server that reads but never replies, so every client below stays
    // parked in the selector.
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => return,
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                while sock.read(&mut buf).await.unwrap_or(0) > 0 {}
            });
        }
    });

    let mut handles = Vec::new();
    for id in 0..CONNS {
        handles.push(tokio::spawn(async move {
            let mut sock = TcpStream::connect(addr).await.expect("connect");
            sock.write_all(format!("hello {id}").as_bytes()).await.expect("write");
            let mut buf = [0u8; 64];
            // Never returns; the answer isn't coming.
            let _ = sock.read(&mut buf).await;
        }));
    }

    // Let them all reach the read before pulling the rug out.
    tokio::time::sleep(Duration::from_millis(50)).await;
    for h in handles.iter().take(CONNS / 2) {
        h.abort();
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    // The rest are left running on purpose.
}
