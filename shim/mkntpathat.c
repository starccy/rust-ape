// __mkntpathat/__mkntpathath for the Linux-personality shim, replacing
// cosmo 4.0.2's libc.a(mkntpathat.o).
//
// A dirfd-relative syscall on NT (the whole *at family) resolves dirfd
// to a directory path with GetFinalPathNameByHandle(), which answers in
// the \\?\ namespace. That namespace bypasses win32 normalization and
// breaks some APIs, so the result is downgraded before use -- and the
// downgrade must know both shapes the answer takes: \\?\C:\x becomes
// C:\x, and \\?\UNC\srv\share\x becomes \\srv\share\x. (Upstream only
// knew the drive form and chopped four chars off either way, which
// turns the UNC form into a relative path.)
//
// This file defines both of the archive member's entry points, not just
// the changed one, so libc.a(mkntpathat.o) is never pulled in: ntspawn
// and posix_spawn call __mkntpathath directly, and a member pulled for
// that would collide with our __mkntpathat. Everything except the
// downgrade branch is a faithful copy of upstream, including the quirk
// that the returned length is not shortened by the downgrade (callers
// only read the buffer up to its NUL, and the last-char peek
// __mkntpathath does at the stale offset lands on the same character
// either way).
//
// Derived from cosmopolitan libc/calls/mkntpathat.c,
// Copyright 2021 Justine Alexandra Roberts Tunney, ISC license.
// Revisit on toolchain upgrade: master has reshuffled these files, and
// whether it still has the bug needs a fresh look.

// The cosmo-internal headers below only work when _COSMO_SOURCE was
// defined before the compiler's -include of normalize.inc, so the linker
// wrapper compiles this one file with -D_COSMO_SOURCE (an in-file define
// would come too late).
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include <libc/calls/internal.h>
#include <libc/calls/syscall_support-nt.internal.h>
#include <libc/macros.h>
#include <libc/nt/enum/fileflagandattributes.h>
#include <libc/nt/files.h>
#include <libc/str/str.h>
#include <libc/sysv/consts/at.h>
#include <libc/sysv/errfuns.h>

// [rust-ape] shim/procfs/core/: a dirfd-relative access that lands inside
// the materialized /proc tree refreshes what it is about to touch.
void __ape_shim_procfs_relative(const char16_t *, unsigned long);

static int IsAlpha(int c) {
  return ('A' <= c && c <= 'Z') || ('a' <= c && c <= 'z');
}

static bool IsAbsolutePathWin32(char16_t *path) {
  if (path[0] == '\\')
    return true;
  if (IsAlpha(path[0]) && path[1] == ':')
    return true;
  return false;
}

static textwindows int __mkntpathath_impl(int64_t dirhand, const char *path,
                                          int flags,
                                          char16_t file[hasatleast PATH_MAX]) {
  size_t n;
  char16_t dir[PATH_MAX];
  uint32_t dirlen, filelen;
  if (!isutf8(path, -1))
    return eilseq();  // thwart overlong nul in conversion
  if ((filelen = __mkntpath2(path, file, flags)) == -1)
    return -1;
  if (!filelen)
    return enoent();
  if (dirhand != AT_FDCWD && !IsAbsolutePathWin32(file)) {
    dirlen = GetFinalPathNameByHandle(dirhand, dir, ARRAYLEN(dir),
                                      kNtFileNameNormalized | kNtVolumeNameDos);
    if (!dirlen)
      return __winerr();
    if (dirlen + 1 + filelen + 1 > ARRAYLEN(dir))
      return enametoolong();
    dir[dirlen] = u'\\';
    memcpy(dir + dirlen + 1, file, (filelen + 1) * sizeof(char16_t));
    memcpy(file, dir, ((n = dirlen + 1 + filelen) + 1) * sizeof(char16_t));
    n = __normntpath(file, n);

    // \\?\-namespace paths break some things when they are not needed;
    // downgrade both forms it takes (drive and UNC), and keep the long
    // form when the result would not fit classic MAX_PATH anyway.
    if (n > 4 &&            //
        file[0] == '\\' &&  //
        file[1] == '\\' &&  //
        file[2] == '?' &&   //
        file[3] == '\\') {
      if (n > 8 &&           //
          file[4] == 'U' &&  //
          file[5] == 'N' &&  //
          file[6] == 'C' &&  //
          file[7] == '\\') {
        // \\?\UNC\srv\share\x -> \\srv\share\x
        if (n - 6 < 260) {
          memmove(file + 2, file + 8, (n - 8 + 1) * sizeof(char16_t));
        }
      } else if (n < 260) {
        // \\?\C:\x -> C:\x
        memmove(file, file + 4, (n - 4 + 1) * sizeof(char16_t));
      }
    }

    __ape_shim_procfs_relative(file, n); // [rust-ape]
    return n;
  } else {
    filelen = __normntpath(file, filelen);
    return filelen;
  }
}

textwindows int __mkntpathath(int64_t dirhand, const char *path, int flags,
                              char16_t file[hasatleast PATH_MAX]) {

  // convert the path.
  int len;
  if ((len = __mkntpathath_impl(dirhand, path, flags, file)) == -1)
    return -1;

  // if path ends with a slash, then we need to manually do what linux
  // does and check to make sure it's a directory, and return ENOTDIR,
  // since WIN32 will reject the path with EINVAL if we don't do this.
  if (len && file[len - 1] == '\\') {
    uint32_t fattr;
    if (len > 1 && !(len == 3 && file[1] == ':'))
      file[--len] = 0;
    if ((fattr = GetFileAttributes(file)) != -1u &&
        !(fattr & kNtFileAttributeReparsePoint) &&
        !(fattr & kNtFileAttributeDirectory))
      return enotdir();
  }

  return len;
}

textwindows int __mkntpathat(int dirfd, const char *path, int flags,
                             char16_t file[hasatleast PATH_MAX]) {
  int64_t dirhand;
  if (dirfd == AT_FDCWD) {
    dirhand = AT_FDCWD;
  } else if (__isfdkind(dirfd, kFdFile)) {
    dirhand = g_fds.p[dirfd].handle;
  } else {
    return ebadf();
  }
  return __mkntpathath(dirhand, path, flags, file);
}
