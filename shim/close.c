// [rust-ape] close, replacing cosmo 4.0.2's libc.a(close.o) for one addition
// marked [rust-ape] below: on Apple Silicon the text behind an emulated
// /proc descriptor is freed here and its tracked directory entry dropped
// (NT does both inside shim/close-nt.c). The number itself is claimed by a
// real kernel descriptor, so the ordinary path still closes that.
//
// Everything else is a faithful copy of upstream. Revisit on toolchain
// upgrade.
//
// Derived from cosmopolitan libc/calls/close.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/assert.h"
#include "libc/calls/calls.h"
#include "libc/calls/internal.h"
#include "libc/calls/state.internal.h"
#include "libc/calls/struct/sigset.internal.h"
#include "libc/calls/syscall-nt.internal.h"
#include "libc/calls/syscall-sysv.internal.h"
#include "libc/dce.h"
#include "libc/errno.h"
#include "libc/intrin/fds.h"
#include "libc/intrin/kprintf.h"
#include "libc/intrin/strace.h"
#include "libc/intrin/weaken.h"
#include "libc/runtime/zipos.internal.h"
#include "libc/sock/syscall_fd.internal.h"
#include "libc/sysv/errfuns.h"

// [rust-ape] shim/procfs/core/
int __ape_shim_procfs_memfd_close(int);
void __ape_shim_procfs_fd_closed(int);

// for performance reasons we want to avoid holding __fds_lock()
// while sys_close() is happening. this leaves the kernel / libc
// having a temporarily inconsistent state. routines that obtain
// file descriptors the way __zipos_open() does need to retry if
// there's indication this race condition happened.

static int close_impl(int fd) {

  if (fd < 0) {
    return ebadf();
  }

  // give kprintf() the opportunity to dup() stderr
  if (fd == 2 && _weaken(kloghandle)) {
    _weaken(kloghandle)();
  }

  if (__isfdkind(fd, kFdZip)) {
    unassert(_weaken(__zipos_close));
    return _weaken(__zipos_close)(fd);
  }

  if (!IsWindows() && !IsMetal()) {
    return sys_close(fd);
  }

  if (IsWindows()) {
    return sys_close_nt(fd, fd);
  }

  return 0;
}

int close(int fd) {
  int rc;
  // [rust-ape] the shim's memory-backed /proc descriptors, Apple Silicon
  // only; both calls decline anything not theirs
  if (fd >= 0 && IsXnuSilicon()) {
    __ape_shim_procfs_memfd_close(fd);
    __ape_shim_procfs_fd_closed(fd);
  }
  if (__isfdkind(fd, kFdZip)) {  // XXX IsWindows()?
    BLOCK_SIGNALS;
    __fds_lock();
    rc = close_impl(fd);
    if (!__vforked)
      __releasefd(fd);
    __fds_unlock();
    ALLOW_SIGNALS;
  } else {
    rc = close_impl(fd);
    if (!__vforked)
      __releasefd(fd);
  }
  STRACE("close(%d) → %d% m", fd, rc);
  return rc;
}
