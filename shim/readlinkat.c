// [rust-ape] readlinkat, replacing cosmo 4.0.2's libc.a(readlinkat.o) for one
// addition marked [rust-ape] below: /proc-shaped link reads are answered by
// shim/procfs/ on Windows.
//
// Three spellings reach here. readlinkat(fd, "") makes Linux read the
// descriptor itself (allowed since 2.6.39), the only way to read a
// /proc/<pid>/fd entry without racing the process that owns it, and what
// the procfs crate does. readlinkat(dirfd, "exe") names a link relative to a directory it
// already holds. And readlink("/proc/self/exe") is the absolute form. Cosmo
// knows none of them as special: the emulated tree stores links as plain
// files (NT cannot open a real symlink the way these callers name one), so
// all three must be answered from the emulation, and a path that is not its
// business falls through to the upstream code unchanged.
//
// Everything below the [rust-ape] block is a faithful copy of upstream.
// Compiled with -D_COSMO_SOURCE by the linker wrapper. Revisit on toolchain
// upgrade.
//
// Derived from cosmopolitan libc/calls/readlinkat.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // cosmo's own build has C23 bool
#include "libc/calls/calls.h"
#include "libc/calls/syscall-nt.internal.h"
#include "libc/calls/syscall-sysv.internal.h"
#include "libc/dce.h"
#include "libc/intrin/describeflags.h"
#include "libc/intrin/kprintf.h"
#include "libc/intrin/strace.h"
#include "libc/intrin/weaken.h"
#include "libc/runtime/runtime.h"
#include "libc/runtime/zipos.internal.h"
#include "libc/stdio/sysparam.h"
#include "libc/str/str.h"
#include "libc/sysv/errfuns.h"

// [rust-ape] shim/procfs/core/: link text for anything /proc-shaped it
// owns, or negative for a read that is not its business.
long __ape_shim_procfs_readlinkat(int, const char *, char *, unsigned long);

// [rust-ape] Whether an absolute path names this process's own exe link.
static bool IsOwnExeLink(const char *path) {
  if (strncmp(path, "/proc/", 6))
    return false;
  path += 6;
  if (!strncmp(path, "self/", 5)) {
    path += 5;
  } else {
    int pid = getpid();
    char num[12], *q = num + sizeof(num);
    *--q = 0;
    do
      *--q = '0' + pid % 10;
    while ((pid /= 10));
    size_t n = strlen(q);
    if (strncmp(path, q, n) || path[n] != '/')
      return false;
    path += n + 1;
  }
  return !strcmp(path, "exe");
}

// [rust-ape] Whether link text names the ape loader rather than a program,
// the same spelling cosmo's own executable-name lookup rejects.
static bool IsApeLoader(const char *s, size_t n) {
  if (n == 12 && !memcmp(s, "/usr/bin/ape", 12))
    return true;
  const char *b = s + n;
  while (b > s && b[-1] != '/')
    b--;
  if (s + n - b < 6 || memcmp(b, ".ape-", 5))
    return false;
  for (b += 5; b < s + n; b++)
    if (!(*b >= '0' && *b <= '9') && *b != '.')
      return false;
  return true;
}

ssize_t readlinkat(int dirfd, const char *path, char *buf, size_t bufsiz) {
  char mybuf[1];
  ssize_t bytes;

  // [rust-ape] the emulated /proc, in any of its three spellings.
  if (bufsiz && !kisdangerous(path)) {
    long r = __ape_shim_procfs_readlinkat(dirfd, path, buf, bufsiz);
    if (r >= 0) {
      STRACE("readlinkat(%d, %#s, [%#.*s]) → %d% m", dirfd, path, (int)r, buf,
             r);
      return r;
    }
  }

  if (IsLinux() && !bufsiz) {
    // the linux kernel will einval if bufsiz is zero. linux is the only
    // os that does this. it's much simpler if we reserve einval for the
    // important thing posix specifies, which is path not being symlink.
    buf = mybuf;
    bufsiz = 1;
  }
  if (kisdangerous(path)) {
    bytes = efault();
  } else if (_weaken(__zipos_notat) &&
             (bytes = __zipos_notat(dirfd, path)) == -1) {
    STRACE("TODO: zipos support for readlinkat");
    bytes = einval();
  } else if (!IsWindows()) {
    bytes = sys_readlinkat(dirfd, path, buf, bufsiz);
    // [rust-ape] On Linux the kernel names the file it exec'd, which is the
    // ape loader when an APE is run through one. A program locating
    // itself this way (Rust's current_exe) would re-exec, self-update or
    // find its resources next to the loader, so answer with the program
    // the loader ran instead, which cosmo already knows.
    if (IsLinux() && bytes > 0 && IsOwnExeLink(path) &&
        IsApeLoader(buf, bytes)) {
      const char *exe = GetProgramExecutableName();
      size_t n = strlen(exe);
      if (n && *exe == '/' && !IsApeLoader(exe, n)) {
        if (n > bufsiz)
          n = bufsiz;
        memcpy(buf, exe, n);
        bytes = n;
      }
    }
  } else {
    bytes = sys_readlinkat_nt(dirfd, path, buf, bufsiz);
  }
  if (IsLinux() && buf == mybuf && bytes == 1)
    bytes = 0;
  STRACE("readlinkat(%s, %#s, [%#.*s]) → %d% m", DescribeDirfd(dirfd), path,
         (int)MAX(0, bytes), buf, bytes);
  return bytes;
}
