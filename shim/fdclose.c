// What the shim forgets when a descriptor closes. The fork's close.c
// hands every close here first.
//
// On NT the /proc emulation (shim/procfs/) tracks directory descriptors
// into its materialized tree and keeps the text behind a /proc content
// descriptor in memory, in a reserved slot with no handle; the socket
// layer (shim/socket.c) parks options for a still-connecting socket. On
// Apple Silicon the /proc descriptors are memory-backed too, claimed by a
// real kernel descriptor that the ordinary path still closes.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include "libc/calls/internal.h"
#include "libc/dce.h"
#include "libc/intrin/fds.h"
#include "libc/sysv/pib.h"

// shim/procfs/core/
void __ape_shim_procfs_fd_closed(int);
int __ape_shim_procfs_memfd_close(int);
// shim/socket.c
void __ape_shim_nt_sockopt_forget(int);

int __ape_shim_close_hook(int fd, int *rc) {
  if (IsWindows()) {
    if ((unsigned)fd >= __get_pib()->fds.n)
      return 0;
    switch (__get_pib()->fds.p[fd].kind) {
      case kFdFile:
        __ape_shim_procfs_fd_closed(fd);
        break;
      case kFdSocket:
        __ape_shim_nt_sockopt_forget(fd);
        break;
      case kFdReserved:
        if (__ape_shim_procfs_memfd_close(fd) == 0) {
          __releasefd(fd);
          *rc = 0;
          return 1;
        }
        break;
      default:
        break;
    }
  } else if (IsXnuSilicon()) {
    if (__ape_shim_procfs_memfd_close(fd) == 0)
      __get_pib()->fds.p[fd].kind = kFdEmpty;
    __ape_shim_procfs_fd_closed(fd);
  }
  return 0;
}
