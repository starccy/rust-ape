//! Diagnostic probe for cosmo's O_NONBLOCK semantics on sockets, per host.
//!
//! On NT, after fcntl(F_SETFL, O_NONBLOCK), accept() returns EAGAIN as
//! expected but send() into a full buffer was seen to block forever, which
//! is a latent hang for every async runtime. This probe reports rather than
//! asserts (exit code is always 0). The risky paths run in child processes
//! that the parent kills after a timeout, so a hang becomes a "HANG" verdict
//! line instead of a hung test suite.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

fn set_nonblock(fd: i32) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        assert!(fl >= 0);
        assert_eq!(libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK), 0);
    }
}

/// A connected pair where nobody reads the receiving end.
fn stuffed_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let tx = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (rx, _) = listener.accept().unwrap();
    (tx, rx)
}

/// Child mode: fill the socket via `send`, `write` or `send(MSG_DONTWAIT)`,
/// print the outcome. If the call blocks, we print nothing and the parent
/// times us out. The dontwait variant exists because cosmo's NT
/// __winsock_block honors MSG_DONTWAIT even where the send path ignores the
/// fd's O_NONBLOCK — it's the workaround hypothesis being probed.
fn child_fill(mode: &str) {
    let (tx, _rx) = stuffed_pair();
    let fd = tx.as_raw_fd();
    set_nonblock(fd);
    let buf = [0u8; 65536];
    let mut total: u64 = 0;
    for _ in 0..100_000 {
        let n = unsafe {
            match mode {
                "write" => libc::write(fd, buf.as_ptr().cast(), buf.len()),
                "dontwait" => libc::send(fd, buf.as_ptr().cast(), buf.len(), libc::MSG_DONTWAIT),
                _ => libc::send(fd, buf.as_ptr().cast(), buf.len(), 0),
            }
        };
        if n > 0 {
            total += n as u64;
            continue;
        }
        let raw = std::io::Error::last_os_error().raw_os_error();
        if raw == Some(libc::EAGAIN) {
            println!("EAGAIN after {}k", total / 1024);
        } else {
            println!("error raw={raw:?} after {}k", total / 1024);
        }
        return;
    }
    println!("never full after {}k?!", total / 1024);
}

/// Parent side: run self in child mode, wait up to 8s, report.
fn probe_child(mode: &str) -> String {
    let mut child = std::process::Command::new(std::env::args().next().unwrap())
        .env("NONBLOCK_PROBE_CHILD", mode)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn child");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match child.try_wait().unwrap() {
            Some(_) => {
                let mut out = String::new();
                child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
                return out.trim().to_string();
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return "HANG (killed after 8s)".into();
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Child mode: interleave poll(POLLOUT) with sends — does poll flip to
/// not-writable once the buffer is full? (That is the precondition for a
/// shim-side "poll gate" emulating nonblocking sends on a host that lacks
/// them.) Runs as a child because on such a host the send itself may block.
fn child_pollgate() {
    let (tx, _rx) = stuffed_pair();
    let fd = tx.as_raw_fd();
    set_nonblock(fd);
    let big = [0u8; 65536];
    let mut sent: u64 = 0;
    for _ in 0..100_000 {
        let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        let writable = unsafe { libc::poll(&mut pfd, 1, 0) };
        if writable == 0 {
            println!("not-writable reached after {}k (gate precondition holds)", sent / 1024);
            return;
        }
        let n = unsafe { libc::send(fd, big.as_ptr().cast(), big.len(), 0) };
        if n > 0 {
            sent += n as u64;
        } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) {
            println!("still 'writable' at first EAGAIN after {}k", sent / 1024);
            return;
        } else {
            println!("send error {:?}", std::io::Error::last_os_error());
            return;
        }
    }
    println!("never full after {}k?!", sent / 1024);
}

fn main() {
    if let Some(mode) = std::env::var_os("NONBLOCK_PROBE_CHILD") {
        if mode == "pollgate" {
            child_pollgate();
        } else {
            child_fill(mode.to_str().unwrap_or("send"));
        }
        return;
    }

    let mut ok = true;
    let mut check = |name: &str, verdict: String| {
        let good = verdict.starts_with("EAGAIN after");
        if !good {
            ok = false;
        }
        println!("{name}: {verdict}{}", if good { "" } else { "  <-- FAIL" });
    };
    check("send ", probe_child("send"));
    check("write", probe_child("write"));
    check("dontw", probe_child("dontwait"));

    // recv/read on an empty nonblocking socket — safe to do inline.
    let (_tx, rx) = stuffed_pair();
    let fd = rx.as_raw_fd();
    set_nonblock(fd);
    let mut buf = [0u8; 16];
    for (name, use_read) in [("recv ", false), ("read ", true)] {
        let n = unsafe {
            if use_read {
                libc::read(fd, buf.as_mut_ptr().cast(), buf.len())
            } else {
                libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0)
            }
        };
        let raw = std::io::Error::last_os_error().raw_os_error();
        if n == -1 && raw == Some(libc::EAGAIN) {
            println!("{name}: EAGAIN ok");
        } else {
            ok = false;
            println!("{name}: n={n} raw={raw:?} (expected -1/EAGAIN)  <-- FAIL");
        }
    }

    let gate = probe_child("pollgate");
    if !gate.starts_with("not-writable reached") {
        ok = false;
    }
    println!("poll : {gate}");

    if !ok {
        println!("\nnonblock semantics BROKEN on this host");
        std::process::exit(1);
    }
    println!("\nnonblock semantics ok");
}
