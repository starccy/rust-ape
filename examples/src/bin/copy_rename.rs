//! copy_file_range, renameat2 with RENAME_NOREPLACE and RENAME_EXCHANGE,
//! sendfile into a file or a pipe, and pthread_getaffinity_np, all through
//! the libc crate. Linux and macOS run their kernel's own calls where they
//! have them; everywhere else the fork builds the same thing from
//! read/write loops, a hard link plus unlink or three renames, and on
//! Windows the thread's basic information block.
//!
//! The musl flavour of the libc crate declares renameat2 and sendfile but
//! not copy_file_range or pthread_getaffinity_np, so those two are declared
//! here the way a crate that wants them would.

use std::ffi::CString;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;

unsafe extern "C" {
    fn copy_file_range(
        fd_in: libc::c_int,
        off_in: *mut libc::off64_t,
        fd_out: libc::c_int,
        off_out: *mut libc::off64_t,
        len: libc::size_t,
        flags: libc::c_uint,
    ) -> libc::ssize_t;
    fn pthread_getaffinity_np(
        thread: libc::pthread_t,
        cpusetsize: libc::size_t,
        cpuset: *mut libc::cpu_set_t,
    ) -> libc::c_int;
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn cpath(p: &Path) -> CString {
    CString::new(p.to_str().unwrap()).unwrap()
}

fn pattern(n: usize, seed: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i.wrapping_mul(seed) + (i >> 8)) as u8)
        .collect()
}

fn copy_checks(dir: &Path) {
    let src = pattern(300_000, 7);
    let sp = dir.join("cfr-src");
    let dp = dir.join("cfr-dst");
    std::fs::write(&sp, &src).unwrap();
    let mut fin = std::fs::File::open(&sp).unwrap();
    let mut fout = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&dp)
        .unwrap();

    let mut done = 0usize;
    let mut calls = 0;
    while done < src.len() {
        let r = unsafe {
            copy_file_range(
                fin.as_raw_fd(),
                std::ptr::null_mut(),
                fout.as_raw_fd(),
                std::ptr::null_mut(),
                src.len() - done,
                0,
            )
        };
        assert!(
            r > 0,
            "copy_file_range gave {r} errno {} after {done}",
            errno()
        );
        done += r as usize;
        calls += 1;
    }
    assert_eq!(
        fin.stream_position().unwrap(),
        src.len() as u64,
        "source position"
    );
    assert_eq!(
        fout.stream_position().unwrap(),
        src.len() as u64,
        "destination position"
    );
    assert_eq!(std::fs::read(&dp).unwrap(), src, "copy differs");

    let r = unsafe {
        copy_file_range(
            fin.as_raw_fd(),
            std::ptr::null_mut(),
            fout.as_raw_fd(),
            std::ptr::null_mut(),
            100,
            0,
        )
    };
    assert_eq!(r, 0, "at EOF errno {}", errno());

    let mut io: libc::off64_t = 1000;
    let mut oo: libc::off64_t = 5000;
    let r =
        unsafe { copy_file_range(fin.as_raw_fd(), &mut io, fout.as_raw_fd(), &mut oo, 2000, 0) };
    assert_eq!(r, 2000, "offset copy errno {}", errno());
    assert_eq!((io, oo), (3000, 7000), "offsets after the copy");
    assert_eq!(
        fin.stream_position().unwrap(),
        src.len() as u64,
        "explicit offsets moved the position"
    );
    let mut back = vec![0u8; 2000];
    fout.seek(SeekFrom::Start(5000)).unwrap();
    fout.read_exact(&mut back).unwrap();
    assert_eq!(back, src[1000..3000], "offset copy landed elsewhere");

    let r = unsafe {
        copy_file_range(
            fin.as_raw_fd(),
            std::ptr::null_mut(),
            fout.as_raw_fd(),
            std::ptr::null_mut(),
            10,
            1,
        )
    };
    assert!(
        r == -1 && errno() == libc::EINVAL,
        "flags gave {r}/{}",
        errno()
    );

    // std::fs::copy has its own fallback, but it should agree with ours
    let cp = dir.join("cfr-std");
    assert_eq!(std::fs::copy(&sp, &cp).unwrap(), src.len() as u64);
    assert_eq!(std::fs::read(&cp).unwrap(), src);
    println!(
        "copy_file_range: {} bytes in {calls} call(s), EOF, offsets, EINVAL, std::fs::copy ok",
        src.len()
    );
}

fn rename_checks(dir: &Path) {
    let a = dir.join("rn-a");
    let b = dir.join("rn-b");
    let c = dir.join("rn-c");
    std::fs::write(&a, "AAA").unwrap();
    std::fs::write(&b, "BB").unwrap();
    let (ca, cb, cc) = (cpath(&a), cpath(&b), cpath(&c));

    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    assert!(
        r == -1 && errno() == libc::EEXIST,
        "NOREPLACE onto existing gave {r}/{}",
        errno()
    );
    assert_eq!(std::fs::read(&a).unwrap(), b"AAA");
    assert_eq!(std::fs::read(&b).unwrap(), b"BB");

    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cc.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    assert_eq!(r, 0, "NOREPLACE onto a free name errno {}", errno());
    assert!(!a.exists() && std::fs::read(&c).unwrap() == b"AAA");

    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            cc.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    assert_eq!(r, 0, "EXCHANGE errno {}", errno());
    assert_eq!(std::fs::read(&b).unwrap(), b"AAA");
    assert_eq!(std::fs::read(&c).unwrap(), b"BB");

    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    assert!(
        r == -1 && errno() == libc::ENOENT,
        "EXCHANGE with a missing name gave {r}/{}",
        errno()
    );
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::AT_FDCWD,
            cc.as_ptr(),
            libc::RENAME_NOREPLACE | libc::RENAME_EXCHANGE,
        )
    };
    assert!(
        r == -1 && errno() == libc::EINVAL,
        "both flags gave {r}/{}",
        errno()
    );

    let da = dir.join("rn-da");
    let db = dir.join("rn-db");
    let dc = dir.join("rn-dc");
    std::fs::create_dir(&da).unwrap();
    std::fs::create_dir(&db).unwrap();
    std::fs::write(da.join("inner"), "x").unwrap();
    let (cda, cdb, cdc) = (cpath(&da), cpath(&db), cpath(&dc));
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            cda.as_ptr(),
            libc::AT_FDCWD,
            cdb.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    assert!(
        r == -1 && errno() == libc::EEXIST,
        "NOREPLACE dir onto dir gave {r}/{}",
        errno()
    );
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            cda.as_ptr(),
            libc::AT_FDCWD,
            cdc.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    assert_eq!(r, 0, "NOREPLACE dir onto a free name errno {}", errno());
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            cdc.as_ptr(),
            libc::AT_FDCWD,
            cdb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    assert_eq!(r, 0, "EXCHANGE dirs errno {}", errno());
    assert!(
        db.join("inner").exists(),
        "inner did not travel with its directory"
    );
    assert!(dc.is_dir() && std::fs::read_dir(&dc).unwrap().next().is_none());
    println!("renameat2: NOREPLACE EEXIST/ok, EXCHANGE files and dirs, ENOENT, EINVAL ok");
}

fn sendfile_checks(dir: &Path) {
    let src = pattern(100_000, 3);
    let sp = dir.join("sf-src");
    let dp = dir.join("sf-dst");
    std::fs::write(&sp, &src).unwrap();
    let mut fin = std::fs::File::open(&sp).unwrap();
    let fout = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&dp)
        .unwrap();

    let mut done = 0usize;
    while done < src.len() {
        let r = unsafe {
            libc::sendfile(
                fout.as_raw_fd(),
                fin.as_raw_fd(),
                std::ptr::null_mut(),
                src.len() - done,
            )
        };
        assert!(
            r > 0,
            "sendfile file->file gave {r} errno {} after {done}",
            errno()
        );
        done += r as usize;
    }
    assert_eq!(std::fs::read(&dp).unwrap(), src, "sendfile copy differs");

    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let mut rd = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let wr = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    let mut off: libc::off_t = 40_000;
    let r = unsafe { libc::sendfile(wr.as_raw_fd(), fin.as_raw_fd(), &mut off, 100) };
    assert_eq!(r, 100, "sendfile file->pipe errno {}", errno());
    assert_eq!(off, 40_100);
    assert_eq!(
        fin.stream_position().unwrap(),
        src.len() as u64,
        "explicit offset moved the position"
    );
    let mut got = [0u8; 100];
    rd.read_exact(&mut got).unwrap();
    assert_eq!(got[..], src[40_000..40_100]);
    drop(wr);
    let _ = fin.write_all(b"");
    println!(
        "sendfile: file->file {} bytes, file->pipe with offset ok",
        src.len()
    );
}

fn affinity_checks() {
    let size = std::mem::size_of::<libc::cpu_set_t>();
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let r = unsafe { pthread_getaffinity_np(libc::pthread_self(), size, &mut set) };
    assert_eq!(
        r, 0,
        "pthread_getaffinity_np returned {r} (cpu_set_t is {size} bytes)"
    );
    let n = unsafe { libc::CPU_COUNT(&set) };
    assert!(n >= 1, "no cpu in the mask");

    let mut pinned = "not pinned (single cpu)".to_string();
    if n >= 2 {
        let first = (0..libc::CPU_SETSIZE as usize)
            .find(|&i| unsafe { libc::CPU_ISSET(i, &set) })
            .unwrap();
        let mut one: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::CPU_ZERO(&mut one);
            libc::CPU_SET(first, &mut one);
        }
        let r = unsafe { libc::pthread_setaffinity_np(libc::pthread_self(), size, &one) };
        if r == 0 {
            let mut back: libc::cpu_set_t = unsafe { std::mem::zeroed() };
            let r = unsafe { pthread_getaffinity_np(libc::pthread_self(), size, &mut back) };
            assert_eq!(r, 0);
            assert!(
                unsafe { libc::CPU_COUNT(&back) } == 1 && unsafe { libc::CPU_ISSET(first, &back) },
                "pinned to {first} but read back {} cpus",
                unsafe { libc::CPU_COUNT(&back) }
            );
            unsafe { libc::pthread_setaffinity_np(libc::pthread_self(), size, &set) };
            pinned = format!("pinned to cpu {first} and read back");
        } else {
            // macOS has no thread affinity to set
            pinned = format!("set not supported here (errno {r})");
        }
    }
    println!("pthread_getaffinity_np: {n} cpu(s) allowed, {pinned}");
}

fn main() {
    let dir = std::env::temp_dir().join(format!("copy_rename-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    copy_checks(&dir);
    rename_checks(&dir);
    sendfile_checks(&dir);
    affinity_checks();
    std::fs::remove_dir_all(&dir).unwrap();
}
