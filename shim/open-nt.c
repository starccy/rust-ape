// [rust-ape] sys_open_nt, replacing cosmo 4.0.2's libc.a(open-nt.o): the
// path conversion and CreateFile happen before __fds_lock is taken, see the
// note above sys_open_nt.
//
// Derived from cosmopolitan libc/calls/open-nt.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.

// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/assert.h"
#include "libc/calls/createfileflags.internal.h"
#include "libc/calls/internal.h"
#include "libc/calls/state.internal.h"
#include "libc/calls/struct/sigset.internal.h"
#include "libc/calls/syscall-nt.internal.h"
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/errno.h"
#include "libc/intrin/fds.h"
#include "libc/macros.h"
#include "libc/nt/console.h"
#include "libc/nt/createfile.h"
#include "libc/nt/enum/accessmask.h"
#include "libc/nt/enum/creationdisposition.h"
#include "libc/nt/enum/fileflagandattributes.h"
#include "libc/nt/enum/filesharemode.h"
#include "libc/nt/enum/filetype.h"
#include "libc/nt/files.h"
#include "libc/nt/process.h"
#include "libc/nt/runtime.h"
#include "libc/nt/synchronization.h"
#include "libc/nt/thunk/msabi.h"
#include "libc/str/str.h"
#include "libc/sysv/consts/fileno.h"
#include "libc/sysv/consts/o.h"
#include "libc/sysv/errfuns.h"

__msabi extern typeof(GetFileAttributes) *const __imp_GetFileAttributesW;

static textwindows int64_t sys_open_nt_impl(int dirfd, const char *path,
                                            uint32_t flags, int32_t mode,
                                            uint32_t extra_attr) {

  // join(topath(dirfd), path) and translate from utf-8 to utf-16
  char16_t path16[PATH_MAX];
  if (__mkntpathat(dirfd, path, flags, path16) == -1) {
    return kNtInvalidHandleValue;
  }

  // implement no follow flag
  // you can't open symlinks; use readlink
  // this flag only applies to the final path component
  // if _O_NOFOLLOW_ANY is passed (-1 on NT) it'll be rejected later
  uint32_t fattr = GetFileAttributes(path16);
  if (flags & _O_NOFOLLOW) {
    if (fattr != -1u && (fattr & kNtFileAttributeReparsePoint)) {
      return eloop();
    }
    flags &= ~_O_NOFOLLOW;  // don't actually pass this to win32
  }

  // handle some obvious cases while we have the attributes
  // we should ideally resolve symlinks ourself before doing this
  if (fattr != -1u) {
    if (fattr & kNtFileAttributeDirectory) {
      if ((flags & O_ACCMODE) != O_RDONLY || (flags & _O_CREAT)) {
        // tried to open directory for writing. note that our
        // undocumented _O_TMPFILE support on windows requires that a
        // filename be passed, rather than a directory like linux.
        return eisdir();
      }
      // on posix, the o_directory flag is an advisory safeguard that
      // isn't required. on windows, it's mandatory for opening a dir
      flags |= _O_DIRECTORY;
    } else if (!(fattr & kNtFileAttributeReparsePoint)) {
      // we know for certain file isn't a directory
      if (flags & _O_DIRECTORY) {
        return enotdir();
      }
    }
  }

  // translate posix flags to win32 flags
  uint32_t perm, share, disp, attr;
  if (GetNtOpenFlags(flags, mode, &perm, &share, &disp, &attr) == -1) {
    return kNtInvalidHandleValue;
  }

  if (fattr != -1u) {
    // "We have been asked to create a read-only file. "If the file
    //  already exists, the semantics of the Unix open system call is to
    //  preserve the existing permissions. If we pass CREATE_ALWAYS and
    //  FILE_ATTRIBUTE_READONLY to CreateFile, and the file already
    //  exists, CreateFile will change the file permissions. Avoid that to
    //  preserve the Unix semantics." -Quoth GoLang syscall_windows.go
    attr &= ~kNtFileAttributeReadonly;
  }

  // kNtTruncateExisting always returns kNtErrorInvalidParameter :'(
  if (disp == kNtTruncateExisting) {
    if (fattr != -1u) {
      disp = kNtCreateAlways;  // file exists (wish it could be more atomic)
    } else {
      return __fix_enotdir(enotdir(), path16);
    }
  }

  // We optimistically request some write permissions in O_RDONLY mode.
  // But that might prevent opening some files. So reactively back off.
  int extra_perm = 0;
  if ((flags & O_ACCMODE) == O_RDONLY) {
    extra_perm = kNtFileWriteAttributes | kNtFileWriteEa;
  }

  // open the file, following symlinks
  int e = errno;
  int64_t hand = CreateFile(path16, perm | extra_perm, share, &kNtIsInheritable,
                            disp, attr | extra_attr, 0);
  if (hand == -1 && errno == EACCES && (flags & O_ACCMODE) == O_RDONLY) {
    errno = e;
    hand = CreateFile(path16, perm, share, &kNtIsInheritable, disp,
                      attr | extra_attr, 0);
  }

  return __fix_enotdir(hand, path16);
}

static textwindows int sys_open_nt_special(int fd, int flags, int mode,
                                           int kind, const char16_t *name) {
  g_fds.p[fd].kind = kind;
  g_fds.p[fd].mode = mode;
  g_fds.p[fd].flags = flags;
  g_fds.p[fd].handle = CreateFile(name, kNtGenericRead | kNtGenericWrite,
                                  kNtFileShareRead | kNtFileShareWrite,
                                  &kNtIsInheritable, kNtOpenExisting, 0, 0);
  return fd;
}

static textwindows int sys_open_nt_no_handle(int fd, int flags, int mode,
                                             int kind) {
  g_fds.p[fd].kind = kind;
  g_fds.p[fd].mode = mode;
  g_fds.p[fd].flags = flags;
  g_fds.p[fd].handle = -1;
  return fd;
}

static textwindows int sys_open_nt_dup(int fd, int flags, int mode, int oldfd) {
  int64_t handle;
  if (!__isfdopen(oldfd))
    return enoent();
  if (DuplicateHandle(GetCurrentProcess(), g_fds.p[oldfd].handle,
                      GetCurrentProcess(), &handle, 0, true,
                      kNtDuplicateSameAccess)) {
    g_fds.p[fd] = g_fds.p[oldfd];
    g_fds.p[fd].handle = handle;
    g_fds.p[fd].mode = mode;
    __cursor_ref(g_fds.p[fd].cursor);
    if (!sys_fcntl_nt_setfl(fd, flags)) {
      return fd;
    } else {
      CloseHandle(handle);
      return -1;
    }
  } else {
    return __winerr();
  }
}

static int Atoi(const char *str) {
  int c;
  unsigned x = 0;
  if (!*str)
    return -1;
  while ((c = *str++)) {
    if ('0' <= c && c <= '9') {
      x *= 10;
      x += c - '0';
    } else {
      return -1;
    }
  }
  return x;
}

// [rust-ape] The path is converted and the handle opened before the
// descriptor table is locked. Upstream holds __fds_lock across both, which
// serializes every open in the process behind one CreateFile round trip,
// and orders that lock ahead of whatever the path conversion needs. The
// /proc emulation (shim/procfs/) answers a path from under its own lock
// while writing files, so a thread opening /proc/... (table lock, wanting
// the tree lock) and a thread generating an entry (tree lock, wanting the
// table lock for its scratch file) deadlocked. cosmopolitan master is
// arranged the same way. The /dev/ names still resolve under the lock; they
// convert no path. A failed reservation also releases the lock, which
// upstream left held.
textwindows int sys_open_nt(int dirfd, const char *file, uint32_t flags,
                            int32_t mode) {
  ssize_t rc;
  int fd, oldfd;
  int64_t handle;
  BLOCK_SIGNALS;
  if (!(flags & _O_CREAT))
    mode = 0;
  if (startswith(file, "/dev/")) {
    __fds_lock();
    if ((rc = fd = __reservefd_unlocked(-1)) != -1) {
      if (!strcmp(file + 5, "tty")) {
        rc = sys_open_nt_special(fd, flags, mode, kFdConsole, u"CONIN$");
      } else if (!strcmp(file + 5, "null")) {
        rc = sys_open_nt_special(fd, flags, mode, kFdDevNull, u"NUL");
      } else if (!strcmp(file + 5, "urandom") || !strcmp(file + 5, "random")) {
        rc = sys_open_nt_no_handle(fd, flags, mode, kFdDevRandom);
      } else if (!strcmp(file + 5, "stdin")) {
        rc = sys_open_nt_dup(fd, flags, mode, STDIN_FILENO);
      } else if (!strcmp(file + 5, "stdout")) {
        rc = sys_open_nt_dup(fd, flags, mode, STDOUT_FILENO);
      } else if (!strcmp(file + 5, "stderr")) {
        rc = sys_open_nt_dup(fd, flags, mode, STDERR_FILENO);
      } else if (startswith(file + 5, "fd/") &&
                 (oldfd = Atoi(file + 8)) != -1) {
        rc = sys_open_nt_dup(fd, flags, mode, oldfd);
      } else {
        rc = enoent();
      }
      if (rc == -1) {
        __releasefd(fd);
      }
    }
    __fds_unlock();
  } else if ((handle = sys_open_nt_impl(dirfd, file, flags, mode,
                                        kNtFileFlagOverlapped)) != -1) {
    __fds_lock();
    if ((rc = fd = __reservefd_unlocked(-1)) != -1) {
      g_fds.p[fd].cursor = __cursor_new();
      g_fds.p[fd].handle = handle;
      g_fds.p[fd].kind = kFdFile;
      g_fds.p[fd].flags = flags;
      g_fds.p[fd].mode = mode;
    }
    __fds_unlock();
    if (rc == -1) {
      CloseHandle(handle);
    }
  } else {
    rc = -1;
  }
  ALLOW_SIGNALS;
  return rc;
}
