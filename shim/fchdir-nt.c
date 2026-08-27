// [rust-ape] sys_fchdir_nt, replacing cosmo 4.0.2's libc.a(fchdir-nt.o):
// the \\?\ prefix is stripped from the path before it becomes the cwd.
//
// Upstream resolves the descriptor to a path with
// GetFinalPathNameByHandle, which always answers in the \\?\ namespace,
// and hands that straight to SetCurrentDirectory. The \\?\ prefix
// disables win32 path normalization, so the cwd is stored verbatim, and
// every child spawned afterwards inherits a \\?\C:\x cwd that most
// programs cannot parse. chdir() does not have this problem: win32
// normalizes the stored cwd only for unprefixed input, so \\?\ input
// must not be stored at all.
//
// The fix is the same downgrade shim/mkntpathat.c documents: \\?\C:\x
// becomes C:\x and \\?\UNC\srv\share\x becomes \\srv\share\x. Only paths
// that still fit win32's classic limit are downgraded; a longer cwd
// genuinely needs the prefix (and no child could inherit it anyway).
// The rest is a faithful copy of upstream. Compiled with -D_COSMO_SOURCE
// by the linker wrapper. Revisit on toolchain upgrade.
//
// Derived from cosmopolitan libc/calls/fchdir-nt.c,
// Copyright 2023 Justine Alexandra Roberts Tunney, ISC license.

// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include "libc/calls/calls.h"
#include "libc/calls/internal.h"
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/dce.h"
#include "libc/limits.h"
#include "libc/macros.h"
#include "libc/nt/files.h"
#include "libc/str/str.h"
#include "libc/sysv/errfuns.h"

int sys_chdir_nt_impl(char16_t[hasatleast PATH_MAX], uint32_t);

textwindows int sys_fchdir_nt(int dirfd) {
  char16_t dir[PATH_MAX];
  if (!__isfdkind(dirfd, kFdFile))
    return ebadf();
  uint32_t len = GetFinalPathNameByHandle(
      g_fds.p[dirfd].handle, dir, ARRAYLEN(dir),
      kNtFileNameNormalized | kNtVolumeNameDos);
  if (len && len < ARRAYLEN(dir) && dir[0] == '\\' && dir[1] == '\\' &&
      dir[2] == '?' && dir[3] == '\\') {
    if (dir[4] == 'U' && dir[5] == 'N' && dir[6] == 'C' && dir[7] == '\\') {
      // \\?\UNC\srv\share\x -> \\srv\share\x
      if (len - 6 < 260) {
        memmove(dir + 2, dir + 8, (len - 8 + 1) * sizeof(char16_t));
        len -= 6;
      }
    } else {
      // \\?\C:\x -> C:\x
      if (len - 4 < 260) {
        memmove(dir, dir + 4, (len - 4 + 1) * sizeof(char16_t));
        len -= 4;
      }
    }
  }
  return sys_chdir_nt_impl(dir, len);
}
