//! Task and runtime plumbing: the blocking pool, JoinSet, LocalSet, cancellation,
//! AsyncFd over a raw fd, and a single-threaded runtime doing real I/O.

use std::cell::RefCell;
use std::os::fd::{AsRawFd, RawFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use tokio::io::unix::AsyncFd;
use tokio::task::JoinSet;

const BLOCKING: usize = 8;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        blocking_pool().await;
        blocking_in_place().await;
        join_set().await;
        local_set().await;
        cancellation().await;
        yielding().await;
        async_fd().await;
    });
    drop(rt);

    current_thread();

    println!("\ntokio task ok");
}

/// spawn_blocking hands work to a separate pool, so eight 100ms sleeps must
/// overlap rather than serialize behind the four worker threads.
async fn blocking_pool() {
    let started = Instant::now();
    let tasks: Vec<_> = (0..BLOCKING)
        .map(|i| {
            tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(100));
                i
            })
        })
        .collect();
    let mut sum = 0;
    for t in tasks {
        sum += t.await.expect("blocking task");
    }
    let elapsed = started.elapsed();
    assert_eq!(sum, BLOCKING * (BLOCKING - 1) / 2);
    assert!(
        elapsed < Duration::from_millis(100 * BLOCKING as u64 / 2),
        "{BLOCKING} blocking tasks took {elapsed:?}, they ran in sequence"
    );
    println!("spawn_blocking: {BLOCKING} × 100ms overlapped into {elapsed:?}");
}

/// block_in_place hands the worker's queue off to another thread. If that
/// handoff didn't happen the sleep below would have nobody to run it.
async fn blocking_in_place() {
    let ticker = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        "timer still ran"
    });
    let v = tokio::task::block_in_place(|| {
        std::thread::sleep(Duration::from_millis(80));
        7
    });
    assert_eq!(v, 7);
    assert_eq!(ticker.await.expect("ticker"), "timer still ran");
    println!("block_in_place: worker handed off, other tasks kept running");
}

async fn join_set() {
    let mut set = JoinSet::new();
    for i in 0..100usize {
        set.spawn(async move {
            tokio::time::sleep(Duration::from_millis((i % 10) as u64)).await;
            i
        });
    }
    let mut seen = vec![false; 100];
    while let Some(r) = set.join_next().await {
        seen[r.expect("joinset task")] = true;
    }
    assert!(seen.iter().all(|&s| s), "JoinSet dropped a task");
    println!("JoinSet: 100 tasks joined as they finished");
}

/// spawn_local runs !Send work on the calling thread.
async fn local_set() {
    let local = tokio::task::LocalSet::new();
    let out = local
        .run_until(async {
            let shared = Rc::new(RefCell::new(Vec::new()));
            let mut handles = Vec::new();
            for i in 0..8 {
                let shared = shared.clone();
                handles.push(tokio::task::spawn_local(async move {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    shared.borrow_mut().push(i);
                }));
            }
            for h in handles {
                h.await.expect("local task");
            }
            let mut v = shared.borrow().clone();
            v.sort_unstable();
            v
        })
        .await;
    assert_eq!(out, (0..8).collect::<Vec<i32>>());
    println!("LocalSet: 8 !Send tasks shared an Rc");
}

async fn cancellation() {
    let h = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        unreachable!("aborted task kept going");
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    h.abort();
    let err = h.await.expect_err("aborted task reported success");
    assert!(err.is_cancelled(), "abort produced {err:?}");

    // The other half of this, JoinError::is_panic, has no test here on purpose.
    // The target sets panic-strategy: abort, so a panicking task takes the
    // whole process with it instead of coming back as a JoinError.

    // Aborting one task leaves the runtime usable.
    assert_eq!(tokio::spawn(async { 1 + 1 }).await.expect("post-abort task"), 2);

    // Dropping a JoinHandle detaches rather than cancels.
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = flag.clone();
    drop(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        f.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(flag.load(std::sync::atomic::Ordering::SeqCst), "a detached task never ran");
    println!("cancellation: abort reported, detached task still ran");
}

async fn yielding() {
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let counter = counter.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..100 {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        }));
    }
    for t in tasks {
        t.await.expect("task");
    }
    assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1600);
    println!("yield_now: 1600 reschedules");
}

/// AsyncFd is how crates put their own file descriptors on the runtime, so it
/// exercises registering something that isn't a socket with the poll selector.
async fn async_fd() {
    struct Fd(RawFd);
    impl AsRawFd for Fd {
        fn as_raw_fd(&self) -> RawFd {
            self.0
        }
    }
    impl Drop for Fd {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }

    let mut fds = [0 as RawFd; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe failed");
    let (r, w) = (Fd(fds[0]), Fd(fds[1]));
    for fd in [r.0, w.0] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_ne!(flags, -1, "F_GETFL failed");
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            -1,
            "F_SETFL failed"
        );
    }

    let reader = AsyncFd::new(r).expect("register the read end");

    // Nothing is in the pipe yet, so the first readable() has to park until the
    // writer below actually writes.
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let msg = b"through the pipe";
        assert_eq!(
            unsafe { libc::write(w.as_raw_fd(), msg.as_ptr().cast(), msg.len()) },
            msg.len() as isize,
            "pipe write failed"
        );
    });

    let started = Instant::now();
    let mut buf = [0u8; 64];
    let n = loop {
        let mut guard = reader.readable().await.expect("readable");
        match guard.try_io(|inner| {
            let n =
                unsafe { libc::read(inner.get_ref().as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
        }) {
            Ok(r) => break r.expect("pipe read"),
            Err(_would_block) => continue,
        }
    };
    writer.await.expect("writer");

    assert_eq!(&buf[..n], b"through the pipe");
    assert!(started.elapsed() >= Duration::from_millis(40), "readable() returned before the write");
    println!("AsyncFd: parked on a raw pipe fd and woke on the write");
}

/// The single-threaded runtime drives the IO driver from the same thread that
/// runs the tasks, which is a different code path from the multi-thread one.
fn current_thread() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current_thread runtime");

    rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 32];
            let n = sock.read(&mut buf).await.expect("server read");
            sock.write_all(&buf[..n]).await.expect("server write");
        });

        let mut sock = tokio::net::TcpStream::connect(addr).await.expect("connect");
        sock.write_all(b"single threaded").await.expect("write");
        let mut back = [0u8; 15];
        sock.read_exact(&mut back).await.expect("read");
        assert_eq!(&back, b"single threaded");

        tokio::time::sleep(Duration::from_millis(20)).await;
    });

    println!("current_thread runtime: TCP roundtrip and a timer on one thread");
}
