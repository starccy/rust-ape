//! test the errno shim, exercised end to end.
//!
//! Rust code compares `raw_os_error()` against `libc::` constants, which are
//! Linux values, while the host reports errors in its own coding (EBADF is 9
//! on Linux, 6 on Windows). shim/errno.c translates at the boundary, so the
//! Rust world only ever sees Linux values. Each block below asserts that.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::net::TcpListener;

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");

    // ENOENT, the code every platform reports differently.
    let e = File::open(dir.path().join("missing")).expect_err("open of a missing file");
    assert_eq!(e.kind(), ErrorKind::NotFound, "wrong kind for a missing file: {e}");
    assert_eq!(e.raw_os_error(), Some(libc::ENOENT), "raw errno is not Linux ENOENT");
    println!("ENOENT: kind={:?} raw={:?}", e.kind(), e.raw_os_error());

    // EEXIST via create_new against a file that's already there.
    let existing = dir.path().join("existing");
    fs::write(&existing, b"x").expect("write");
    let e = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&existing)
        .expect_err("create_new over an existing file");
    assert_eq!(e.kind(), ErrorKind::AlreadyExists, "wrong kind for create_new: {e}");
    assert_eq!(e.raw_os_error(), Some(libc::EEXIST), "raw errno is not Linux EEXIST");
    println!("EEXIST: kind={:?} raw={:?}", e.kind(), e.raw_os_error());

    // ENOTDIR: a path that uses a file as a directory. This exact code was
    // the deliberately-uncovered case in the shim's proof of concept: it
    // passed through as Windows' raw 3 back then. Now it must not.
    let e = File::open(existing.join("below")).expect_err("open through a non-directory");
    assert_eq!(e.raw_os_error(), Some(libc::ENOTDIR), "raw errno is not Linux ENOTDIR");
    assert_eq!(e.kind(), ErrorKind::NotADirectory, "wrong kind for ENOTDIR: {e}");
    println!("ENOTDIR: kind={:?} raw={:?}", e.kind(), e.raw_os_error());

    // EBADF through a raw libc call, the same route third-party crates take.
    let rc = unsafe { libc::write(-1, b"x".as_ptr().cast(), 1) };
    assert_eq!(rc, -1, "write to fd -1 somehow succeeded");
    let e = std::io::Error::last_os_error();
    assert_eq!(e.raw_os_error(), Some(libc::EBADF), "raw errno is not Linux EBADF");
    println!("EBADF: raw={:?}", e.raw_os_error());

    // EAGAIN from the network stack, which on Windows is a WSA code underneath.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("set_nonblocking");
    let e = listener.accept().expect_err("accept with nobody connecting");
    assert_eq!(e.kind(), ErrorKind::WouldBlock, "wrong kind for empty accept: {e}");
    assert_eq!(e.raw_os_error(), Some(libc::EAGAIN), "raw errno is not Linux EAGAIN");
    println!("EAGAIN: kind={:?} raw={:?}", e.kind(), e.raw_os_error());

    // readdir sets errno to 0 and checks it afterwards to tell "end of
    // directory" from "error", which is exactly the write-then-read pattern
    // the shim's copy has to keep in sync with the host side.
    for name in ["a", "b", "c"] {
        fs::write(dir.path().join(name), name).expect("write");
    }
    let mut seen: Vec<_> = fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|entry| entry.expect("dir entry").file_name().into_string().unwrap())
        .collect();
    seen.sort();
    assert_eq!(seen, ["a", "b", "c", "existing"], "read_dir lost or invented entries");
    println!("readdir errno=0 idiom: {} entries ok", seen.len());

    // The message path: strerror_r gets the host's coding back through the
    // shim. The text varies per platform; it just must not be the bare
    // "os error N" fallback that an untranslated code produces.
    let msg = std::io::Error::from_raw_os_error(libc::ENOENT).to_string();
    println!("ENOENT message: {msg}");
    assert!(
        !msg.starts_with("os error"),
        "strerror_r fell back to the raw-code message: {msg}"
    );

    println!("\nerrno codes ok");
}
