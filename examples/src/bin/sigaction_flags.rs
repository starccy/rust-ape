//! The sigaction struct domain, end to end. Before the shim repacked the
//! struct, cosmo read sa_flags from offset 8 of musl's layout, which lands
//! in the middle of sa_mask, so SA_SIGINFO and SA_ONSTACK were silently
//! dropped on every host, Linux included, and std's stack-overflow
//! detection never worked.
//!
//! Covered here: a plain handler and a SA_SIGINFO one, both asserted to
//! receive the musl-coded signum; an oldact round-trip through both
//! repacking directions; sigprocmask block/unblock; and, in a child
//! process, a deliberate stack overflow expecting std's "has overflowed its
//! stack" abort.

use std::mem;
use std::sync::atomic::{AtomicI32, Ordering};

static PLAIN_SIG: AtomicI32 = AtomicI32::new(0);
static INFO_SIG: AtomicI32 = AtomicI32::new(0);
static INFO_SI_SIGNO: AtomicI32 = AtomicI32::new(0);

extern "C" fn plain_handler(sig: i32) {
    PLAIN_SIG.store(sig, Ordering::SeqCst);
}

extern "C" fn info_handler(sig: i32, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    INFO_SIG.store(sig, Ordering::SeqCst);
    if !info.is_null() {
        INFO_SI_SIGNO.store(unsafe { (*info).si_signo }, Ordering::SeqCst);
    }
}

// The two casts rustc wants spelled out: function item -> fn pointer -> usize.
fn addr1(f: extern "C" fn(i32)) -> usize {
    f as usize
}
fn addr3(f: extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void)) -> usize {
    f as usize
}

#[allow(unconditional_recursion)]
fn overflow_the_stack() {
    // A frame fat enough that the recursion hits the guard page quickly, and
    // opaque enough that it cannot be tail-called away.
    let mut buf = [0u8; 4096];
    buf[0] = std::hint::black_box(1);
    std::hint::black_box(&buf);
    overflow_the_stack();
}

fn main() {
    if std::env::var_os("SIGACTION_FLAGS_OVERFLOW").is_some() {
        std::thread::spawn(overflow_the_stack).join().unwrap();
        unreachable!();
    }

    unsafe {
        // Plain handler: the trampoline must hand back musl's signum.
        let mut act: libc::sigaction = mem::zeroed();
        act.sa_sigaction = addr1(plain_handler);
        assert_eq!(libc::sigaction(libc::SIGUSR1, &act, std::ptr::null_mut()), 0);
        assert_eq!(libc::raise(libc::SIGUSR1), 0);
        assert_eq!(PLAIN_SIG.load(Ordering::SeqCst), libc::SIGUSR1);
        println!("plain handler saw musl SIGUSR1({})", libc::SIGUSR1);

        // SA_SIGINFO handler: both the argument and si_signo must be musl's.
        let mut act: libc::sigaction = mem::zeroed();
        act.sa_sigaction = addr3(info_handler);
        act.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        assert_eq!(libc::sigaction(libc::SIGUSR2, &act, std::ptr::null_mut()), 0);
        assert_eq!(libc::raise(libc::SIGUSR2), 0);
        assert_eq!(INFO_SIG.load(Ordering::SeqCst), libc::SIGUSR2);
        assert_eq!(INFO_SI_SIGNO.load(Ordering::SeqCst), libc::SIGUSR2,
            "si_signo not translated back to musl coding");
        println!("SA_SIGINFO handler saw musl SIGUSR2({}) in arg and si_signo", libc::SIGUSR2);

        // oldact round-trip: reinstalling must report the previous user
        // handler (not the shim's trampoline) and the previous flags.
        let mut replacement: libc::sigaction = mem::zeroed();
        replacement.sa_sigaction = libc::SIG_IGN;
        let mut old: libc::sigaction = mem::zeroed();
        assert_eq!(libc::sigaction(libc::SIGUSR2, &replacement, &mut old), 0);
        assert_eq!(old.sa_sigaction, addr3(info_handler), "oldact leaked the trampoline");
        assert_ne!(old.sa_flags & libc::SA_SIGINFO, 0, "oldact lost SA_SIGINFO");
        assert_ne!(old.sa_flags & libc::SA_RESTART, 0, "oldact lost SA_RESTART");
        println!("oldact returned the user handler with flags {:#x}", old.sa_flags);

        // signal() rides the same machinery and must return the old handler.
        let prev = libc::signal(libc::SIGUSR1, libc::SIG_IGN);
        assert_eq!(prev, addr1(plain_handler), "signal() lost the previous handler");
        println!("signal() round-trip ok");

        // sigprocmask: block, raise (must stay pending), unblock (must fire).
        let mut set: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR2);
        let mut oldset: libc::sigset_t = mem::zeroed();
        let mut act: libc::sigaction = mem::zeroed();
        act.sa_sigaction = addr1(plain_handler);
        assert_eq!(libc::sigaction(libc::SIGUSR2, &act, std::ptr::null_mut()), 0);
        PLAIN_SIG.store(0, Ordering::SeqCst);
        assert_eq!(libc::sigprocmask(libc::SIG_BLOCK, &set, &mut oldset), 0);
        assert_eq!(libc::raise(libc::SIGUSR2), 0);
        assert_eq!(PLAIN_SIG.load(Ordering::SeqCst), 0, "signal delivered while blocked");
        assert_eq!(libc::sigprocmask(libc::SIG_SETMASK, &oldset, std::ptr::null_mut()), 0);
        assert_eq!(PLAIN_SIG.load(Ordering::SeqCst), libc::SIGUSR2, "pending signal not delivered on unblock");
        println!("sigprocmask block/unblock delivery order ok");
    }

    // Stack overflow detection: SA_ONSTACK + sigaltstack + si_addr, the whole
    // chain, in a child so the abort doesn't take this process down.
    // argv[0], not current_exe(): under the APE loader /proc/self/exe points
    // at the loader, and spawning that alone just prints its usage screen.
    let self_path = std::env::args().next().expect("argv[0]");
    let out = std::process::Command::new(self_path)
        .env("SIGACTION_FLAGS_OVERFLOW", "1")
        .output()
        .expect("spawn self");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "overflow child exited cleanly?!");
    assert!(
        stderr.contains("overflowed its stack"),
        "no overflow message; std's detector is still blind (child {:?}, stderr: {stderr:?})",
        out.status
    );
    println!("stack overflow detected by std: SA_ONSTACK/SA_SIGINFO/si_addr all intact");

    println!("\nsigaction flags ok");
}
