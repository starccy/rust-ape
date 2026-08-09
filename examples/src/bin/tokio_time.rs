//! The timer wheel: intervals, deadlines, timeouts and a thousand sleeps at once.

use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior, interval, sleep, sleep_until, timeout};

const SLEEPS: usize = 1000;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        intervals().await;
        deadlines().await;
        timeouts().await;
        many_sleeps().await;
    });

    println!("\ntokio time ok");
}

async fn intervals() {
    // The first tick is immediate, so five ticks is four gaps.
    let started = Instant::now();
    let mut tick = interval(Duration::from_millis(50));
    for _ in 0..5 {
        tick.tick().await;
    }
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(195), "five ticks in {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "five ticks took {elapsed:?}");
    println!("interval: 5 ticks of 50ms in {elapsed:?}");

    // Falling behind must not queue up a burst of catch-up ticks.
    let mut tick = interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick.tick().await;
    sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    tick.tick().await;
    tick.tick().await;
    assert!(
        started.elapsed() >= Duration::from_millis(8),
        "a delayed interval burst-fired its backlog"
    );
    println!("interval: missed ticks delayed rather than bursting");
}

async fn deadlines() {
    let deadline = Instant::now() + Duration::from_millis(120);
    sleep_until(deadline).await;
    assert!(Instant::now() >= deadline, "sleep_until returned before its deadline");

    // Zero and past deadlines have to complete rather than hang.
    sleep(Duration::ZERO).await;
    sleep_until(Instant::now() - Duration::from_secs(1)).await;
    println!("deadlines: sleep_until honored, zero and past deadlines returned");
}

async fn timeouts() {
    let r = timeout(Duration::from_millis(80), sleep(Duration::from_secs(30))).await;
    assert!(r.is_err(), "timeout never fired");

    let r = timeout(Duration::from_secs(30), async { 7 }).await;
    assert_eq!(r.expect("timeout fired on a ready future"), 7);

    // A timeout wrapped around a socket read is the shape most code actually
    // uses: the driver has to hand the timer back even though the fd never
    // becomes readable.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let accepted = tokio::spawn(async move { listener.accept().await.expect("accept") });
    let mut sock = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let _peer = accepted.await.expect("accept task");

    let mut buf = [0u8; 16];
    let started = Instant::now();
    let r = timeout(Duration::from_millis(100), tokio::io::AsyncReadExt::read(&mut sock, &mut buf)).await;
    assert!(r.is_err(), "a read that had no data reported ready");
    assert!(started.elapsed() >= Duration::from_millis(90), "the read timeout fired early");
    println!("timeout: fired on a silent socket, passed a ready future through");
}

/// A thousand overlapping deadlines spread across the wheel's slots, which is
/// where a broken timer shows up as either an early wake or a lost one.
async fn many_sleeps() {
    let started = Instant::now();
    let tasks: Vec<_> = (0..SLEEPS)
        .map(|i| {
            let want = Duration::from_millis((i % 200) as u64 + 1);
            tokio::spawn(async move {
                let t = Instant::now();
                sleep(want).await;
                assert!(t.elapsed() >= want, "sleep({want:?}) returned after {:?}", t.elapsed());
            })
        })
        .collect();
    for t in tasks {
        t.await.expect("sleeper");
    }
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(10), "{SLEEPS} sleeps took {elapsed:?}");
    println!("{SLEEPS} overlapping sleeps, longest 200ms, all done in {elapsed:?}");
}
