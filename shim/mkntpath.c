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
// [rust-ape] __mkntpath/__mkntpath2/__normntpath, replacing cosmo 4.0.2's
// libc.a(mkntpath.o) for a few additions in __mkntpath2, marked [rust-ape]
// below.
//
// First, a win32-absolute segment embedded mid-path re-roots the path.
// The Rust world runs with unix Path semantics, which treat a
// win32-absolute path (C:\x, \\server\share\x) as relative and join it
// onto the cwd. This function already accepts "c:\foo" at position 0;
// the addition extends that to "last absolute segment wins", the same
// rule Path::join applies to segments it recognizes as absolute. The
// marker (drive letter, colon, separator — or a \\server\share pair)
// cannot occur inside real NT components: colons only appear in ADS
// names, which are terminal and never followed by a separator. The scan
// runs before all other handling, so every conversion (absolute,
// dirfd-relative, /tmp remap) sees the re-rooted path.
//
// Second, a UNC share written with a single leading slash is recognized.
// cosmo spells \\server\share as //server/share, the only unix form
// there is, but POSIX leaves a leading "//" implementation-defined and
// unix path code (Rust's Path, realpath-style normalizers) collapses it
// to "/server/share/x". NT reads that as a path rooted on the current
// drive, which on a UNC cwd is the share root itself, so a file saved
// under the collapsed cwd landed in <share>/server/share/x with the
// directories silently created. Every server/share pair this process
// has seen as a real UNC root (its cwd, any "//server/share" input) is
// remembered, and a single-slash path whose first two components match
// one is restored to the UNC form. A registered pair is required, so an
// ordinary rooted path like /usr/x is never touched.
//
// Third, a rooted path with no drive ("/", "/bin/sh") is pinned to the
// cosmos drive (shim/cosmosdrive.c), as master does, instead of being
// handed to NT as root-relative and landing on whatever drive or share
// the cwd is on. And a relative path is joined onto the cwd where NT's
// own resolution differs from unix (shim/uncshare.c): ".." above a
// share root, and any name under the materialized "//server" directory.
//
// Everything else is a faithful copy of upstream. Compiled with
// -D_COSMO_SOURCE by the linker wrapper. Revisit on toolchain upgrade.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/dce.h"

#include "libc/intrin/kprintf.h"
#include "libc/intrin/strace.h"
#include "libc/macros.h"
#include "libc/nt/files.h"       // [rust-ape]
#include "libc/nt/systeminfo.h"
#include "libc/stdio/stdio.h"  // [rust-ape]
#include "libc/str/str.h"
#include "libc/sysv/consts/o.h"
#include "libc/sysv/errfuns.h"

// [rust-ape] shim/procfs/core/
int __ape_shim_procfs_rewrite(const char *, char *, unsigned long);
// [rust-ape] shim/uncshare.c
int __ape_shim_unc_server_dir(const char *, size_t, char *, size_t);
int __ape_shim_unc_rel(const char *, char *, size_t);
// [rust-ape] shim/cosmosdrive.c
char __ape_shim_cosmos_drive(void);

static inline bool IsSlash(char c) {
  return c == '/' || c == '\\';
}

static inline int IsAlpha(int c) {
  return ('A' <= c && c <= 'Z') || ('a' <= c && c <= 'z');
}

// [rust-ape] UNC roots seen by this process, as "server/share" in the
// spelling first seen; matched ASCII case-insensitively, the way NT
// matches names. Writers take the lock; readers only see an entry once
// the count that publishes it is stored, so lookups are lock-free.
#define UNC_ROOT_MAX 16
#define UNC_ROOT_LEN 256
static char unc_roots[UNC_ROOT_MAX][UNC_ROOT_LEN];
static int unc_count;
static int unc_lock;

static inline int Lower(int c) {
  return ('A' <= c && c <= 'Z') ? c + ('a' - 'A') : c;
}

// Length of the "server/share" prefix at p (which follows the leading
// slashes), or 0 when there isn't one. Device namespaces (//?/, //./)
// and the unix-side "//" root alone are not shares.
static size_t UncRootLen(const char *p) {
  size_t i = 0;
  if (!p[0] || IsSlash(p[0]) || p[0] == '?' || p[0] == '.')
    return 0;
  while (p[i] && !IsSlash(p[i]))
    i++;
  if (!IsSlash(p[i]) || !p[i + 1] || IsSlash(p[i + 1]))
    return 0;
  i++;
  size_t share = i;
  while (p[i] && !IsSlash(p[i]))
    i++;
  if (p[share] == '.' && (i - share == 1 || (i - share == 2 && p[share + 1] == '.')))
    return 0;
  return i < UNC_ROOT_LEN ? i : 0;
}

// [rust-ape] Whether "/x" names drive x, which is so only when that drive
// exists; otherwise the single letter is an ordinary name under the
// cosmos drive, the same as "/bin". The drive bitmap is cached so the
// common "/C/..." costs nothing; a letter not in it asks the OS again,
// so a drive attached after startup is still found.
static bool DriveExists(int letter) {
  static uint32_t drives;
  uint32_t bit = 1u << (Lower(letter) - 'a');
  if (__atomic_load_n(&drives, __ATOMIC_RELAXED) & bit)
    return true;
  uint32_t now = GetLogicalDrives();
  __atomic_store_n(&drives, now, __ATOMIC_RELAXED);
  return now & bit;
}

// [rust-ape] the same test for shim/mkntcmdline.c, so a spawn rewrites
// "/x/" for a native child only where the child would read it as drive x.
int __ape_shim_drive_exists(int letter) {
  return DriveExists(letter);
}

// [rust-ape] whether any UNC root has been seen, so the relative-path
// hook can stay free for processes that never touch a share.
int __ape_shim_unc_any(void) {
  return __atomic_load_n(&unc_count, __ATOMIC_RELAXED) > 0;
}

static bool UncRootEquals(const char *root, const char *p, size_t n) {
  for (size_t i = 0; i < n; i++) {
    int a = root[i], b = p[i];
    if (IsSlash(a) && IsSlash(b))
      continue;
    if (Lower(a) != Lower(b))
      return false;
  }
  return !root[n];
}

static bool UncRootKnown(const char *p, size_t n) {
  int count = __atomic_load_n(&unc_count, __ATOMIC_ACQUIRE);
  for (int i = 0; i < count; i++)
    if (UncRootEquals(unc_roots[i], p, n))
      return true;
  return false;
}

// Remembers the share a unix-spelled UNC path ("//server/share/...")
// names. Safe to call with any path; anything else is ignored.
void __ape_shim_unc_note(const char *path) {
  if (!path || !IsSlash(path[0]) || !IsSlash(path[1]))
    return;
  const char *p = path + 2;
  size_t n = UncRootLen(p);
  if (!n || UncRootKnown(p, n))
    return;
  while (__atomic_exchange_n(&unc_lock, 1, __ATOMIC_ACQUIRE))
    ;
  int count = unc_count;
  if (!UncRootKnown(p, n) && count < UNC_ROOT_MAX) {
    memcpy(unc_roots[count], p, n);
    unc_roots[count][n] = 0;
    __atomic_store_n(&unc_count, count + 1, __ATOMIC_RELEASE);
  }
  __atomic_store_n(&unc_lock, 0, __ATOMIC_RELEASE);
}

// True when some remembered share lives on this server.
static bool UncServerKnown(const char *p, size_t n) {
  int count = __atomic_load_n(&unc_count, __ATOMIC_ACQUIRE);
  for (int i = 0; i < count; i++) {
    const char *r = unc_roots[i];
    size_t k = 0;
    while (k < n && r[k] && Lower(r[k]) == Lower(p[k]))
      k++;
    if (k == n && IsSlash(r[k]))
      return true;
  }
  return false;
}

// True when path is "/server/share..." for a remembered share, i.e. a
// UNC path whose leading "//" was collapsed by unix path code. A bare
// "/server" counts when a share on it is remembered, since that is what
// Path::parent of a collapsed share root yields.
static bool IsCollapsedUnc(const char *path);
int __ape_shim_unc_collapsed(const char *path) {
  return IsWindows() && path && IsCollapsedUnc(path);
}
static bool IsCollapsedUnc(const char *path) {
  if (!IsSlash(path[0]) || IsSlash(path[1]))
    return false;
  if (IsAlpha(path[1]) && (IsSlash(path[2]) || !path[2]))
    return false;  // a /c/... drive path
  if (!__atomic_load_n(&unc_count, __ATOMIC_ACQUIRE))
    return false;
  size_t n = UncRootLen(path + 1);
  if (n)
    return UncRootKnown(path + 1, n);
  size_t k = 1;
  while (path[k] && !IsSlash(path[k]))
    k++;
  if (path[k] && (!IsSlash(path[k]) || path[k + 1]))
    return false;
  return UncServerKnown(path + 1, k - 1);
}

textwindows size_t __normntpath(char16_t *p, size_t n) {
  size_t i, j;
  for (j = i = 0; i < n; ++i) {
    int c = p[i];
    if (c == '/') {
      c = '\\';
    }
    if (j > 1 && c == '\\' && p[j - 1] == '\\') {
      // matched "^/" or "//" but not "^//"
    } else if ((j && p[j - 1] == '\\') &&  //
               c == '.' &&                 //
               (i + 1 == n || IsSlash(p[i + 1]))) {
      // matched "/./" or "/.$"
      i += !(i + 1 == n);
    } else if ((j && p[j - 1] == '\\') &&         //
               c == '.' &&                        //
               (i + 1 < n && p[i + 1] == '.') &&  //
               (i + 2 == n || IsSlash(p[i + 2]))) {
      // matched "/../" or "/..$"
      while (j && p[j - 1] == '\\')
        --j;
      if (j && p[j - 1] == '.') {
        // matched "." before
        if (j >= 2 && p[j - 2] == '.' &&  //
            (j == 2 || p[j - 3] == '\\')) {
          // matched "^.." or "/.." before
          p[++j] = '.';
          ++j;
          continue;
        } else if (j == 1 || p[j - 2] == '\\') {
          // matched "^." or "/." before
          continue;
        }
      }
      while (j && p[j - 1] != '\\')
        --j;
    } else {
      p[j++] = c;
    }
  }
  p[j] = 0;
  return j;
}

// [rust-ape] Normalizes what follows a UNC root, in place. Components are
// resolved against a stack so "." vanishes and ".." pops one; a ".." with
// nothing left to pop is a climb out of the share, which the caller
// resolves. The result is "" or "\a\b", with a trailing slash kept when
// the input had one, since callers read it as "must be a directory".
textwindows static size_t __normuncrest(char16_t *p, size_t n,
                                        bool *climbed) {
  size_t i = 0, j = 0;
  bool trailing = n && (p[n - 1] == '\\' || p[n - 1] == '/');
  *climbed = false;
  while (i < n) {
    while (i < n && (p[i] == '\\' || p[i] == '/'))
      i++;
    size_t k = i;
    while (k < n && p[k] != '\\' && p[k] != '/')
      k++;
    size_t len = k - i;
    if (len == 1 && p[i] == '.') {
      // nothing
    } else if (len == 2 && p[i] == '.' && p[i + 1] == '.') {
      if (j) {
        while (j && p[j - 1] != '\\')
          j--;
        if (j)
          j--;
      } else {
        *climbed = true;
      }
    } else if (len) {
      p[j++] = '\\';
      if (j != i)
        memmove(p + j, p + i, len * sizeof(char16_t));
      j += len;
    }
    i = k;
  }
  if (trailing && j)
    p[j++] = '\\';
  p[j] = 0;
  return j;
}

textwindows int __mkntpath(const char *path,
                           char16_t path16[hasatleast PATH_MAX]) {
  return __mkntpath2(path, path16, -1);
}

/**
 * Copies path for Windows NT.
 *
 * This function does the following chores:
 *
 * 1. Converting UTF-8 to UTF-16
 * 2. Replacing forward-slashes with backslashes
 * 3. Fixing drive letter paths, e.g. `/c/` → `c:\`
 * 4. Add `\\?\` prefix for paths exceeding 260 chars
 * 5. Remapping well-known paths, e.g. `/dev/null` → `NUL`
 *
 * @param flags is used by open()
 * @param path16 is shortened so caller can prefix, e.g. \\.\pipe\, and
 *     due to a plethora of special-cases throughout the Win32 API
 * @return short count excluding NUL on success, or -1 w/ errno
 * @error ENAMETOOLONG
 */
textwindows int __mkntpath2(const char *path,
                            char16_t path16[hasatleast PATH_MAX], int flags) {
  // 1. Need +1 for NUL-terminator
  // 2. Need +1 for UTF-16 overflow
  // 3. Need ≥2 for SetCurrentDirectory trailing slash requirement
  // 4. Need ≥13 for mkdir() i.e. 1+8+3+1, e.g. "\\ffffffff.xxx\0"
  //    which is an "8.3 filename" from the DOS days

  if (!path) {
    return efault();
  }

  // [rust-ape] /proc, which NT does not have and shim/procfs/ emulates by
  // materializing a skeleton under the temp directory. Done here because
  // this is the one place every path-taking call on NT converts its path,
  // so open, stat, opendir and the rest all reach the tree without each
  // needing to know about it.
  char procbuf[600];
  if (__ape_shim_procfs_rewrite(path, procbuf, sizeof procbuf)) {
    path = procbuf;
  }

  // [rust-ape] last absolute win32 segment wins: an "X:" drive marker
  // right after a separator (or a "\\" UNC pair after a slash) starts
  // an absolute path, so everything before it is a spurious prefix.
  // Scan for the LAST such marker so repeated joins still resolve to
  // the innermost intent.
  for (const char *s = path; *s; s++) {
    if (s > path && !IsSlash(s[-1]))
      continue;
    if (IsAlpha(s[0]) && s[1] == ':' && IsSlash(s[2])) {
      path = s;
    } else if (s > path && s[-1] == '/' && s[0] == '\\' && s[1] == '\\' &&
               !IsSlash(s[2])) {
      path = s;
    }
  }

  // [rust-ape] a relative path is NT's to resolve against the cwd, except
  // where NT's answer differs from unix: it clamps ".." at a share root,
  // and the "//server" directory it is resolved against may be the
  // materialized one. Those are joined onto the cwd here and converted
  // as absolute paths.
  char relbuf[PATH_MAX];
  if (path[0] && !IsSlash(path[0]) && !(IsAlpha(path[0]) && path[1] == ':') &&
      __ape_shim_unc_rel(path, relbuf, sizeof relbuf)) {
    path = relbuf;
  }

  // [rust-ape] a UNC root is learned from every path that spells one,
  // and restored to a path that lost its second slash on the way through
  // unix path code (see the header comment).
  char uncbuf[PATH_MAX];
  if (IsSlash(path[0]) && IsSlash(path[1])) {
    __ape_shim_unc_note(path);
  } else if (IsCollapsedUnc(path)) {
    size_t len = strlen(path);
    if (len + 2 > sizeof uncbuf)
      return enametoolong();
    uncbuf[0] = '/';
    memcpy(uncbuf + 1, path, len + 1);
    path = uncbuf;
  }

  // [rust-ape] "//server" alone, which NT has no directory for, is
  // diverted to the share list shim/uncshare.c materializes.
  char srvbuf[600];
  if (IsSlash(path[0]) && IsSlash(path[1]) && path[2] && !IsSlash(path[2]) &&
      path[2] != '?' && path[2] != '.') {
    size_t k = 2;
    while (path[k] && !IsSlash(path[k]))
      k++;
    if ((!path[k] || !path[k + 1]) &&
        __ape_shim_unc_server_dir(path + 2, k - 2, srvbuf, sizeof srvbuf)) {
      path = srvbuf;
    }
  }

  size_t x, z;
  bool uncroot = false;  // [rust-ape] p sits right after "\\?\UNC\srv\share"
  char root8[UNC_ROOT_LEN];  // [rust-ape] "srv/share" of a UNC path
  char16_t *p = path16;
  const char *q = path;
  if (IsSlash(q[0]) && IsAlpha(q[1]) && IsSlash(q[2]) && DriveExists(q[1])) {
    z = MIN(32767, PATH_MAX);
    // turn "\c\foo" into "\\?\c:\foo"
    p[0] = '\\';
    p[1] = '\\';
    p[2] = '?';
    p[3] = '\\';
    p[4] = q[1];
    p[5] = ':';
    p[6] = '\\';
    p += 7;
    q += 3;
    z -= 7;
    x = 7;
  } else if (IsSlash(q[0]) && IsAlpha(q[1]) && !q[2] && DriveExists(q[1])) {
    z = MIN(32767, PATH_MAX);
    // turn "\c" into "\\?\c:\"
    p[0] = '\\';
    p[1] = '\\';
    p[2] = '?';
    p[3] = '\\';
    p[4] = q[1];
    p[5] = ':';
    p[6] = '\\';
    p += 7;
    q += 2;
    z -= 7;
    x = 7;
  } else if (IsAlpha(q[0]) && q[1] == ':' && IsSlash(q[2])) {
    z = MIN(32767, PATH_MAX);
    // turn "c:\foo" into "\\?\c:\foo"
    p[0] = '\\';
    p[1] = '\\';
    p[2] = '?';
    p[3] = '\\';
    p[4] = q[0];
    p[5] = ':';
    p[6] = '\\';
    p += 7;
    q += 3;
    z -= 7;
    x = 7;
  } else if (IsSlash(q[0]) && IsSlash(q[1]) && q[2] == '?' && IsSlash(q[3])) {
    z = MIN(32767, PATH_MAX);
    x = 0;
  } else if (IsSlash(q[0]) && IsSlash(q[1]) && q[2] && !IsSlash(q[2]) &&
             q[2] != '?' && q[2] != '.') {
    // [rust-ape] turn "\\srv\share\foo" into "\\?\UNC\srv\share\foo", so a
    // share path gets the same long-path headroom a drive path has. The
    // server and share are copied as a fixed root that the ".." handling
    // below cannot climb out of, the way "/.." stays at "/" on unix;
    // upstream let ".." pop the share and NT rejected the result.
    z = MIN(32767, PATH_MAX);
    p[0] = '\\';
    p[1] = '\\';
    p[2] = '?';
    p[3] = '\\';
    p[4] = 'U';
    p[5] = 'N';
    p[6] = 'C';
    p[7] = '\\';
    p += 8;
    q += 2;
    z -= 8;
    x = 8;
    size_t rootlen = UncRootLen(q);
    if (rootlen) {
      memcpy(root8, q, rootlen);
      root8[rootlen] = 0;
      size_t r16 = tprecode8to16(p, z, root8).ax;
      if (r16 >= z - 1)
        return enametoolong();
      for (size_t i = 0; i < r16; i++)
        if (p[i] == '/')
          p[i] = '\\';
      p += r16;
      q += rootlen;
      z -= r16;
      x += r16;
      uncroot = true;
    }
  } else if (IsSlash(q[0]) &&
             !(q[1] == 't' && q[2] == 'm' && q[3] == 'p' &&
               (IsSlash(q[4]) || !q[4]))) {
    // [rust-ape] turn "\foo" and "\" into "\\?\c:\foo", the drive chosen
    // by shim/cosmosdrive.c, as master does. Upstream passed "\foo"
    // through, which NT resolves against the drive or share of the cwd,
    // so /bin/sh named a different file from each cwd and nothing at all
    // from a share.
    z = MIN(32767, PATH_MAX);
    p[0] = '\\';
    p[1] = '\\';
    p[2] = '?';
    p[3] = '\\';
    p[4] = __ape_shim_cosmos_drive();
    p[5] = ':';
    p[6] = '\\';
    p += 7;
    q += 1;
    z -= 7;
    x = 7;
  } else {
    z = MIN(260, PATH_MAX);
    x = 0;
  }

  // turn /tmp into GetTempPath()
  size_t m;
  if (!x && IsSlash(q[0]) && q[1] == 't' && q[2] == 'm' && q[3] == 'p' &&
      (IsSlash(q[4]) || !q[4])) {
    m = GetTempPath(z, p);
    if (!q[4])
      return m;
    q += 5;
    p += m;
    z -= m;
  } else {
    m = 0;
  }

  // turn utf-8 into utf-16
  size_t n = tprecode8to16(p, z, q).ax;
  if (n >= z - 1) {
    return enametoolong();
  }

  // normalize path
  // we need it because \\?\... paths have to be normalized
  // we don't remove the trailing slash since it is special
  // [rust-ape] what follows a UNC root is relative to it and normalized
  // on its own, so ".." can never pop the share. One that climbs out of
  // it lands in the server directory shim/uncshare.c provides, the way
  // "/x/.." is "/" on unix: the climb is resolved by converting
  // "//server" plus whatever remains. When the server has no such
  // directory the path is clamped at the share root instead.
  if (uncroot) {
    bool climbed;
    n = __normuncrest(p, n, &climbed);
    if (climbed) {
      size_t srvlen = 0;
      while (root8[srvlen] && !IsSlash(root8[srvlen]))
        srvlen++;
      char rest8[PATH_MAX], full[PATH_MAX];
      if (tprecode16to8(rest8, sizeof rest8, p).ax < sizeof rest8 - 1) {
        int len = snprintf(full, sizeof full, "//%.*s%s", (int)srvlen, root8,
                           rest8);
        if (len > 0 && (size_t)len < sizeof full &&
            __ape_shim_unc_server_dir(root8, srvlen, procbuf, sizeof procbuf))
          return __mkntpath2(full, path16, flags);
      }
    }
  } else {
    n = __normntpath(p, n);
  }

  // our path is now stored at `path16` with length `n`
  n = x + m + n;

  // To avoid toil like this:
  //
  //     "CMD.EXE was started with the above path as the current
  //      directory. UNC paths are not supported. Defaulting to Windows
  //      directory. Access is denied." -Quoth CMD.EXE
  //
  // Remove \\?\ prefix if we're within the 260 character limit.
  // [rust-ape] within 244, as master does: CreateDirectory wants room
  // for an 8.3 name on top, so a stripped path of 248 to 259 characters
  // was refused as too long
  if (n > 4 &&              //
      path16[0] == '\\' &&  //
      path16[1] == '\\' &&  //
      path16[2] == '?' &&   //
      path16[3] == '\\') {
    if (n > 8 &&           //
        path16[4] == 'U' &&  //
        path16[5] == 'N' &&  //
        path16[6] == 'C' &&  //
        path16[7] == '\\') {
      // [rust-ape] \\?\UNC\srv\share\x -> \\srv\share\x
      if (n - 6 < 244) {
        memmove(path16 + 2, path16 + 8, (n - 8 + 1) * sizeof(char16_t));
        n -= 6;
      }
    } else if (n < 244) {
      memmove(path16, path16 + 4, (n - 4 + 1) * sizeof(char16_t));
      n -= 4;
    }
  }

  return n;
}
