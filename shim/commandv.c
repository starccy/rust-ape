// [rust-ape] commandv (a.k.a. __commandv), replacing cosmo 4.0.2's
// libc.a(commandv.o) so $PATH lookup on NT resolves the host's
// executable suffixes.
//
// commandv is the single resolver behind posix_spawnp() and
// execvp()/execvpe(). Upstream probes each PATH directory for the bare
// name only, but NT executables carry a suffix, and every resolver
// native to the platform honors that: CreateProcess appends .exe
// itself, and the Cygwin/MSYS exec layers try the suffixes during
// their PATH search. This copy does the same: the bare name keeps
// priority, and only when it misses and the last path component has no
// dot are ".exe" and ".com" tried, NT only -- POSIX hosts keep
// upstream behavior bit for bit. The resolved path (suffix included)
// is what the caller execs, so nothing else has to repeat the trick.
//
// The rest is a faithful copy of upstream, reshaped only by hoisting the
// per-candidate probe into IsExecutableFile(). Compiled with
// -D_COSMO_SOURCE by the linker wrapper. Revisit on toolchain upgrade.
//
// Derived from cosmopolitan libc/calls/commandv.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.

// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include "libc/calls/calls.h"
#include "libc/calls/struct/stat.h"
#include "libc/dce.h"
#include "libc/errno.h"
#include "libc/paths.h"
#include "libc/runtime/runtime.h"
#include "libc/str/str.h"
#include "libc/sysv/consts/ok.h"
#include "libc/sysv/consts/s.h"
#include "libc/sysv/errfuns.h"

static bool IsExecutableFile(const char *path, bool *seen_eacces) {
  if (!access(path, X_OK)) {
    struct stat st;
    if (!stat(path, &st) && S_ISREG(st.st_mode))
      return true;
  } else if (errno == EACCES) {
    *seen_eacces = true;
  }
  return false;
}

/**
 * Resolves full pathname of executable.
 *
 * @return execve()'able path, or NULL w/ errno
 * @errno ENOENT, EACCES, ENOMEM
 * @see free(), execvpe()
 * @asyncsignalsafe
 * @vforksafe
 */
char *commandv(const char *name, char *pathbuf, size_t pathbufsz) {

  // bounce empty names
  size_t namelen;
  if (!(namelen = strlen(name))) {
    enoent();
    return 0;
  }

  // get system path
  const char *syspath;
  if (memchr(name, '/', namelen)) {
    syspath = "";
  } else if (!(syspath = getenv("PATH"))) {
    syspath = _PATH_DEFPATH;
  }

  // [rust-ape] a name whose last component already has an extension
  // (git.exe, foo.bat) is taken literally; only extensionless names
  // get the NT suffix retries below.
  const char *dot = strrchr(name, '.');
  const char *slash = strrchr(name, '/');
  bool suffixable = IsWindows() && (!dot || (slash && dot < slash));

  // iterate through directories
  int old_errno = errno;
  bool seen_eacces = false;
  const char *b, *a = syspath;
  errno = ENOENT;
  do {
    b = strchrnul(a, ':');
    size_t dirlen = b - a;
    if (dirlen + 1 + namelen < pathbufsz) {
      size_t len;
      if (dirlen) {
        memcpy(pathbuf, a, dirlen);
        pathbuf[dirlen] = '/';
        memcpy(pathbuf + dirlen + 1, name, namelen + 1);
        len = dirlen + 1 + namelen;
      } else {
        memcpy(pathbuf, name, namelen + 1);
        len = namelen;
      }
      if (IsExecutableFile(pathbuf, &seen_eacces)) {
        errno = old_errno;
        return pathbuf;
      }
      // [rust-ape] the host resolves bare names against its executable
      // suffixes; give the unix-shaped caller the same courtesy
      if (suffixable && len + 4 + 1 <= pathbufsz) {
        static const char kSuffixes[2][5] = {".exe", ".com"};
        for (int i = 0; i < 2; ++i) {
          memcpy(pathbuf + len, kSuffixes[i], 5);
          if (IsExecutableFile(pathbuf, &seen_eacces)) {
            errno = old_errno;
            return pathbuf;
          }
        }
      }
    } else {
      enametoolong();
    }
    a = b + 1;
  } while (*b);

  // return error if not found
  if (seen_eacces) {
    errno = EACCES;
  }
  return 0;
}
