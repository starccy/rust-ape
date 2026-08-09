//! tokio's signal driver: a handler writes to a self-pipe and the runtime turns
//! that into a stream.

use std::time::Duration;

use tokio::signal::unix::{SignalKind, signal};

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        single().await;
        repeated().await;
        two_kinds().await;
        ctrl_c_registers().await;
    });

    println!("\ntokio signal ok");
}

async fn single() {
    let mut usr1 = signal(SignalKind::user_defined1()).expect("register SIGUSR1");
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0, "raise failed");
    });

    tokio::time::timeout(Duration::from_secs(10), usr1.recv())
        .await
        .expect("SIGUSR1 never arrived")
        .expect("signal stream ended");
    println!("SIGUSR1 delivered to the runtime");
}

/// The driver coalesces, so ten raises are somewhere between one and ten
/// notifications. What matters is that it keeps delivering rather than going
/// deaf after the first.
async fn repeated() {
    let mut usr1 = signal(SignalKind::user_defined1()).expect("register SIGUSR1");
    let mut seen = 0;
    for _ in 0..10 {
        assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0, "raise failed");
        tokio::time::sleep(Duration::from_millis(10)).await;
        if tokio::time::timeout(Duration::from_millis(200), usr1.recv()).await.is_ok() {
            seen += 1;
        }
    }
    assert!(seen >= 5, "only {seen} of 10 raises came through");
    println!("10 raises produced {seen} notifications");
}

async fn two_kinds() {
    let mut usr1 = signal(SignalKind::user_defined1()).expect("register SIGUSR1");
    let mut usr2 = signal(SignalKind::user_defined2()).expect("register SIGUSR2");

    assert_eq!(unsafe { libc::raise(libc::SIGUSR2) }, 0, "raise failed");
    let which = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = usr1.recv() => "usr1",
            _ = usr2.recv() => "usr2",
        }
    })
    .await
    .expect("neither signal arrived");
    assert_eq!(which, "usr2", "SIGUSR2 woke the SIGUSR1 stream");
    println!("two streams stayed separate");
}

/// ctrl_c() is SIGINT here. Raising it would be a fine test on Linux but the
/// window between installing the handler and the raise is not worth the risk in
/// CI, so this only checks that registering it works and that it stays pending.
async fn ctrl_c_registers() {
    let r = tokio::time::timeout(Duration::from_millis(100), tokio::signal::ctrl_c()).await;
    assert!(r.is_err(), "ctrl_c() completed without anyone pressing anything");
    println!("ctrl_c() registered and stayed pending");
}
