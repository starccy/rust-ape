// The unix-path rewrites that run before cosmo converts a path for
// win32, on NT. The fork's __mkntpathath() calls __ape_shim_ntpath_rewrite
// on every path it is handed, so open, stat, opendir and the rest all
// see the result without knowing about any of it.
//
// First, /proc, which NT does not have and shim/procfs/ emulates by
// materializing a skeleton under the temp directory.
//
// Second, a win32-absolute segment embedded mid-path re-roots the path.
// The Rust world runs with unix Path semantics, which treat a
// win32-absolute path (C:\x, \\server\share\x) as relative and join it
// onto the cwd. cosmo already accepts "c:\foo" at position 0; this
// extends that to "last absolute segment wins", the same rule Path::join
// applies to segments it recognizes as absolute. The marker (drive
// letter, colon, separator, or a \\server\share pair) cannot occur inside
// real NT components: colons only appear in ADS names, which are terminal
// and never followed by a separator.
//
// Third, a UNC share written with a single leading slash is recognized.
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
// Fourth, a relative path is joined onto the cwd where NT's own
// resolution differs from unix (shim/uncshare.c): ".." above a share
// root, and any name under the materialized "//server" directory. A
// ".." that climbs out of a share lands in that directory too, with
// whatever remained joined onto "//server"; where the server has no
// such directory the fork clamps the path at the share root instead.
//
// The fork itself pins a rooted path with no drive ("/", "/bin/sh") to
// the cosmos drive, treats "/x" as drive x only when that drive exists,
// and keeps ".." from climbing out of a share, so none of that is here.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/dce.h"
#include "libc/limits.h"
#include "libc/nt/systeminfo.h"
#include "libc/str/str.h"
#include "libc/sysv/errfuns.h"

// shim/procfs/core/
int __ape_shim_procfs_rewrite(const char *, char *, unsigned long);
void __ape_shim_procfs_relative(const char16_t *, unsigned long);
// shim/uncshare.c
int __ape_shim_unc_server_dir(const char *, size_t, char *, size_t);
int __ape_shim_unc_rel(const char *, char *, size_t);
int __ape_shim_unc_cwd(char *, size_t);

static inline bool IsSlash(char c) {
  return c == '/' || c == '\\';
}

static inline int IsAlpha(int c) {
  return ('A' <= c && c <= 'Z') || ('a' <= c && c <= 'z');
}

static inline int Lower(int c) {
  return ('A' <= c && c <= 'Z') ? c + ('a' - 'A') : c;
}

// UNC roots seen by this process, as "server/share" in the spelling
// first seen; matched ASCII case-insensitively, the way NT matches
// names. Writers take the lock; readers only see an entry once the count
// that publishes it is stored, so lookups are lock-free.
#define UNC_ROOT_MAX 16
#define UNC_ROOT_LEN 256
static char unc_roots[UNC_ROOT_MAX][UNC_ROOT_LEN];
static int unc_count;
static int unc_lock;

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
  if (p[share] == '.' &&
      (i - share == 1 || (i - share == 2 && p[share + 1] == '.')))
    return 0;
  return i < UNC_ROOT_LEN ? i : 0;
}

// whether any UNC root has been seen, so the relative-path hook can stay
// free for processes that never touch a share.
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
static void UncNote(const char *path) {
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

int __ape_shim_unc_collapsed(const char *path) {
  return IsWindows() && path && IsCollapsedUnc(path);
}

// "//srv/share/rest" whose rest climbs out of the share with ".." is
// resolved the way "/x/.." is "/" on unix: the climb lands in the server
// directory shim/uncshare.c provides, with whatever remained joined onto
// "//srv". False when there is no climb, or the server has no such
// directory (the fork then clamps the path at the share root).
static bool UncClimb(const char *path, char *out, size_t outsz) {
  if (!IsSlash(path[0]) || !IsSlash(path[1]))
    return false;
  size_t root = UncRootLen(path + 2);
  if (!root)
    return false;
  const char *rest = path + 2 + root;
  if (!strstr(rest, ".."))
    return false;
  char tail[PATH_MAX];
  size_t o = 0;
  bool climbed = false;
  const char *p = rest;
  while (*p) {
    while (IsSlash(*p))
      p++;
    const char *q = p;
    while (*q && !IsSlash(*q))
      q++;
    size_t len = (size_t)(q - p);
    if (len == 1 && p[0] == '.') {
    } else if (len == 2 && p[0] == '.' && p[1] == '.') {
      if (o) {
        while (o && tail[o - 1] != '/')
          o--;
        if (o)
          o--;
      } else {
        climbed = true;
      }
    } else if (len) {
      if (o + 1 + len + 1 > sizeof tail)
        return false;
      tail[o++] = '/';
      memcpy(tail + o, p, len);
      o += len;
    }
    p = q;
  }
  if (!climbed)
    return false;
  tail[o] = 0;
  size_t srv = 0;
  while (srv < root && !IsSlash(path[2 + srv]))
    srv++;
  char dir[600];
  if (!__ape_shim_unc_server_dir(path + 2, srv, dir, sizeof dir))
    return false;
  if (2 + srv + o + 1 > outsz)
    return false;
  out[0] = '/';
  out[1] = '/';
  memcpy(out + 2, path + 2, srv);
  memcpy(out + 2 + srv, tail, o + 1);
  return true;
}

// "/tmp" is the host's temp directory, the way cosmo 4.0.2 had it in the
// path layer (master moved that into its own tmpdir() and left "/tmp" a
// plain name on the cosmos drive, which doesn't exist). Programs written
// against Linux hardcode /tmp all the time. Fetched once, as a win32 path
// with a trailing backslash, which the fork's conversion accepts as is.
static const char *TempDir(size_t *len) {
  static char dir[PATH_MAX];
  static size_t dirlen;
  if (!__atomic_load_n(&dirlen, __ATOMIC_ACQUIRE)) {
    char16_t dir16[PATH_MAX];
    char tmp[PATH_MAX];
    uint32_t n = GetTempPath(PATH_MAX, dir16);
    if (!n || n >= PATH_MAX)
      return 0;
    size_t m = tprecode16to8(tmp, sizeof tmp, dir16).ax;
    if (!m || m >= sizeof tmp - 1)
      return 0;
    if (tmp[m - 1] != '\\' && tmp[m - 1] != '/')
      tmp[m++] = '\\', tmp[m] = 0;
    memcpy(dir, tmp, m + 1);
    __atomic_store_n(&dirlen, m, __ATOMIC_RELEASE);
  }
  *len = dirlen;
  return dir;
}

static bool IsTmpPath(const char *p) {
  return p[0] == '/' && p[1] == 't' && p[2] == 'm' && p[3] == 'p' &&
         (!p[4] || IsSlash(p[4]));
}

// The hook. Returns 1 with the rewritten path in out, 0 to leave the
// path alone, -1 with errno set.
int __ape_shim_ntpath_rewrite(const char *path, char *out, size_t outsz) {
  if (!path)
    return 0;
  const char *cur = path;

  char procbuf[600];
  if (__ape_shim_procfs_rewrite(cur, procbuf, sizeof procbuf))
    cur = procbuf;

  char tmpbuf[PATH_MAX];
  if (IsTmpPath(cur)) {
    size_t dirlen;
    const char *dir = TempDir(&dirlen);
    if (dir) {
      const char *rest = cur + 4;
      while (IsSlash(*rest))
        rest++;
      size_t restlen = strlen(rest);
      if (dirlen + restlen + 1 > sizeof tmpbuf)
        return enametoolong();
      memcpy(tmpbuf, dir, dirlen);
      memcpy(tmpbuf + dirlen, rest, restlen + 1);
      cur = tmpbuf;
    }
  }

  // last absolute win32 segment wins: an "X:" drive marker right after a
  // separator (or a "\\" UNC pair after a slash) starts an absolute path,
  // so everything before it is a spurious prefix. Scan for the LAST such
  // marker so repeated joins still resolve to the innermost intent.
  for (const char *s = cur; *s; s++) {
    if (s > cur && !IsSlash(s[-1]))
      continue;
    if (IsAlpha(s[0]) && s[1] == ':' && IsSlash(s[2])) {
      cur = s;
    } else if (s > cur && s[-1] == '/' && s[0] == '\\' && s[1] == '\\' &&
               !IsSlash(s[2])) {
      cur = s;
    }
  }

  // a relative path is NT's to resolve against the cwd, except where
  // NT's answer differs from unix
  char relbuf[PATH_MAX];
  if (cur[0] && !IsSlash(cur[0]) && !(IsAlpha(cur[0]) && cur[1] == ':') &&
      __ape_shim_unc_rel(cur, relbuf, sizeof relbuf))
    cur = relbuf;

  // a UNC root is learned from every path that spells one, and restored
  // to a path that lost its second slash on the way through unix path
  // code (see the header comment)
  char uncbuf[PATH_MAX];
  if (IsSlash(cur[0]) && IsSlash(cur[1])) {
    UncNote(cur);
  } else if (IsCollapsedUnc(cur)) {
    size_t len = strlen(cur);
    if (len + 2 > sizeof uncbuf)
      return enametoolong();
    uncbuf[0] = '/';
    memcpy(uncbuf + 1, cur, len + 1);
    cur = uncbuf;
  }

  char climbbuf[PATH_MAX];
  if (UncClimb(cur, climbbuf, sizeof climbbuf)) {
    cur = climbbuf;
    UncNote(cur);
  }

  // "//server" alone, which NT has no directory for, is diverted to the
  // share list shim/uncshare.c materializes
  char srvbuf[600];
  if (IsSlash(cur[0]) && IsSlash(cur[1]) && cur[2] && !IsSlash(cur[2]) &&
      cur[2] != '?' && cur[2] != '.') {
    size_t k = 2;
    while (cur[k] && !IsSlash(cur[k]))
      k++;
    if ((!cur[k] || !cur[k + 1]) &&
        __ape_shim_unc_server_dir(cur + 2, k - 2, srvbuf, sizeof srvbuf))
      cur = srvbuf;
  }

  if (cur == path)
    return 0;
  size_t len = strlen(cur);
  if (len + 1 > outsz)
    return enametoolong();
  memcpy(out, cur, len + 1);
  return 1;
}

// The fork calls this with the finished win32 path of every
// dirfd-relative conversion; a dirfd-relative access that lands inside
// the materialized /proc tree refreshes what it is about to touch.
void __ape_shim_ntpath_relative(const char16_t *file, size_t n) {
  __ape_shim_procfs_relative(file, n);
}

// The fork's getcwd() hands the converted cwd here on NT. A cwd inside
// the materialized "//server" tree is reported as that server; any real
// share seen is remembered. Returns the new length including the NUL,
// or 0 when the path stays as it is.
int __ape_shim_getcwd_hook(char *buf, size_t size) {
  int m = __ape_shim_unc_cwd(buf, size);
  UncNote(buf);
  return m;
}
