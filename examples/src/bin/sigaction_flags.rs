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

use std::io::Read;
use std::mem;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static PLAIN_SIG: AtomicI32 = AtomicI32::new(0);
static INFO_SIG: AtomicI32 = AtomicI32::new(0);
static INFO_SI_SIGNO: AtomicI32 = AtomicI32::new(0);
/// Which thread the masking test's signal ran its handler on.
static CAUGHT_ON: AtomicU64 = AtomicU64::new(0);
static WORKER_READY: AtomicBool = AtomicBool::new(false);

extern "C" fn plain_handler(sig: i32) {
    PLAIN_SIG.store(sig, Ordering::SeqCst);
}

extern "C" fn thread_handler(_sig: i32) {
    CAUGHT_ON.store(tid(), Ordering::SeqCst);
}

fn tid() -> u64 {
    unsafe { libc::pthread_self() as usize as u64 }
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

/// Where the stack pointer lands inside the guard page decides whether the
/// overflow is reportable at all on Windows: the kernel writes the exception
/// record BELOW the faulting sp before dispatching it, and cosmo maps a
/// thread stack as exactly guard+stack, so a low sp put that write past the
/// end of the mapping and killed the process with nothing printed. Every
/// frame here is a whole page, so the offset is whatever the thread pushed
/// before recursing and stays put for the whole descent — these pads walk it
/// across the page so both halves get covered.
fn overflow_with_pad(pad: usize) {
    macro_rules! pad_then_overflow {
        ($n:expr) => {{
            let p = [0u8; $n];
            std::hint::black_box(&p);
            overflow_the_stack();
            std::hint::black_box(&p);
        }};
    }
    match pad {
        0 => overflow_the_stack(),
        1 => pad_then_overflow!(1024),
        2 => pad_then_overflow!(2048),
        3 => pad_then_overflow!(3072),
        _ => pad_then_overflow!(3900),
    }
}

const PADS: usize = 5;

fn main() {
    if let Some(pad) = std::env::var_os("SIGACTION_FLAGS_OVERFLOW") {
        let pad: usize = pad.to_string_lossy().parse().expect("pad index");
        std::thread::spawn(move || overflow_with_pad(pad)).join().unwrap();
        unreachable!();
    }

    // Child mode for the SIGTERM test. Nothing is installed here on purpose:
    // the default disposition is what the parent is checking for.
    if std::env::var_os("SIGACTION_FLAGS_SLEEP").is_some() {
        println!("ready");
        std::io::Write::flush(&mut std::io::stdout()).expect("flush");
        std::thread::sleep(Duration::from_secs(30));
        std::process::exit(9);
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

    // argv[0], not current_exe(): under the APE loader /proc/self/exe points
    // at the loader, and spawning that alone just prints its usage screen.
    let self_path = std::env::args().next().expect("argv[0]");

    // Delivery into another process. The child installs no handler, so
    // SIGTERM takes its default disposition, and the number has to survive
    // the trip back out through the exit status.
    let mut child = std::process::Command::new(&self_path)
        .env("SIGACTION_FLAGS_SLEEP", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the sleeper");
    let mut hello = [0u8; 6];
    child
        .stdout
        .as_mut()
        .expect("child stdout")
        .read_exact(&mut hello)
        .expect("the child never said ready");
    assert_eq!(&hello, b"ready\n", "the child said something else");
    let pid = child.id() as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "kill(pid, 0) says the child isn't there");
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "kill(SIGTERM) failed");
    let status = child.wait().expect("wait for the sleeper");
    assert!(!status.success(), "the child outlived SIGTERM: {status}");
    let sig = status.signal().expect("the exit status carries no signal at all");
    assert_eq!(sig, libc::SIGTERM, "the exit status named the wrong signal");
    println!("child killed by SIGTERM({sig}), and its exit status says so");

    // pthread_sigmask: a process-directed signal has to find a thread that
    // hasn't blocked it. The main thread blocks SIGUSR1, a worker lifts it,
    // and the handler records which one it ran on.
    let main_tid = tid();
    unsafe {
        let mut set: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()), 0);
        let mut act: libc::sigaction = mem::zeroed();
        act.sa_sigaction = addr1(thread_handler);
        assert_eq!(libc::sigaction(libc::SIGUSR1, &act, std::ptr::null_mut()), 0);
    }
    let worker = std::thread::spawn(|| unsafe {
        // Threads inherit the mask, so this one has to unblock explicitly.
        let mut set: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        assert_eq!(libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()), 0);
        WORKER_READY.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        while CAUGHT_ON.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        tid()
    });
    while !WORKER_READY.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGUSR1) }, 0, "kill(self, SIGUSR1)");
    let worker_tid = worker.join().expect("worker thread");
    let caught = CAUGHT_ON.load(Ordering::SeqCst);
    let landed = if caught == 0 {
        "was never delivered".to_owned()
    } else if caught == worker_tid {
        "landed on the worker that unblocked it".to_owned()
    } else if caught == main_tid {
        "landed on the main thread, which had it blocked".to_owned()
    } else {
        format!("landed on a third thread ({caught:#x})")
    };
    println!("process-directed SIGUSR1 {landed}");
    // Held as a contract on Linux only. Off Linux cosmo emulates signals and
    // this line is here to report what it does, not to fail the run.
    if ape::is_linux() {
        assert_eq!(caught, worker_tid, "the thread mask was ignored");
    }
    unsafe {
        let mut set: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        assert_eq!(libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()), 0);
    }

    // The realtime range. musl reaches SIGRTMIN/SIGRTMAX through
    // __libc_current_sigrt{min,max}, which cosmo has as variables rather than
    // functions, so shim/signal.c defines them, answering with musl's numbers
    // because everything above the shim is Linux-coded. Those two bounds are
    // the shim's own contract and are asserted everywhere.
    //
    // Delivery is a different question, and the note at the top of that file
    // already answers it: the realtime range doesn't work off Linux. XNU
    // stops at signal 31 and rejects sigaction(35) with EINVAL. Windows takes
    // the raise, which is luck rather than support. So only Linux is held to
    // the rest; elsewhere this reports what the host did.
    let rtmin = libc::SIGRTMIN();
    let rtmax = libc::SIGRTMAX();
    assert_eq!((rtmin, rtmax), (35, 64), "the shim's realtime range drifted from musl's");
    unsafe {
        let mut act: libc::sigaction = mem::zeroed();
        act.sa_sigaction = addr1(plain_handler);
        PLAIN_SIG.store(0, Ordering::SeqCst);
        let install = libc::sigaction(rtmin, &act, std::ptr::null_mut());
        let install_err = std::io::Error::last_os_error();
        let raised = install == 0 && libc::raise(rtmin) == 0;
        let seen = PLAIN_SIG.load(Ordering::SeqCst);
        if ape::is_linux() {
            assert_eq!(install, 0, "sigaction(SIGRTMIN) failed: {install_err}");
            assert!(raised, "raise(SIGRTMIN) failed: {}", std::io::Error::last_os_error());
            assert_eq!(seen, rtmin, "the realtime handler was handed a different signum");
        }
        if seen == rtmin {
            println!("SIGRTMIN({rtmin})..SIGRTMAX({rtmax}), raise delivered and coded as {seen}");
        } else if install != 0 {
            println!("SIGRTMIN({rtmin})..SIGRTMAX({rtmax}) bounds ok, sigaction refused: {install_err}");
        } else {
            println!("SIGRTMIN({rtmin})..SIGRTMAX({rtmax}) installed, but the handler never ran");
        }
    }

    // std sets SIGPIPE to SIG_IGN at startup, so writing into a pipe nobody
    // is holding open has to come back as EPIPE instead of killing us.
    unsafe {
        let mut fds = [0i32; 2];
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0, "pipe");
        assert_eq!(libc::close(fds[0]), 0, "close the read end");
        let n = libc::write(fds[1], b"x".as_ptr().cast(), 1);
        let raw = std::io::Error::last_os_error().raw_os_error();
        assert_eq!(n, -1, "writing into a reader-less pipe reported success");
        assert_eq!(raw, Some(libc::EPIPE), "expected EPIPE from a reader-less pipe");
        assert_eq!(libc::close(fds[1]), 0);
        println!("write to a reader-less pipe gave EPIPE, not a SIGPIPE death");
    }

    // Stack overflow detection: SA_ONSTACK + sigaltstack + si_addr, the whole
    // chain, in a child so the abort doesn't take this process down.
    // --strace: eaten by cosmo before argv, so the child behaves identically
    // but narrates its syscalls to stderr. The message check below is a
    // contains(), so the extra lines are free — and when the detector goes
    // blind on some host, the tail of the trace IS the crash-site forensics
    // (this failure only reproduces on CI, never locally).
    for pad in 0..PADS {
        let out = std::process::Command::new(&self_path)
            .arg("--strace")
            .env("SIGACTION_FLAGS_OVERFLOW", pad.to_string())
            .output()
            .expect("spawn self");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "overflow child {pad} exited cleanly?!");
        let tail = &stderr[stderr.len().saturating_sub(3000)..];
        assert!(
            stderr.contains("overflowed its stack"),
            "pad {pad}: no overflow message; std's detector is blind at this \
             stack-pointer offset (child {:?}, stderr tail: {tail:?})",
            out.status
        );
    }
    println!("stack overflow detected by std at {PADS} sp offsets: SA_ONSTACK/SA_SIGINFO/si_addr all intact");

    println!("\nsigaction flags ok");
}
