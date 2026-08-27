/*-*- mode:c;indent-tabs-mode:nil;c-basic-offset:2;tab-width:8;coding:utf-8 -*-│
│ vi: set et ft=c ts=2 sts=2 sw=2 fenc=utf-8                               :vi │
╞══════════════════════════════════════════════════════════════════════════════╡
│ Copyright 2020 Justine Alexandra Roberts Tunney                              │
│                                                                              │
│ Permission to use, copy, modify, and/or distribute this software for         │
│ any purpose with or without fee is hereby granted, provided that the         │
│ above copyright notice and this permission notice appear in all copies.      │
│                                                                              │
│ THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL                │
│ WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED                │
│ WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE             │
│ AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL         │
│ DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR        │
│ PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER               │
│ TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR             │
│ PERFORMANCE OF THIS SOFTWARE.                                                │
╚─────────────────────────────────────────────────────────────────────────────*/
// [rust-ape] ntaccesscheck, replacing cosmo 4.0.2's
// libc.a(ntaccesscheck.o), the NT backend of access/faccessat. One
// change, marked [rust-ape] below: an execute check the DACL denies is
// retried by opening the file for execution. A network share may
// synthesize a DACL without execute while still honoring an open for
// execute, which is all CreateProcess needs, so files that run fine
// would fail access(X_OK). Only the denied path pays for the extra
// open.
//
// Everything else is a faithful copy. Compiled with -D_COSMO_SOURCE by
// the linker wrapper. Revisit on toolchain upgrade.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/assert.h"
#include "libc/calls/calls.h"
#include "libc/calls/internal.h"
#include "libc/calls/sig.internal.h"
#include "libc/calls/struct/sigset.internal.h"
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/dce.h"
#include "libc/errno.h"
#include "libc/intrin/strace.h"
#include "libc/intrin/weaken.h"
#include "libc/mem/mem.h"
#include "libc/nt/createfile.h"
#include "libc/nt/enum/accessmask.h"
#include "libc/nt/enum/creationdisposition.h"
#include "libc/nt/enum/fileflagandattributes.h"
#include "libc/nt/enum/filesharemode.h"
#include "libc/nt/enum/securityimpersonationlevel.h"
#include "libc/nt/enum/securityinformation.h"
#include "libc/nt/errors.h"
#include "libc/nt/files.h"
#include "libc/nt/runtime.h"
#include "libc/nt/struct/byhandlefileinformation.h"
#include "libc/nt/struct/genericmapping.h"
#include "libc/nt/struct/privilegeset.h"
#include "libc/nt/struct/securitydescriptor.h"
#include "libc/runtime/runtime.h"
#include "libc/sock/internal.h"
#include "libc/str/str.h"
#include "libc/sysv/consts/ok.h"
#include "libc/sysv/errfuns.h"

// [rust-ape] Asks the file system itself, by opening with the rights
// the caller wants. Execute implies read here as it does upstream.
static textwindows bool ntaccesscheck_open(const char16_t *pathname,
                                           uint32_t flags) {
  uint32_t rights = kNtFileGenericRead | kNtFileGenericExecute;
  if (flags & W_OK)
    rights |= kNtFileGenericWrite;
  int64_t hFile = CreateFile(
      pathname, rights,
      kNtFileShareRead | kNtFileShareWrite | kNtFileShareDelete, 0,
      kNtOpenExisting, kNtFileAttributeNormal | kNtFileFlagBackupSemantics, 0);
  if (hFile == -1)
    return false;
  struct NtByHandleFileInformation wst;
  bool ok = GetFileInformationByHandle(hFile, &wst) &&
            ((wst.dwFileAttributes & kNtFileAttributeDirectory) ||
             IsWindowsExecutable(hFile, pathname));
  CloseHandle(hFile);
  return ok;
}

/**
 * Asks Microsoft if we're authorized to use a folder or file.
 *
 * @param flags can have R_OK, W_OK, X_OK, etc.
 * @return 0 if authorized, or -1 w/ errno
 * @see https://blog.aaronballman.com/2011/08/how-to-check-access-rights/
 * @see libc/sysv/consts.sh
 */
textwindows int ntaccesscheck(const char16_t *pathname, uint32_t flags) {
  int rc;
  bool32 result;
  uint32_t flagmask;
  struct NtSecurityDescriptor *s;
  struct NtGenericMapping mapping;
  struct NtPrivilegeSet privileges;
  uint32_t secsize, granted, privsize;
  struct NtByHandleFileInformation wst;
  int64_t hToken, hImpersonatedToken, hFile;
  intptr_t buffer[1024 / sizeof(intptr_t)];
  BLOCK_SIGNALS;
  if (flags & X_OK)
    flags |= R_OK;
  granted = 0;
  result = false;
  flagmask = flags;
  s = (void *)buffer;
  secsize = sizeof(buffer);
  privsize = sizeof(privileges);
  bzero(&privileges, sizeof(privileges));
  mapping.GenericRead = kNtFileGenericRead;
  mapping.GenericWrite = kNtFileGenericWrite;
  mapping.GenericExecute = kNtFileGenericExecute;
  mapping.GenericAll = kNtFileAllAccess;
  MapGenericMask(&flagmask, &mapping);
  hImpersonatedToken = hToken = -1;
  if (GetFileSecurity(pathname,
                      kNtOwnerSecurityInformation |
                          kNtGroupSecurityInformation |
                          kNtDaclSecurityInformation,
                      s, secsize, &secsize)) {
    if (OpenProcessToken(GetCurrentProcess(),
                         kNtTokenImpersonate | kNtTokenQuery |
                             kNtTokenDuplicate | kNtStandardRightsRead,
                         &hToken)) {
      if (DuplicateToken(hToken, kNtSecurityImpersonation,
                         &hImpersonatedToken)) {
        if (AccessCheck(s, hImpersonatedToken, flagmask, &mapping, &privileges,
                        &privsize, &granted, &result)) {
          if (result || flags == F_OK) {
            if (flags & X_OK) {
              if ((hFile = CreateFile(
                       pathname, kNtFileGenericRead,
                       kNtFileShareRead | kNtFileShareWrite |
                           kNtFileShareDelete,
                       0, kNtOpenExisting,
                       kNtFileAttributeNormal | kNtFileFlagBackupSemantics,
                       0)) != -1) {
                unassert(GetFileInformationByHandle(hFile, &wst));
                if ((wst.dwFileAttributes & kNtFileAttributeDirectory) ||
                    IsWindowsExecutable(hFile, pathname)) {
                  rc = 0;
                } else {
                  rc = eacces();
                }
                CloseHandle(hFile);
              } else {
                rc = __winerr();
              }
            } else {
              rc = 0;
            }
          } else if ((flags & X_OK) && ntaccesscheck_open(pathname, flags)) {
            rc = 0;  // [rust-ape] the DACL said no but the file system says yes
          } else {
            NTTRACE("ntaccesscheck finale failed: result=%d flags=%x", result,
                    flags);
            rc = eacces();
          }
        } else {
          rc = __winerr();
          NTTRACE("%s(%#hs) failed: %s", "AccessCheck", pathname,
                  strerror(errno));
        }
      } else {
        rc = __winerr();
        NTTRACE("%s(%#hs) failed: %s", "DuplicateToken", pathname,
                strerror(errno));
      }
    } else {
      rc = __winerr();
      NTTRACE("%s(%#hs) failed: %s", "OpenProcessToken", pathname,
              strerror(errno));
    }
  } else {
    rc = __winerr();
    NTTRACE("%s(%#hs) failed: %s", "GetFileSecurity", pathname,
            strerror(errno));
  }
  if (hImpersonatedToken != -1) {
    CloseHandle(hImpersonatedToken);
  }
  if (hToken != -1) {
    CloseHandle(hToken);
  }
  ALLOW_SIGNALS;
  return rc;
}
