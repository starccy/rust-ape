// [rust-ape] sys_close_nt, replacing cosmo 4.0.2's libc.a(close-nt.o) minus
// the implicit FlushFileBuffers on every close of a writable disk file.
//
// Derived from cosmopolitan libc/calls/close-nt.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.

// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/calls/internal.h"
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/intrin/fds.h"
#include "libc/intrin/weaken.h"
#include "libc/nt/runtime.h"
#include "libc/runtime/zipos.internal.h"
#include "libc/sock/syscall_fd.internal.h"
#include "libc/sysv/errfuns.h"

// not declared by any SDK header; weak in upstream too, so a program that
// never takes an fcntl lock does not link the lock machinery
int sys_fcntl_nt_lock_cleanup(int);

// [rust-ape] shim/procfs/core/ forgets a /proc directory descriptor here
void __ape_shim_procfs_fd_closed(int);

textwindows int sys_close_nt(int fd, int fd_for_locks) {
    if ((unsigned)fd >= g_fds.n)
        return ebadf();
    struct Fd *f = g_fds.p + fd;
    switch (f->kind) {
        case kFdEmpty:
            return ebadf();
        case kFdFile:
            if (_weaken(sys_fcntl_nt_lock_cleanup))
                _weaken(sys_fcntl_nt_lock_cleanup)(fd_for_locks);
            __ape_shim_procfs_fd_closed(fd); // [rust-ape]
            // [rust-ape] upstream: if ((f->flags & O_ACCMODE) != O_RDONLY &&
            // GetFileType(f->handle) == kNtFileTypeDisk)
            //     FlushFileBuffers(f->handle);
            break;
        case kFdSocket:
            if (_weaken(sys_closesocket_nt))
                return _weaken(sys_closesocket_nt)(f);
            break;
        case kFdZip:
            return __zipos_close(fd);
        default:
            break;
    }
    if (f->cursor)
        __cursor_unref(f->cursor);
    if (!CloseHandle(f->handle))
        return __winerr();
    return 0;
}
