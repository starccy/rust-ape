//! File-lock (fcntl F_SETLK/F_GETLK) round-trip through the shim.
//!
//! l_type carries F_RDLCK/F_WRLCK/F_UNLCK, which are runtime constants on
//! the cosmo side; shim/open.c translates them. struct flock itself passes
//! through untouched. POSIX record locks are held per process, so the
//! F_GETLK observer must be a child process.

use std::io::Write;
use std::os::fd::AsRawFd;

fn flk(ty: libc::c_int) -> libc::flock {
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = ty as libc::c_short;
    fl.l_whence = libc::SEEK_SET as libc::c_short;
    fl.l_start = 0;
    fl.l_len = 10;
    fl
}

fn run_child(exe: &str, flag: &str, path: &str) {
    let st = std::process::Command::new(exe)
        .args([flag, path])
        .status()
        .expect("spawn child");
    assert!(st.success(), "child {flag} failed");
}

fn child(flag: &str, path: &str) {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("child open");
    let fd = f.as_raw_fd();
    match flag {
        "--locked" => {
            // F_GETLK must report the parent's write lock, in musl coding.
            let mut q = flk(libc::F_WRLCK);
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETLK, &mut q) }, 0, "F_GETLK");
            assert_eq!(
                q.l_type,
                libc::F_WRLCK as libc::c_short,
                "expected the parent's write lock back, got l_type={}",
                q.l_type
            );
            // NT's lock emulation can't name the holder and reports -1.
            assert!(q.l_pid > 0 || q.l_pid == -1, "bogus l_pid {}", q.l_pid);
            // Taking a conflicting lock must fail cleanly.
            let mut fl = flk(libc::F_WRLCK);
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETLK, &mut fl) },
                -1,
                "conflicting F_SETLK must fail"
            );
            let e = std::io::Error::last_os_error().raw_os_error().unwrap();
            assert!(
                e == libc::EAGAIN || e == libc::EACCES,
                "want EAGAIN/EACCES, got {e}"
            );
            // Past the locked range the file is free.
            let mut q2 = flk(libc::F_WRLCK);
            q2.l_start = 10;
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETLK, &mut q2) }, 0);
            assert_eq!(
                q2.l_type,
                libc::F_UNLCK as libc::c_short,
                "range past the lock should read back F_UNLCK"
            );
        }
        "--unlocked" => {
            let mut fl = flk(libc::F_WRLCK);
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETLK, &mut fl) },
                0,
                "lock after release: {:?}",
                std::io::Error::last_os_error()
            );
        }
        other => panic!("unknown flag {other}"),
    }
}

fn main() {
    let mut args = std::env::args();
    let self_exe = args.next().expect("argv[0]");
    if let Some(flag) = args.next() {
        let path = args.next().expect("path");
        child(&flag, &path);
        return;
    }

    let path = std::env::temp_dir().join(format!("rust_ape_flock_{}", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(b"0123456789abcdef").unwrap();
    let fd = f.as_raw_fd();

    // Hold a write lock on [0,10) and let a child inspect the conflict.
    let mut fl = flk(libc::F_WRLCK);
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETLK, &mut fl) },
        0,
        "F_SETLK: {:?}",
        std::io::Error::last_os_error()
    );
    run_child(&self_exe, "--locked", path.to_str().unwrap());

    // Release, then the child must be able to take the lock itself.
    let mut un = flk(libc::F_UNLCK);
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETLK, &mut un) }, 0, "F_UNLCK");
    run_child(&self_exe, "--unlocked", path.to_str().unwrap());

    drop(f);
    let _ = std::fs::remove_file(&path);
    println!("file_lock: all assertions passed");
}
