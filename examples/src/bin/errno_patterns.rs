//! Error-handling patterns that hard-code Linux errno values, the way real
//! crates do. Before the errno shim every block here took the wrong branch
//! on Windows, because the raw values were the host's (EEXIST arrived as
//! NT's 80, EAGAIN as WSA's 10035). With the shim they are Linux-coded on
//! every host. errno_codes.rs tests the same translation assertion-style;
//! this file exercises it through realistic code.

use std::fs::OpenOptions;
use std::net::{TcpListener, TcpStream};
use std::path::Path;

/// The lockfile idiom: create_new, and EEXIST specifically means "somebody
/// else holds it". Pre-shim Windows: raw 80, so the EEXIST arm never matched
/// and holding the lock looked like an I/O failure.
fn try_lock(path: &Path) -> bool {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => true,
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => false,
        Err(e) => panic!("lockfile: expected raw EEXIST({}), got {:?}: {e}", libc::EEXIST, e.raw_os_error()),
    }
}

/// The poll-loop accept idiom: EAGAIN means "no client yet, come back
/// later", anything else is fatal. Pre-shim Windows: raw 10035, which took
/// the fatal arm the first time the loop spun.
fn accept_when_ready(listener: &TcpListener) -> TcpStream {
    let mut spins = 0;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => {
                spins += 1;
                assert!(spins < 10_000, "client never showed up");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => panic!("accept: expected raw EAGAIN({}), got {:?}: {e}", libc::EAGAIN, e.raw_os_error()),
        }
    }
}

/// The path-probe idiom: ENOTDIR means "a file is in the way", anything else
/// is a real error. Pre-shim Windows: raw 3, fatal arm again.
fn blocked_by_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(_) => false,
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => true,
        Err(e) => panic!("probe: expected raw ENOTDIR({}), got {:?}: {e}", libc::ENOTDIR, e.raw_os_error()),
    }
}

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");

    // EEXIST: second lock attempt must be told "held", not "broken".
    let lock = dir.path().join("pid.lock");
    assert!(try_lock(&lock), "first lock failed outright");
    assert!(!try_lock(&lock), "second lock succeeded twice?!");
    println!("lockfile EEXIST branch ok");

    // EAGAIN: spin on a nonblocking accept. The empty phase must take the
    // EAGAIN branch (not the fatal one), and the loop must still notice the
    // client once it connects.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("set_nonblocking");
    match listener.accept() {
        Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => {}
        Ok(_) => panic!("accept found a client nobody started"),
        Err(e) => panic!("accept: expected raw EAGAIN({}), got {:?}: {e}", libc::EAGAIN, e.raw_os_error()),
    }
    let _tx = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let _rx = accept_when_ready(&listener);
    println!("nonblocking accept EAGAIN branch ok");

    // ENOTDIR: probe a path that runs through a regular file.
    let file = dir.path().join("plain");
    std::fs::write(&file, b"x").expect("write");
    assert!(blocked_by_file(&file.join("below")), "probe missed the file in the way");
    println!("path probe ENOTDIR branch ok");

    // EBADF through a raw libc call, the third-party-crate route: no std
    // error machinery involved until we read errno back.
    let rc = unsafe { libc::write(-1, b"x".as_ptr().cast(), 1) };
    let raw = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(rc, -1);
    assert_eq!(raw, Some(libc::EBADF), "expected raw EBADF({}) from fd -1", libc::EBADF);
    println!("raw libc EBADF ok");

    // connect_timeout drives the (poll, getsockopt(SO_ERROR)) pair: the
    // payload of SO_ERROR is itself an errno, in host coding, which the shim
    // translates back. Pre-shim: raw 10061 on Windows, and the refused
    // connect surfaced as Uncategorized.
    let e = TcpStream::connect_timeout(
        &"127.0.0.1:1".parse().unwrap(), // port 1: nothing listens there
        std::time::Duration::from_secs(5),
    )
    .expect_err("connect to a dead port");
    assert_eq!(e.raw_os_error(), Some(libc::ECONNREFUSED),
        "SO_ERROR payload not translated: {e}");
    println!("connect_timeout SO_ERROR branch ok");

    // And the human-facing text: pre-shim an untranslated code rendered as
    // the bare "os error 80" fallback.
    let msg = std::io::Error::from_raw_os_error(libc::EEXIST).to_string();
    println!("EEXIST renders as: {msg}");
    assert!(!msg.starts_with("os error"), "strerror_r fell back to the raw-code message");

    println!("\nerrno patterns ok");
}
