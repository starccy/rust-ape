//! The gaps `cargo xtask audit` found: constants that went to cosmo
//! untranslated because the function taking them wasn't redirected.

use std::fs::{self, File, FileTimes};
use std::os::fd::AsRawFd;
use std::time::{Duration, SystemTime};

fn main() {
    let dir = std::env::temp_dir().join(format!("audit_gaps_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("f");
    let f = File::create(&path).unwrap();

    // flock: LOCK_SH is 0 and LOCK_NB is 1 on NT
    let other = File::open(&path).unwrap();
    f.lock_shared().expect("lock_shared");
    other.try_lock_shared().expect("second shared lock");
    assert!(other.try_lock().is_err(), "exclusive lock over a shared one");
    other.unlock().unwrap();
    f.unlock().unwrap();
    f.try_lock().expect("try_lock");
    assert!(other.try_lock_shared().is_err(), "shared lock over an exclusive one");
    f.unlock().unwrap();
    println!("flock ok");

    // futimens: an omitted field carries UTIME_OMIT
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    f.set_times(FileTimes::new().set_accessed(old).set_modified(old)).unwrap();
    let newer = old + Duration::from_secs(3600);
    f.set_times(FileTimes::new().set_modified(newer)).unwrap();
    let meta = f.metadata().unwrap();
    assert_eq!(meta.modified().unwrap(), newer);
    assert_eq!(meta.accessed().unwrap(), old, "atime was not left alone");
    println!("futimens ok");

    // AT_FDCWD through the *at calls std doesn't use itself
    let cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&dir).unwrap();
    unsafe {
        assert_eq!(libc::mkdirat(libc::AT_FDCWD, c"sub".as_ptr(), 0o755), 0, "mkdirat");
        let linked = libc::symlinkat(c"sub".as_ptr(), libc::AT_FDCWD, c"link".as_ptr());
        if linked == 0 {
            let mut buf = [0u8; 16];
            let n = libc::readlinkat(libc::AT_FDCWD, c"link".as_ptr(), buf.as_mut_ptr().cast(), buf.len());
            assert_eq!(&buf[..n as usize], b"sub", "readlinkat");
        } else {
            // unprivileged symlinks can be off on Windows
            println!("symlinkat skipped: {}", std::io::Error::last_os_error());
        }
        let (uid, gid) = (libc::getuid(), libc::getgid());
        if libc::fchownat(libc::AT_FDCWD, c"f".as_ptr(), uid, gid, 0) != 0 {
            // cosmo has no chown on Windows at all
            let e = std::io::Error::last_os_error();
            assert_eq!(e.raw_os_error(), Some(libc::ENOSYS), "fchownat: {e}");
        }
        assert_eq!(libc::fchmodat(libc::AT_FDCWD, c"f".as_ptr(), 0o600, 0), 0, "fchmodat");
    }
    std::env::set_current_dir(cwd).unwrap();
    println!("at calls ok");

    // cfsetspeed works on musl's struct termios, not cosmo's
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        assert_eq!(libc::cfsetspeed(&mut t, libc::B115200), 0, "cfsetspeed");
        assert_eq!(libc::cfgetospeed(&t), libc::B115200, "cfsetspeed wrote somewhere else");
    }
    println!("cfsetspeed ok");

    // FIOCLEX goes through fcntl
    unsafe {
        let fd = f.as_raw_fd();
        assert_eq!(libc::ioctl(fd, libc::FIONCLEX), 0, "FIONCLEX");
        assert_eq!(libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
        assert_eq!(libc::ioctl(fd, libc::FIOCLEX), 0, "FIOCLEX");
        assert_ne!(libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
    }
    println!("fioclex ok");

    // cosmo's PTHREAD_STACK_MIN is 32768, musl's 2048
    let t = std::thread::Builder::new().stack_size(16 * 1024).spawn(|| 1 + 1).expect("small stack");
    assert_eq!(t.join().unwrap(), 2);
    println!("small stack ok");

    // f_flag arrives in Linux's ST_* bits: the temp dir is writable
    unsafe {
        let mut sv: libc::statvfs = std::mem::zeroed();
        let c = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        assert_eq!(libc::statvfs(c.as_ptr(), &mut sv), 0, "statvfs");
        assert_eq!(sv.f_flag & libc::ST_RDONLY, 0, "temp dir reported read-only: {:#x}", sv.f_flag);
    }
    println!("statvfs ok");

    // musl and cosmo number the mutex types and pshared values differently
    unsafe {
        let mut attr: libc::pthread_mutexattr_t = std::mem::zeroed();
        assert_eq!(libc::pthread_mutexattr_init(&mut attr), 0);
        assert_eq!(libc::pthread_mutexattr_settype(&mut attr, libc::PTHREAD_MUTEX_RECURSIVE), 0);
        let mut m: libc::pthread_mutex_t = std::mem::zeroed();
        assert_eq!(libc::pthread_mutex_init(&mut m, &attr), 0);
        assert_eq!(libc::pthread_mutex_lock(&mut m), 0);
        assert_eq!(libc::pthread_mutex_trylock(&mut m), 0, "a recursive mutex refused its owner");
        libc::pthread_mutex_unlock(&mut m);
        libc::pthread_mutex_unlock(&mut m);
        libc::pthread_mutex_destroy(&mut m);

        assert_eq!(libc::pthread_mutexattr_settype(&mut attr, libc::PTHREAD_MUTEX_NORMAL), 0);
        assert_eq!(libc::pthread_mutex_init(&mut m, &attr), 0);
        assert_eq!(libc::pthread_mutex_lock(&mut m), 0);
        assert_eq!(libc::pthread_mutex_trylock(&mut m), libc::EBUSY, "a normal mutex let its owner in twice");
        libc::pthread_mutex_unlock(&mut m);
        libc::pthread_mutex_destroy(&mut m);

        assert_eq!(libc::pthread_mutexattr_setpshared(&mut attr, libc::PTHREAD_PROCESS_SHARED), 0, "setpshared");
        let mut shared = -1;
        assert_eq!(libc::pthread_mutexattr_getpshared(&attr, &mut shared), 0);
        assert_eq!(shared, libc::PTHREAD_PROCESS_SHARED, "pshared read back as {shared}");
        libc::pthread_mutexattr_destroy(&mut attr);
    }
    println!("mutex attributes ok");

    // msync flags, and the 64-bit names going the same way as the plain ones
    let rw = fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
    unsafe {
        let fd = rw.as_raw_fd();
        assert_eq!(libc::ftruncate(fd, 4096), 0);
        let p = libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        assert_ne!(p, libc::MAP_FAILED, "mmap: {}", std::io::Error::last_os_error());
        *(p as *mut u8) = b'x';
        assert_eq!(libc::msync(p, 4096, libc::MS_SYNC), 0, "msync: {}", std::io::Error::last_os_error());
        libc::munmap(p, 4096);
        assert_eq!(libc::lseek64(fd, 0, libc::SEEK_END), 4096, "lseek64");
    }
    println!("msync ok");

    // RUSAGE_THREAD and the scheduling policy numbers
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_THREAD, &mut ru) != 0 {
            // macOS has no per-thread usage
            let e = std::io::Error::last_os_error();
            assert_eq!(e.raw_os_error(), Some(libc::EINVAL), "getrusage(RUSAGE_THREAD): {e}");
        }
        let policy = libc::sched_getscheduler(0);
        if policy >= 0 {
            assert!(
                [libc::SCHED_OTHER, libc::SCHED_FIFO, libc::SCHED_RR, libc::SCHED_BATCH, libc::SCHED_IDLE]
                    .contains(&(policy & !libc::SCHED_RESET_ON_FORK)),
                "sched_getscheduler gave {policy}, not one of Linux's numbers"
            );
        }
        println!("rusage and sched ok (policy {policy})");
    }

    drop(rw);
    drop(f);
    drop(other);
    fs::remove_dir_all(&dir).unwrap();
}
