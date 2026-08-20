// Struct-layout audit, C side. No code, only compile-time asserts.
//
// The shim translates VALUES at the libc boundary; STRUCTS it mostly passes
// through, relying on the two layouts being identical. That is true today
// but could silently stop being true after a cosmocc or libc upgrade, so
// the load-bearing structs are pinned here field by field. This file asserts
// cosmo's offsets against literal numbers, and struct_layout_check.rs (built
// for both Rust targets) asserts the musl/libc-crate side against the same
// numbers. If both compile, the two worlds agree; if one drifts, the build
// breaks with the field's name in the error.
//
// What's covered and why:
//   - struct stat: cosmo's "cosmo abi" happens to equal musl x86_64 exactly
//     (cosmo's st_flags/st_birthtim/st_gen live inside musl's padding). The
//     libc crate's aarch64 stat is hand-realigned to the same layout by
//     patches/libc.patch — the Rust-side asserts are what keep that patch
//     honest.
//   - struct dirent: cosmo declares the Linux getdents64 ABI, which is
//     musl's dirent on every arch.
//   - struct utsname: cosmo's is 150 bytes per field against musl's 65. The
//     libc crate's declaration is widened to cosmo's in patches/libc.patch,
//     the same treatment aarch64's stat gets; the asserts on both sides are
//     what keep that honest.
//   - struct statfs: identical for the whole Linux ABI, then cosmo adds a
//     BSD-flavored tail (f_owner, f_fstypename) that takes it from musl's
//     120 bytes to 144. Widened in patches/libc.patch like utsname, since
//     a caller who allocates musl's 120 gets 24 bytes of its own frame
//     overwritten. struct statvfs goes the other way and is fine.
//   - struct timespec: the {i64, i64} everyone already relies on.
//   - struct flock: identical up to musl's tail padding (cosmo's l_sysid
//     lives there); only l_type VALUES differ, translated in open.c.
//   - ucontext_t/mcontext_t: cosmo lays both out in the Linux kernel ABI
//     shape (on aarch64 it even pads uc_sigmask+__unused to 136 bytes so
//     uc_mcontext lands at musl's offset 176). Signal handlers receive
//     cosmo's pointer straight through the trampoline, so a crate reading
//     uc_mcontext.gregs[REG_RIP] sees the right slot. The tails differ
//     past uc_sigmask (cosmo: 8-byte set + fpu state; musl: 128-byte set +
//     __fpregs_mem) but nothing addresses those through the struct.
//
// struct termios is covered differently. It genuinely differs (musl has
// c_line, NCCS 32, speeds at 52/56; cosmo has no c_line, NCCS 20, size 44,
// plus runtime V*/ICANON-class constants), so it is repacked like
// sigaction. shim/termios.c rebuilds it field by field and pins the
// Linux-side shapes with its own asserts; the asserts further down pin the
// cosmo-side shape that repack writes into.

#include <dirent.h>
#include <fcntl.h>
#include <stddef.h>
#include <sys/stat.h>
#include <time.h>
#include <ucontext.h>

#define CHECK(strct, field, off) \
    _Static_assert(offsetof(struct strct, field) == (off), \
                   #strct "." #field " moved; fix the shim's layout contract")

CHECK(timespec, tv_sec, 0);
CHECK(timespec, tv_nsec, 8);
_Static_assert(sizeof(struct timespec) == 16, "timespec size");

CHECK(stat, st_dev, 0);
CHECK(stat, st_ino, 8);
CHECK(stat, st_nlink, 16);
CHECK(stat, st_mode, 24);
CHECK(stat, st_uid, 28);
CHECK(stat, st_gid, 32);
CHECK(stat, st_rdev, 40);
CHECK(stat, st_size, 48);
CHECK(stat, st_blksize, 56);
CHECK(stat, st_blocks, 64);
CHECK(stat, st_atim, 72);
CHECK(stat, st_mtim, 88);
CHECK(stat, st_ctim, 104);
_Static_assert(sizeof(struct stat) >= 120, "stat tail");
// musl's stat is 144 bytes (three trailing spare longs); cosmo puts
// st_birthtim + st_gen there. Rust must never see a stat SMALLER than what
// cosmo writes, or stack copies get clobbered past the end.
_Static_assert(sizeof(struct stat) <= 144, "cosmo stat outgrew musl's 144 bytes");

CHECK(dirent, d_ino, 0);
CHECK(dirent, d_off, 8);
CHECK(dirent, d_reclen, 16);
CHECK(dirent, d_type, 18);
CHECK(dirent, d_name, 19);
_Static_assert(sizeof(struct dirent) == 280, "dirent size");

CHECK(flock, l_type, 0);
CHECK(flock, l_whence, 2);
CHECK(flock, l_start, 8);
CHECK(flock, l_len, 16);
CHECK(flock, l_pid, 24);
// musl's flock is 32 bytes (4 of tail padding); cosmo's l_sysid at 28 fits
// inside. Anything bigger would let cosmo write past a musl-sized struct.
_Static_assert(sizeof(struct flock) <= 32, "cosmo flock outgrew musl's 32 bytes");

// cosmo's utsname is SYS_NMLN=150 per field where musl has 65, so the libc
// crate's declaration is widened to match in patches/libc.patch rather than
// repacked here. These numbers are what that patch has to keep hitting: get
// them wrong and uname() writes past the caller's struct.
#include <sys/utsname.h>
CHECK(utsname, sysname, 0);
CHECK(utsname, nodename, 150);
CHECK(utsname, release, 300);
CHECK(utsname, version, 450);
CHECK(utsname, machine, 600);
CHECK(utsname, domainname, 750);
_Static_assert(sizeof(struct utsname) == 900, "cosmo utsname size");

// cosmo's statfs agrees with musl for the whole Linux ABI, then adds
// f_owner and f_fstypename[16] past f_spare, which takes it from 120 bytes
// to 144. The libc crate's declaration is widened to 144 in
// patches/libc.patch, same treatment as utsname; these numbers are what
// that patch has to keep hitting, because statfs()/fstatfs() write the full
// 144 into whatever the caller handed over.
#include <sys/statfs.h>
#include <sys/statvfs.h>
CHECK(statfs, f_type, 0);
CHECK(statfs, f_bsize, 8);
CHECK(statfs, f_blocks, 16);
CHECK(statfs, f_bfree, 24);
CHECK(statfs, f_bavail, 32);
CHECK(statfs, f_files, 40);
CHECK(statfs, f_ffree, 48);
CHECK(statfs, f_fsid, 56);
CHECK(statfs, f_namelen, 64);
CHECK(statfs, f_frsize, 72);
CHECK(statfs, f_flags, 80);
CHECK(statfs, f_spare, 88);
_Static_assert(sizeof(struct statfs) == 144, "cosmo statfs size");
// The other direction: cosmo's statvfs is smaller than musl's 112, so it
// short-writes and needs no widening. Pinned so that stops being quiet if
// a cosmocc upgrade grows it.
_Static_assert(sizeof(struct statvfs) <= 112,
               "cosmo statvfs outgrew musl's 112 bytes");

// cosmo's pthread_attr_t is 64 bytes against musl's 56
#include <pthread.h>
_Static_assert(sizeof(pthread_attr_t) == 64, "cosmo pthread_attr_t size");

// cosmo's termios, the repack target of shim/termios.c: no c_line, NCCS 20,
// speeds as fields. If any of this drifts, the repack corrupts.
#include <termios.h>
CHECK(termios, c_iflag, 0);
CHECK(termios, c_oflag, 4);
CHECK(termios, c_cflag, 8);
CHECK(termios, c_lflag, 12);
CHECK(termios, c_cc, 16);
CHECK(termios, _c_ispeed, 36);
CHECK(termios, _c_ospeed, 40);
_Static_assert(sizeof(struct termios) == 44, "cosmo termios size");

CHECK(ucontext, uc_flags, 0);
CHECK(ucontext, uc_link, 8);
CHECK(ucontext, uc_stack, 16);
#ifdef __x86_64__
CHECK(ucontext, uc_mcontext, 40);
CHECK(ucontext, uc_sigmask, 296);
_Static_assert(sizeof(mcontext_t) == 256, "mcontext size");
CHECK(sigcontext, rip, 168 - 40); // kernel sigcontext order, gregs[16]
CHECK(sigcontext, rsp, 160 - 40); // gregs[15]
CHECK(sigcontext, fpregs, 184);
#elif defined(__aarch64__)
CHECK(ucontext, uc_sigmask, 40);
CHECK(ucontext, uc_mcontext, 176); // musl: 40 + 128B sigset, align 16
CHECK(sigcontext, fault_address, 0);
CHECK(sigcontext, sp, 256);
CHECK(sigcontext, pc, 264);
CHECK(sigcontext, pstate, 272);
#endif
