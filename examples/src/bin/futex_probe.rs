//! Pins the exact Linux futex contract of __ape_shim_syscall (shim/futex.c):
//! parking_lot only debug_asserts these errno values, so this is the suite's
//! release-mode guard. The timeout case doubles as the regression test for
//! the ETIME/ETIMEDOUT reverse-lookup collision on NT (both share one host
//! value there; table order must make ETIMEDOUT win).

use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;

fn probe(name: &str, want_r: i64, want_errno: Option<i32>, f: impl FnOnce() -> i64) {
    let r = f();
    let e = std::io::Error::last_os_error();
    println!("{name}: r={r} errno={:?} ({})", e.raw_os_error(), e);
    assert_eq!(r, want_r, "{name}: wrong return");
    if let Some(want) = want_errno {
        assert_eq!(e.raw_os_error(), Some(want), "{name}: wrong errno");
    }
}

fn main() {
    let futex = AtomicI32::new(0);
    let addr = &futex as *const AtomicI32;

    // Value mismatch: expect EAGAIN.
    probe("mismatch", -1, Some(libc::EAGAIN), || unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            999,
            std::ptr::null::<libc::timespec>(),
        ) as i64
    });

    // Short timed wait on the right value: expect ETIMEDOUT.
    futex.store(1, Ordering::SeqCst);
    let ts = libc::timespec { tv_sec: 0, tv_nsec: 50_000_000 };
    probe("timeout", -1, Some(libc::ETIMEDOUT), || unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            1,
            &ts,
        ) as i64
    });

    // Wake with no waiters: expect r=0.
    probe("wake0", 0, None, || unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1,
            std::ptr::null::<libc::timespec>(),
        ) as i64
    });

    // Untimed wait woken by another thread: expect r=0.
    let addr_num = addr as usize;
    let waker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let futex_ref = unsafe { &*(addr_num as *const AtomicI32) };
        futex_ref.store(2, Ordering::SeqCst);
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                addr_num as *const AtomicI32,
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                i32::MAX,
                std::ptr::null::<libc::timespec>(),
            );
        }
    });
    probe("woken", 0, None, || unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            1,
            std::ptr::null::<libc::timespec>(),
        ) as i64
    });
    waker.join().unwrap();
    println!("futex_probe: all assertions passed");
}
