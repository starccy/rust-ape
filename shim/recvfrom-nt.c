// [rust-ape] sys_recvfrom_nt, replacing cosmo 4.0.2's libc.a(recvfrom-nt.o) for one addition
// marked [rust-ape] below: a non-blocking receive asks Winsock whether data
// is queued before it starts anything.
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
// Derived from cosmopolitan libc/sock/recvfrom-nt.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // cosmo's own build has C23 bool
#include "libc/calls/internal.h"
#include "libc/calls/struct/iovec.h"
#include "libc/calls/struct/sigset.internal.h"
#include "libc/intrin/fds.h"
#include "libc/nt/struct/iovec.h"
#include "libc/nt/winsock.h"
#include "libc/sock/internal.h"
#include "libc/sock/struct/sockaddr.h"
#include "libc/sock/syscall_fd.internal.h"
#include "libc/sysv/consts/msg.h"
#include "libc/sysv/consts/o.h"
#include "libc/sysv/errfuns.h"
#ifdef __x86_64__

#define _MSG_OOB      1
#define _MSG_PEEK     2
#define _MSG_DONTWAIT 64

bool __ape_shim_sock_recv_ready(int64_t, uint32_t); // recv-nt.c

struct RecvFromArgs {
  const struct iovec *iov;
  size_t iovlen;
  void *opt_out_srcaddr;
  uint32_t *opt_inout_srcaddrsize;
  struct NtIovec iovnt[16];
};

textwindows static int sys_recvfrom_nt_start(int64_t handle,
                                             struct NtOverlapped *overlap,
                                             uint32_t *flags, void *arg) {
  struct RecvFromArgs *args = arg;
  return WSARecvFrom(
      handle, args->iovnt, __iovec2nt(args->iovnt, args->iov, args->iovlen), 0,
      flags, args->opt_out_srcaddr, args->opt_inout_srcaddrsize, overlap, 0);
}

textwindows ssize_t sys_recvfrom_nt(int fd, const struct iovec *iov,
                                    size_t iovlen, uint32_t flags,
                                    void *opt_out_srcaddr,
                                    uint32_t *opt_inout_srcaddrsize) {
  if (flags & ~(_MSG_DONTWAIT | _MSG_OOB | _MSG_PEEK))
    return einval();
  ssize_t rc;
  struct Fd *f = g_fds.p + fd;
  sigset_t waitmask = __sig_block();
  // [rust-ape] see the header comment
  bool nonblock = (f->flags & O_NONBLOCK) || (flags & _MSG_DONTWAIT);
  if (nonblock && !__ape_shim_sock_recv_ready(f->handle, flags)) {
    __sig_unblock(waitmask);
    return eagain();
  }
  rc = __winsock_block(f->handle, flags & ~_MSG_DONTWAIT, nonblock,
                       f->rcvtimeo, waitmask, sys_recvfrom_nt_start,
                       &(struct RecvFromArgs){iov, iovlen, opt_out_srcaddr,
                                              opt_inout_srcaddrsize});
  __sig_unblock(waitmask);
  return rc;
}

#endif /* __x86_64__ */
