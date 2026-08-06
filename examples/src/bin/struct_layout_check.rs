//! Struct-layout audit, Rust side. Hand-written (unlike shim_tables_check.rs,
//! which gen-shim regenerates).
//!
//! shim/layouts.c pins cosmo's struct offsets to literal numbers; this file
//! pins the libc crate's musl side to the same numbers, for both target
//! arches. If both compile, stat/dirent/timespec/flock/ucontext can keep
//! passing through the shim without repacking. It also guards the
//! hand-aligned aarch64 stat in patches/libc.patch; if that patch is wrong,
//! the assert here names the field that moved.
//!
//! struct termios is absent on purpose. Its layouts genuinely differ, so
//! shim/termios.c repacks it and carries its own asserts.

use std::mem::{offset_of, size_of};

const _: () = {
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
    assert_eq!(st.st_size as u64, meta.len(), "st_size offset is live-wrong");
    assert!(st.st_mode & libc::S_IFMT == libc::S_IFREG, "st_mode offset is live-wrong");
    assert!(st.st_nlink >= 1);
    println!(
        "layout check ok (compile-time); live stat: size={} mode={:o} nlink={}",
        st.st_size, st.st_mode, st.st_nlink
    );
}
