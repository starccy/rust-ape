//! The open/fcntl flag shim, exercised end to end.
//!
//! File flags have the same problem as errno. musl's values are baked in at
//! compile time while each host wants its own; shim/open.c translates them
//! once at the libc boundary, so std keeps using plain `libc::O_*`. These
//! scenarios each push a different flag family through it. The append cases
//! matter most, since a wrong O_APPEND bit doesn't error but silently
//! truncates (Linux's O_APPEND is XNU's O_TRUNC).

use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::time::{Duration, SystemTime};

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");

    // O_CREAT|O_TRUNC, then O_APPEND through std's append(true).
    let path = dir.path().join("appended");
    fs::write(&path, b"aaa").expect("write");
    let mut f = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open append");
    f.write_all(b"bbb").expect("append write");
    drop(f);
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "aaabbb",
        "append truncated or misplaced"
    );
    println!("std append ok");

    // The same bit straight from libc, the way third-party crates pass it.
    // Pre-shim this was the unfixable case: no std patch could reach it.
    let mut f = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_APPEND)
        .open(&path)
        .expect("open custom O_APPEND");
    f.write_all(b"ccc").expect("custom append write");
    drop(f);
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "aaabbbccc",
        "custom_flags O_APPEND ignored"
    );
    println!("custom_flags O_APPEND ok");

    // try_clone = fcntl(F_DUPFD_CLOEXEC). musl's 1030 never matched cosmo's
    // runtime command number, so this was quietly broken off Linux until now.
    let mut a = File::open(&path).expect("open");
    let mut b = a.try_clone().expect("try_clone");
    let mut buf = [0u8; 3];
    a.read_exact(&mut buf).expect("read via original");
    assert_eq!(&buf, b"aaa");
    b.read_exact(&mut buf).expect("read via clone");
    assert_eq!(&buf, b"bbb", "clone does not share the file offset");
    println!("fcntl F_DUPFD_CLOEXEC (try_clone) ok");

    // O_EXCL through create_new: must report "already there", not invent.
    let e = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect_err("create_new over an existing file");
    assert_eq!(
        e.raw_os_error(),
        Some(libc::EEXIST),
        "O_EXCL got lost in translation"
    );
    println!("O_CREAT|O_EXCL ok");

    for (name, flag) in [("O_SYNC", libc::O_SYNC), ("O_DSYNC", libc::O_DSYNC)] {
        let synced = dir.path().join(format!("synced-{name}"));
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(flag)
            .open(&synced)
            .unwrap_or_else(|e| panic!("open with {name}: {e}"));
        f.write_all(b"durable")
            .unwrap_or_else(|e| panic!("write with {name}: {e}"));
        drop(f);
        assert_eq!(
            fs::read_to_string(&synced).expect("read"),
            "durable",
            "{name} lost the write"
        );
        println!("custom_flags {name} ok");
    }

    // utimensat, whose timespec smuggles UTIME_OMIT (a platform constant) for
    // the field left unset. Wrong sentinel = EINVAL or a clobbered atime.
    let f = File::options()
        .write(true)
        .open(&path)
        .expect("open for times");
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    f.set_times(FileTimes::new().set_modified(stamp))
        .expect("set_times");
    drop(f);
    let got = fs::metadata(&path)
        .expect("metadata")
        .modified()
        .expect("modified");
    assert_eq!(got, stamp, "mtime did not stick");
    println!("utimensat UTIME_OMIT ok");

    // seek back and forth just to prove the fd is still sane after all that.
    let mut f = File::open(&path).expect("reopen");
    f.seek(SeekFrom::End(-3)).expect("seek");
    let mut tail = String::new();
    f.read_to_string(&mut tail).expect("read tail");
    assert_eq!(tail, "ccc");

    // unlinkat(AT_REMOVEDIR) underneath remove_dir_all.
    let tree = dir.path().join("t");
    fs::create_dir_all(tree.join("x/y")).expect("mkdirs");
    fs::write(tree.join("x/f"), b"f").expect("write");
    fs::remove_dir_all(&tree).expect("remove_dir_all");
    assert!(!tree.exists(), "remove_dir_all left the tree behind");
    println!("unlinkat AT_REMOVEDIR ok");

    // O_PATH names a file without opening it: fstat, fstatat and openat
    // work through the descriptor, read does not. The fork hands NT a
    // FILE_READ_ATTRIBUTES handle and XNU an O_EVTONLY one; before that it
    // was a plain read open, which needs read permission and blocks on a
    // fifo. O_NOFOLLOW on top opens a symlink as itself. Straight through
    // libc::open, since on musl targets O_ACCMODE contains O_PATH (it is
    // O_SEARCH there) and std's custom_flags masks it away.
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).expect("mkdir sub");
    fs::write(sub.join("inner"), b"i").expect("write inner");
    let open_path = |p: &std::path::Path, extra: i32| -> File {
        let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC | extra) };
        assert!(
            fd >= 0,
            "open({}, O_PATH): {}",
            p.display(),
            std::io::Error::last_os_error()
        );
        unsafe { File::from_raw_fd(fd) }
    };
    let d = open_path(&sub, libc::O_DIRECTORY);
    assert!(d.metadata().expect("fstat O_PATH dir").is_dir());
    let fl = unsafe { libc::fcntl(d.as_raw_fd(), libc::F_GETFL) };
    assert!(fl != -1 && fl & libc::O_PATH != 0, "F_GETFL lost O_PATH");
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe { libc::fstatat(d.as_raw_fd(), c"inner".as_ptr(), st.as_mut_ptr(), 0) };
    assert_eq!(rc, 0, "fstatat through an O_PATH dirfd");
    let mut byte = [0u8; 1];
    let err = (&d).read(&mut byte).expect_err("read on an O_PATH fd");
    // XNU has no descriptor kind that refuses reads, so there the open is
    // O_EVTONLY and reading the directory fails as any directory read does
    let want = if ape::is_xnu() {
        libc::EISDIR
    } else {
        libc::EBADF
    };
    assert_eq!(err.raw_os_error(), Some(want), "read gave {err}");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink("sub/inner", &link).expect("symlink");
    let l = open_path(&link, libc::O_NOFOLLOW);
    assert!(
        l.metadata().expect("fstat link").file_type().is_symlink(),
        "O_PATH|O_NOFOLLOW followed the link"
    );
    println!("O_PATH ok");

    println!("\nopen flags ok");
}
