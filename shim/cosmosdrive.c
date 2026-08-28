// The drive a rooted unix path lives on, on NT.
//
// A path like /bin or / names no drive, and cosmo 4.0.2 handed it to NT
// as a root-relative "\bin", which resolves against whatever drive (or
// share) the cwd is on. master instead pins such paths to one drive,
// chosen by $COSMOSDRIVE, then $SYSTEMDRIVE, then C, so /bin/sh means the
// same file from every cwd. This is that rule, for mkntpath.c.
// cflags: -D_COSMO_SOURCE
#include "libc/intrin/getenv.h"
#include "libc/nt/process.h"
#include "libc/runtime/runtime.h"

static int IsAlpha(int c) {
  return ('A' <= c && c <= 'Z') || ('a' <= c && c <= 'z');
}

char __ape_shim_cosmos_drive(void) {
  char *p;
  char16_t b[3];
  if (environ && (p = __getenv(environ, "COSMOSDRIVE").s) && IsAlpha(p[0]) &&
      p[1] == ':' && !p[2])
    return p[0];
  if (GetEnvironmentVariable(u"COSMOSDRIVE", b, 3) == 2 && IsAlpha(b[0]) &&
      b[1] == ':')
    return b[0];
  if (GetEnvironmentVariable(u"SYSTEMDRIVE", b, 3) == 2 && IsAlpha(b[0]) &&
      b[1] == ':')
    return b[0];
  return 'C';
}
