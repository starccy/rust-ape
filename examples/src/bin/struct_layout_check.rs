//! Struct-layout audit, Rust side.
//!
//! shim/layouts.c pins cosmo's struct offsets to literal numbers; this file
//! pins the libc crate's musl side to the same numbers, for both target
//! arches. If both compile, stat/dirent/timespec/flock/ucontext can keep
//! passing through the shim without repacking. It also guards the
//! hand-aligned aarch64 stat in patches/libc.patch; if that patch is wrong,
//! the assert here names the field that moved.

use std::mem::{align_of, offset_of, size_of};

const _: () = {
    assert!(offset_of!(libc::termios, c_iflag) == 0);
    assert!(offset_of!(libc::termios, c_line) == 16);
    assert!(offset_of!(libc::termios, c_cc) == 17);
    assert!(offset_of!(libc::termios, __c_ispeed) == 52);
    assert!(offset_of!(libc::termios, __c_ospeed) == 56);
    assert!(size_of::<libc::termios>() == 60);
    assert!(offset_of!(libc::timespec, tv_sec) == 0);
    assert!(offset_of!(libc::timespec, tv_nsec) == 8);
    assert!(size_of::<libc::timespec>() == 16);

    assert!(offset_of!(libc::stat, st_dev) == 0);
    assert!(offset_of!(libc::stat, st_ino) == 8);
    assert!(offset_of!(libc::stat, st_nlink) == 16);
    assert!(offset_of!(libc::stat, st_mode) == 24);
    assert!(offset_of!(libc::stat, st_uid) == 28);
    assert!(offset_of!(libc::stat, st_gid) == 32);
    assert!(offset_of!(libc::stat, st_rdev) == 40);
    assert!(offset_of!(libc::stat, st_size) == 48);
    assert!(offset_of!(libc::stat, st_blksize) == 56);
    assert!(offset_of!(libc::stat, st_blocks) == 64);
    assert!(offset_of!(libc::stat, st_atime) == 72);
    assert!(offset_of!(libc::stat, st_atime_nsec) == 80);
    assert!(offset_of!(libc::stat, st_mtime) == 88);
    assert!(offset_of!(libc::stat, st_mtime_nsec) == 96);
    assert!(offset_of!(libc::stat, st_ctime) == 104);
    assert!(offset_of!(libc::stat, st_ctime_nsec) == 112);
    assert!(size_of::<libc::stat>() == 144);

    assert!(offset_of!(libc::utsname, sysname) == 0);
    assert!(offset_of!(libc::utsname, nodename) == 150);
    assert!(offset_of!(libc::utsname, release) == 300);
    assert!(offset_of!(libc::utsname, version) == 450);
    assert!(offset_of!(libc::utsname, machine) == 600);
    assert!(offset_of!(libc::utsname, domainname) == 750);
    assert!(size_of::<libc::utsname>() == 900);

    // musl's statfs as it is; the fork keeps f_owner and f_fstypename
    // inside f_spare.
    assert!(offset_of!(libc::statfs, f_type) == 0);
    assert!(offset_of!(libc::statfs, f_bsize) == 8);
    assert!(offset_of!(libc::statfs, f_blocks) == 16);
    assert!(offset_of!(libc::statfs, f_bfree) == 24);
    assert!(offset_of!(libc::statfs, f_bavail) == 32);
    assert!(offset_of!(libc::statfs, f_files) == 40);
    assert!(offset_of!(libc::statfs, f_ffree) == 48);
    assert!(offset_of!(libc::statfs, f_fsid) == 56);
    assert!(offset_of!(libc::statfs, f_namelen) == 64);
    assert!(offset_of!(libc::statfs, f_frsize) == 72);
    assert!(offset_of!(libc::statfs, f_flags) == 80);
    assert!(offset_of!(libc::statfs, f_spare) == 88);
    assert!(size_of::<libc::statfs>() == 120);
    assert!(size_of::<libc::statfs64>() == 120);
    // statvfs is the other direction: cosmo's is 88 bytes against musl's
    // 112, so it short-writes, which is safe. Pinned so a cosmocc upgrade
    // that grows it past musl's has to come through here.
    assert!(size_of::<libc::statvfs>() == 112);

    assert!(size_of::<libc::pthread_attr_t>() == 56);

    // widened from musl's 40 to cosmo's 64 in patches/libc.patch;
    // pthread_mutex.rs is the live check
    assert!(size_of::<libc::pthread_mutex_t>() == 64);
    assert!(align_of::<libc::pthread_mutex_t>() == 8);

    // sem_t is widened from musl's 32 to cosmo's 256: sem_init() writes the
    // whole cosmo union into the caller's buffer.
    assert!(size_of::<libc::sem_t>() == 256);
    assert!(align_of::<libc::sem_t>() == 8);

    assert!(offset_of!(libc::dirent, d_ino) == 0);
    assert!(offset_of!(libc::dirent, d_off) == 8);
    assert!(offset_of!(libc::dirent, d_reclen) == 16);
    assert!(offset_of!(libc::dirent, d_type) == 18);
    assert!(offset_of!(libc::dirent, d_name) == 19);
    assert!(size_of::<libc::dirent>() == 280);

    assert!(offset_of!(libc::flock, l_type) == 0);
    assert!(offset_of!(libc::flock, l_whence) == 2);
    assert!(offset_of!(libc::flock, l_start) == 8);
    assert!(offset_of!(libc::flock, l_len) == 16);
    assert!(offset_of!(libc::flock, l_pid) == 24);
    assert!(size_of::<libc::flock>() == 32);

    assert!(offset_of!(libc::ucontext_t, uc_flags) == 0);
    assert!(offset_of!(libc::ucontext_t, uc_link) == 8);
    assert!(offset_of!(libc::ucontext_t, uc_stack) == 16);
    #[cfg(target_arch = "x86_64")]
    {
        assert!(offset_of!(libc::ucontext_t, uc_mcontext) == 40);
        assert!(offset_of!(libc::ucontext_t, uc_sigmask) == 296);
        assert!(size_of::<libc::mcontext_t>() == 256);
        assert!(offset_of!(libc::mcontext_t, gregs) == 0);
        // gregs are u64 slots in kernel sigcontext order; REG_RIP/REG_RSP
        // index the slots cosmo's named rip/rsp fields sit in.
        assert!(libc::REG_RIP == 16 && libc::REG_RSP == 15);
    }
    #[cfg(target_arch = "aarch64")]
    {
        assert!(offset_of!(libc::ucontext_t, uc_sigmask) == 40);
        assert!(offset_of!(libc::ucontext_t, uc_mcontext) == 176);
        assert!(offset_of!(libc::mcontext_t, fault_address) == 0);
        assert!(offset_of!(libc::mcontext_t, sp) == 256);
        assert!(offset_of!(libc::mcontext_t, pc) == 264);
        assert!(offset_of!(libc::mcontext_t, pstate) == 272);
    }
};

fn main() {
    // A live spot-check on top of the static proof: stat a real file and see
    // sane values come back through the fields the offsets above pin down.
    let meta = std::fs::metadata(std::env::args().next().expect("argv[0]")).expect("stat self");
    assert!(meta.len() > 0);
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let path = std::ffi::CString::new(std::env::args().next().unwrap()).unwrap();
    assert_eq!(unsafe { libc::stat(path.as_ptr(), &mut st) }, 0);
    assert_eq!(
        st.st_size as u64,
        meta.len(),
        "st_size offset is live-wrong"
    );
    assert!(
        st.st_mode & libc::S_IFMT == libc::S_IFREG,
        "st_mode offset is live-wrong"
    );
    assert!(st.st_nlink >= 1);
    println!(
        "live stat: size={} mode={:o} nlink={}",
        st.st_size, st.st_mode, st.st_nlink
    );

    // statfs and pthread_getattr_np write into the caller's struct; a
    // canary right behind it catches a write past musl's size.
    #[repr(C)]
    struct Guarded<T> {
        inner: T,
        canary: [u8; 64],
    }
    let mut fs: Guarded<libc::statfs> = unsafe { std::mem::zeroed() };
    fs.canary = [0xa5; 64];
    assert_eq!(unsafe { libc::statfs(c".".as_ptr(), &mut fs.inner) }, 0);
    assert!(fs.canary == [0xa5; 64], "statfs wrote past musl's struct");
    assert!(fs.inner.f_bsize > 0 && fs.inner.f_blocks > 0);
    println!(
        "live statfs: bsize={} blocks={} namelen={}",
        fs.inner.f_bsize, fs.inner.f_blocks, fs.inner.f_namelen
    );

    let mut attr: Guarded<libc::pthread_attr_t> = unsafe { std::mem::zeroed() };
    attr.canary = [0xa5; 64];
    let mut stack = std::ptr::null_mut();
    let mut size = 0;
    unsafe {
        assert_eq!(
            libc::pthread_getattr_np(libc::pthread_self(), &mut attr.inner),
            0
        );
        assert_eq!(
            libc::pthread_attr_getstack(&attr.inner, &mut stack, &mut size),
            0
        );
        libc::pthread_attr_destroy(&mut attr.inner);
    }
    assert!(
        attr.canary == [0xa5; 64],
        "pthread_getattr_np wrote past musl's struct"
    );
    assert!(size > 0);
    println!("live pthread_getattr_np: stack size={size}");

    println!("\nlayout check ok");
}
