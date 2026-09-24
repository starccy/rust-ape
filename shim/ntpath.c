// The one unix-path rewrite left on the Rust side of the fork's
// __mkntpathath(), which calls __ape_shim_ntpath_rewrite on every path
// it is handed, so open, stat, opendir and the rest all see the result
// without knowing about it.
//
// A win32-absolute segment embedded mid-path re-roots the path. The Rust
// world runs with unix Path semantics, which treat a win32-absolute path
// (C:\x, \\server\share\x) as relative and join it onto the cwd. cosmo
// already accepts "c:\foo" at position 0; this extends that to "last
// absolute segment wins", the same rule Path::join applies to segments
// it recognizes as absolute. The marker (drive letter, colon, separator,
// or a \\server\share pair) cannot occur inside real NT components:
// colons only appear in ADS names, which are terminal and never followed
// by a separator.
//
// "/tmp" as the host's temp directory, the "//server" share list, and
// the share roots a process has seen (so a "//server/share" that unix
// path code collapsed to one slash is restored) are all in the fork now
// (libc/calls/mkntpath.c and uncserver.c). The fork runs after this hook.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include "libc/limits.h"
#include "libc/sysv/errfuns.h"

static inline bool IsSlash(char c) {
  return c == '/' || c == '\\';
}

static inline int IsAlpha(int c) {
  return ('A' <= c && c <= 'Z') || ('a' <= c && c <= 'z');
}

// The hook. Returns 1 with the rewritten path in out, 0 to leave the
// path alone, -1 with errno set.
int __ape_shim_ntpath_rewrite(const char *path, char *out, size_t outsz) {
  if (!path)
    return 0;
  const char *cur = path;

  // last absolute win32 segment wins: an "X:" drive marker right after a
  // separator (or a "\\" UNC pair after a slash) starts an absolute path,
  // so everything before it is a spurious prefix. Scan for the LAST such
  // marker so repeated joins still resolve to the innermost intent.
  for (const char *s = cur; *s; s++) {
    if (s > cur && !IsSlash(s[-1]))
      continue;
    if (IsAlpha(s[0]) && s[1] == ':' && (IsSlash(s[2]) || !s[2])) {
      cur = s;
    } else if (s > cur && s[-1] == '/' && s[0] == '\\' && s[1] == '\\' &&
               !IsSlash(s[2])) {
      cur = s;
    }
  }

  if (cur == path)
    return 0;
  size_t len = strlen(cur);
  // a bare "X:" at the end names the drive's root here, not its current
  // directory, which is what NT reads a slashless "X:" as
  bool bare = len == 2;
  if (len + bare + 1 > outsz)
    return enametoolong();
  memcpy(out, cur, len);
  if (bare)
    out[len++] = '/';
  out[len] = 0;
  return 1;
}
