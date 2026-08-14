// The Linux-only half of the pipe-plumbing family
//
// These are Linux-only as an API; no other host has a counterpart to
// translate to. So Linux gets the real syscall and every other host answers
// ENOSYS, which is what cosmo's own splice() does off Linux and what callers
// already handle: these calls are a fast path, tried once and dropped in
// favor of a read/write loop the moment they fail.
//
// No __ape_shim_ prefix here, unlike most of this directory. There is no
// cosmo definition to displace and no constant to translate -- the flags
// (SPLICE_F_*) are the kernel's own, passed through as given -- so these are
// plain definitions filling a hole in libc, and the declarations on both
// sides (cosmo's header, the libc crate's extern block) bind to them
// directly. vmsplice keeps cosmo's declared signature, int64_t/uint32_t
// rather than the size_t/unsigned the man page spells, so the header it
// already has agrees with it.
//
// The raw-syscall mechanics and the errno reasoning behind them live in
// syscall.h.

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>
#include <sys/uio.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

ssize_t tee(int fd_in, int fd_out, size_t len, unsigned int flags) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return __ape_syscall_ret(
        __ape_raw_syscall(__NR_tee, fd_in, fd_out, (long)len, flags, 0));
}

ssize_t vmsplice(int fd, const struct iovec *iov, int64_t nr_segs,
                 uint32_t flags) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return __ape_syscall_ret(
        __ape_raw_syscall(__NR_vmsplice, fd, (long)iov, (long)nr_segs, flags, 0));
}
