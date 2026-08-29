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
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include <stdint.h>
#include <sys/stat.h>
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
#include <libc/sock/struct/pollfd.h>
#include <libc/stdckdint.h>
#include <libc/sysv/errfuns.h>

void __ape_shim_epoll_rearm_in(int fd); // shim/epoll.c
int __ape_shim_poll(struct pollfd *, unsigned long, int); // shim/poll.c
void __ape_shim_console_before_wait(int fd);                // shim/console.c
long __ape_shim_procfs_memfd_read(int, const struct iovec *, int); // shim/procfs/core/

// ---------------------------------------------------------------------------
// Escape sequences from the NT console arrive one byte per read, so a
// parser fed a lone ESC takes it for the Escape key and stalls on a reply
// that never assembles. Linux delivers the whole sequence in one read, and
// the fix is to do the same, waiting briefly after a console read that ends
// inside an escape sequence and appending the rest to it.

#define ESC_COALESCE_MS 10   // per step; bytes really come ~1ms apart
#define ESC_COALESCE_MAX 64  // longest sequence worth waiting for

// True when the buffer ends inside an escape sequence: a lone ESC, a CSI
// without its final byte, an SS3 without its one following byte, or an OSC
// not yet closed by BEL or ST.
static bool EndsInsideEscape(const unsigned char *b, size_t n) {
  size_t i = n;
  while (i && b[i - 1] != 0x1b)
    i--;
  if (!i)
    return false;  // no ESC at all
  size_t after = n - i;
  if (!after)
    return true;  // lone ESC
  unsigned char c = b[i];
  if (c == '[') {
    for (size_t k = i + 1; k < n; k++)
      if (b[k] >= 0x40 && b[k] <= 0x7e)
        return false;  // final byte seen
    return true;
  }
  if (c == 'O')
    return after < 2;
  if (c == ']') {
    for (size_t k = i + 1; k < n; k++)
      if (b[k] == 0x07)
        return false;
    return true;
  }
  return false;  // ESC + anything else (alt-key) is complete as is
}

static ssize_t CoalesceConsoleEscape(int fd, unsigned char *b, size_t cap,
                                     ssize_t n) {
  while (n > 0 && (size_t)n < cap && n < ESC_COALESCE_MAX &&
         EndsInsideEscape(b, (size_t)n)) {
    struct pollfd p = {fd, 1 /* POLLIN, Linux-coded */, 0};
    if (__ape_shim_poll(&p, 1, ESC_COALESCE_MS) <= 0 || !(p.revents & 1))
      break;
    ssize_t m = sys_readv_nt(fd, &(struct iovec){b + n, cap - (size_t)n}, 1);
    if (m <= 0)
      break;
    n += m;
  }
  return n;
}

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

  if (IsWindows()) {
    ssize_t n = __ape_shim_procfs_memfd_read(fd, iov, iovlen);
    if (n != -2) return n;
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
    if (g_fds.p[fd].kind == kFdConsole) __ape_shim_console_before_wait(fd);
    ssize_t n = sys_readv_nt(fd, iov, iovlen);
    if (n > 0 && iovlen == 1 && g_fds.p[fd].kind == kFdConsole)
      n = CoalesceConsoleEscape(fd, iov[0].iov_base, iov[0].iov_len, n);
    // [rust-ape] ReadFile on a directory handle fails ERROR_INVALID_FUNCTION,
    // which cosmo reports as EINVAL; Linux says EISDIR (cat /proc/x/cwd)
    if (n == -1 && errno == EINVAL && g_fds.p[fd].kind == kFdFile) {
      struct stat st;
      if (!fstat(fd, &st) && S_ISDIR(st.st_mode)) return eisdir();
      errno = EINVAL;
    }
    return n;
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
