// read/readv for the Linux-personality shim, replacing cosmo 4.0.2's
// libc.a(read.o) and libc.a(readv.o).
//
// Not a bug fix: the forwarding below is upstream's, condensed into the
// shared readv_impl. The addition is the re-arm hook at each exit, which
// tells shim/epoll.c's edge-triggered emulation to watch this fd's read
// side again. It fires on EAGAIN and on any read that returned data: under
// kernel ET a later arrival is reported as a new edge, so a consumer that
// stops at a short read instead of reading to EAGAIN still gets events,
// and the same must hold here. EOF and hard errors don't re-arm, keeping
// HUP/ERR single-shot.
// The libc crate's read/readv are not link_name-redirected like the other
// entry points, so the interception happens at the symbol itself: each of
// the two archive members defines exactly its one function, and defining
// both names here keeps either member from being pulled in.
//
// Derived from cosmopolitan libc/calls/read.c and libc/calls/readv.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.
// Revisit on toolchain upgrade.

// The cosmo-internal headers below only work when _COSMO_SOURCE was defined
// before the compiler's -include of normalize.inc, so the linker wrapper
// compiles this file with -D_COSMO_SOURCE (an in-file define would come too
// late).
#include <stdbool.h>
#include <stdint.h>
#include <libc/calls/calls.h>
#include <libc/calls/cp.internal.h>
#include <libc/calls/internal.h>
#include <libc/calls/struct/iovec.h>
#include <libc/calls/struct/iovec.internal.h>
#include <libc/calls/syscall-sysv.internal.h>
#include <libc/errno.h>
#include <libc/intrin/strace.h>
#include <libc/intrin/weaken.h>
#include <libc/macros.h>
#include <libc/mem/alloca.h>
#include <libc/runtime/stack.h>
#include <libc/runtime/zipos.internal.h>
#include <libc/sock/internal.h>
#include <libc/stdckdint.h>
#include <libc/sysv/errfuns.h>

void __ape_shim_epoll_rearm_in(int fd); // shim/epoll.c

static size_t SumIovecBytes(const struct iovec *iov, int iovlen) {
  size_t count = 0;
  for (int i = 0; i < iovlen; ++i)
    if (ckd_add(&count, count, iov[i].iov_len))
      count = SIZE_MAX;
  return count;
}

static ssize_t readv_impl(int fd, const struct iovec *iov, int iovlen) {
  if (fd < 0)
    return ebadf();
  if (iovlen < 0)
    return einval();

  // XNU and BSDs will EINVAL if requested bytes exceeds INT_MAX
  // this is inconsistent with Linux which ignores huge requests
  if (!IsLinux()) {
    size_t sum, remain = 0x7ffff000;
    if ((sum = SumIovecBytes(iov, iovlen)) > remain) {
      struct iovec *iov2;
#pragma GCC push_options
#pragma GCC diagnostic ignored "-Walloca-larger-than="
#pragma GCC diagnostic ignored "-Wanalyzer-out-of-bounds"
      iov2 = alloca(iovlen * sizeof(struct iovec));
      CheckLargeStackAllocation(iov2, iovlen * sizeof(struct iovec));
#pragma GCC pop_options
      for (int i = 0; i < iovlen; ++i) {
        iov2[i] = iov[i];
        if (remain >= iov2[i].iov_len) {
          remain -= iov2[i].iov_len;
        } else {
          iov2[i].iov_len = remain;
          remain = 0;
        }
      }
      iov = iov2;
    }
  }

  if (fd < g_fds.n && g_fds.p[fd].kind == kFdZip) {
    return _weaken(__zipos_read)(
        (struct ZiposHandle *)(intptr_t)g_fds.p[fd].handle, iov, iovlen, -1);
  } else if (IsLinux() || IsXnu() || IsFreebsd() || IsOpenbsd() || IsNetbsd()) {
    if (iovlen == 1) {
      return sys_read(fd, iov[0].iov_base, iov[0].iov_len);
    } else {
      return sys_readv(fd, iov, iovlen);
    }
  } else if (fd >= g_fds.n) {
    return ebadf();
  } else if (IsMetal()) {
    return sys_readv_metal(fd, iov, iovlen);
  } else if (IsWindows()) {
    return sys_readv_nt(fd, iov, iovlen);
  } else {
    return enosys();
  }
}

ssize_t read(int fd, void *buf, size_t size) {
  ssize_t rc;
  BEGIN_CANCELATION_POINT;
  size = MIN(size, 0x7ffff000);
  if (!buf && size) {
    rc = efault();
  } else {
    rc = readv_impl(fd, &(struct iovec){buf, size}, 1);
  }
  END_CANCELATION_POINT;
  if (rc > 0 || (rc == -1 && errno == EAGAIN))
    __ape_shim_epoll_rearm_in(fd);
  DATATRACE("read(%d, [%#.*hhs%s], %'zu) → %'zd% m", fd,
            (int)MAX(0, MIN(40, rc)), buf, rc > 40 ? "..." : "", size, rc);
  return rc;
}

ssize_t readv(int fd, const struct iovec *iov, int iovcnt) {
  ssize_t rc;
  BEGIN_CANCELATION_POINT;
  rc = readv_impl(fd, iov, iovcnt);
  END_CANCELATION_POINT;
  if (rc > 0 || (rc == -1 && errno == EAGAIN))
    __ape_shim_epoll_rearm_in(fd);
  STRACE("readv(%d, %p, %d) → %'zd% m", fd, iov, iovcnt, rc);
  return rc;
}
