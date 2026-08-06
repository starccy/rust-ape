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
use std::time::{Duration, SystemTime};

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");

    // O_CREAT|O_TRUNC, then O_APPEND through std's append(true).
    let path = dir.path().join("appended");
    fs::write(&path, b"aaa").expect("write");
    let mut f = OpenOptions::new().append(true).open(&path).expect("open append");
    f.write_all(b"bbb").expect("append write");
    drop(f);
    assert_eq!(fs::read_to_string(&path).expect("read"), "aaabbb", "append truncated or misplaced");
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
    assert_eq!(fs::read_to_string(&path).expect("read"), "aaabbbccc", "custom_flags O_APPEND ignored");
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
    assert_eq!(e.raw_os_error(), Some(libc::EEXIST), "O_EXCL got lost in translation");
    println!("O_CREAT|O_EXCL ok");

    // utimensat, whose timespec smuggles UTIME_OMIT (a platform constant) for
    // the field left unset. Wrong sentinel = EINVAL or a clobbered atime.
    let f = File::options().write(true).open(&path).expect("open for times");
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    f.set_times(FileTimes::new().set_modified(stamp)).expect("set_times");
    drop(f);
    let got = fs::metadata(&path).expect("metadata").modified().expect("modified");
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

    println!("\nopen flags ok");
}
