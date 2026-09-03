// [rust-ape] sys_recv_nt, replacing cosmo 4.0.2's libc.a(recv-nt.o) for two
// additions marked [rust-ape] below: a non-blocking receive asks Winsock
// whether data is queued before it starts anything, and an end-of-file
// completion is reported as a zero-byte read rather than a stray errno.
//
// Cosmo runs every receive as an overlapped operation and, on a
// non-blocking socket, cancels it the moment it reports pending. A send
// arriving between the two can complete the receive, consuming the data,
// while the cancel still reports aborted; the caller sees EAGAIN, the
// data is gone, and the socket never polls readable again.
//
// So a non-blocking receive asks WSAPoll first. Nothing queued is EAGAIN
// with the socket untouched; anything else completes the receive at once,
// never reaching the cancel. A failed poll falls through to the upstream
// code unchanged.
//
// Everything below the [rust-ape] block is a faithful copy of upstream.
// Revisit on toolchain upgrade.
//
// Derived from cosmopolitan libc/sock/recv-nt.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // cosmo's own build has C23 bool
#include "libc/calls/internal.h"
#include "libc/calls/struct/sigset.internal.h"
#include "libc/errno.h"
#include "libc/nt/errors.h"
#include "libc/nt/struct/iovec.h"
#include "libc/nt/struct/overlapped.h"
#include "libc/nt/thunk/msabi.h"
#include "libc/nt/struct/pollfd.h"
#include "libc/nt/winsock.h"
#include "libc/sock/internal.h"
#include "libc/sock/syscall_fd.internal.h"
#include "libc/sysv/consts/fio.h"
#include "libc/sysv/consts/o.h"
#include "libc/sysv/errfuns.h"
#include "libc/vga/vga.internal.h"
#ifdef __x86_64__

#define _MSG_OOB      1
#define _MSG_PEEK     2
#define _MSG_WAITALL  8
#define _MSG_DONTWAIT 64

__msabi extern typeof(__sys_ioctlsocket_nt) *const __imp_ioctlsocket;


// [rust-ape] whether a receive would complete without waiting; data, a
// hangup or an error all qualify. A failed poll counts as ready, since
// that only runs the upstream path, while a false EAGAIN loses a wakeup.
bool __ape_shim_sock_recv_ready(int64_t handle, uint32_t flags) {
  struct sys_pollfd_nt p = {handle, (flags & _MSG_OOB) ? 0x0200 : 0x0100, 0};
  int r = WSAPoll(&p, 1, 0);
  return r != 0;
}

struct RecvArgs {
  const struct iovec *iov;
  size_t iovlen;
  struct NtIovec iovnt[16];
};

textwindows static int sys_recv_nt_start(int64_t handle,
                                         struct NtOverlapped *overlap,
                                         uint32_t *flags, void *arg) {
  struct RecvArgs *args = arg;
  return WSARecv(handle, args->iovnt,
                 __iovec2nt(args->iovnt, args->iov, args->iovlen), 0, flags,
                 overlap, 0);
}

textwindows ssize_t sys_recv_nt(int fd, const struct iovec *iov, size_t iovlen,
                                uint32_t flags) {
  if (flags & ~(_MSG_DONTWAIT | _MSG_OOB | _MSG_PEEK | _MSG_WAITALL))
    return einval();
  ssize_t rc;
  struct Fd *f = g_fds.p + fd;
  sigset_t waitmask = __sig_block();

  // "Be aware that if the underlying transport provider does not
  //  support MSG_WAITALL, or if the socket is in a non-blocking mode,
  //  then this call will fail with WSAEOPNOTSUPP. Also, if MSG_WAITALL
  //  is specified along with MSG_OOB, MSG_PEEK, or MSG_PARTIAL, then
  //  this call will fail with WSAEOPNOTSUPP."
  //                             —Quoth MSDN § WSARecv
  if (flags & _MSG_WAITALL)
    __imp_ioctlsocket(f->handle, FIONBIO, (uint32_t[]){0});

  // [rust-ape] see the header comment
  bool nonblock = (f->flags & O_NONBLOCK) || (flags & _MSG_DONTWAIT);
  if (nonblock && !__ape_shim_sock_recv_ready(f->handle, flags)) {
    __sig_unblock(waitmask);
    return eagain();
  }

  rc = __winsock_block(f->handle, flags & ~_MSG_DONTWAIT, nonblock,
                       f->rcvtimeo, waitmask, sys_recv_nt_start,
                       &(struct RecvArgs){iov, iovlen});

  // [rust-ape] a receive can complete with ERROR_HANDLE_EOF, which cosmo's
  // errno table doesn't know, so the raw win32 code leaked out as errno and
  // happened to read as ENOSYS. It means the stream has nothing more to
  // give, which cosmo already reports as a zero-byte read on files and pipes.
  if (rc == -1 && errno == kNtErrorHandleEof) rc = 0;

  __sig_unblock(waitmask);

  return rc;
}

#endif /* __x86_64__ */
