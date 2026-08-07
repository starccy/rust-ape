// Extended attributes for the Linux-personality shim.
//
// cosmo ships the __NR_*xattr numbers but no wrappers for them, and its
// syscall() is a stub that answers ENOSYS to nearly everything, so the whole
// family lands on the Rust side as undefined symbols (eza's xattr module is
// gated on target_os = "linux", which is always true for us, so it cannot be
// configured away).
//
// xattr is Linux-only as an API: macOS spells getxattr with two extra
// arguments, the BSDs use extattr_get_file & co, and Windows has nothing
// comparable. So Linux gets the real call, issued as a raw syscall, and
// every other host answers ENOTSUP -- which is exactly what a filesystem
// without xattr support returns, and what callers already treat as "this
// file has no attributes".
//
// The raw-syscall mechanics and the errno reasoning behind them live in
// syscall.h.

#include <errno.h>
#include <stddef.h>
#include <sys/types.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

static long call5(int nr, long a, long b, long c, long d, long e) {
    if (!IsLinux()) return errno = ENOTSUP, -1;
    return __ape_syscall_ret(__ape_raw_syscall(nr, a, b, c, d, e));
}

// ---- get ------------------------------------------------------------------

ssize_t __ape_shim_getxattr(const char *path, const char *name, void *value,
                            size_t size) {
    return call5(__NR_getxattr, (long)path, (long)name, (long)value, (long)size, 0);
}

ssize_t __ape_shim_lgetxattr(const char *path, const char *name, void *value,
                             size_t size) {
    return call5(__NR_lgetxattr, (long)path, (long)name, (long)value, (long)size, 0);
}

ssize_t __ape_shim_fgetxattr(int fd, const char *name, void *value, size_t size) {
    return call5(__NR_fgetxattr, fd, (long)name, (long)value, (long)size, 0);
}

// ---- set ------------------------------------------------------------------
//
// The flags argument (XATTR_CREATE / XATTR_REPLACE) is 1/2 on Linux and has
// no host counterpart to translate to; it goes through as given.

int __ape_shim_setxattr(const char *path, const char *name, const void *value,
                        size_t size, int flags) {
    return (int)call5(__NR_setxattr, (long)path, (long)name, (long)value,
                      (long)size, flags);
}

int __ape_shim_lsetxattr(const char *path, const char *name, const void *value,
                         size_t size, int flags) {
    return (int)call5(__NR_lsetxattr, (long)path, (long)name, (long)value,
                      (long)size, flags);
}

int __ape_shim_fsetxattr(int fd, const char *name, const void *value,
                         size_t size, int flags) {
    return (int)call5(__NR_fsetxattr, fd, (long)name, (long)value, (long)size,
                      flags);
}

// ---- list -----------------------------------------------------------------

ssize_t __ape_shim_listxattr(const char *path, char *list, size_t size) {
    return call5(__NR_listxattr, (long)path, (long)list, (long)size, 0, 0);
}

ssize_t __ape_shim_llistxattr(const char *path, char *list, size_t size) {
    return call5(__NR_llistxattr, (long)path, (long)list, (long)size, 0, 0);
}

ssize_t __ape_shim_flistxattr(int fd, char *list, size_t size) {
    return call5(__NR_flistxattr, fd, (long)list, (long)size, 0, 0);
}

// ---- remove ---------------------------------------------------------------

int __ape_shim_removexattr(const char *path, const char *name) {
    return (int)call5(__NR_removexattr, (long)path, (long)name, 0, 0, 0);
}

int __ape_shim_lremovexattr(const char *path, const char *name) {
    return (int)call5(__NR_lremovexattr, (long)path, (long)name, 0, 0, 0);
}

int __ape_shim_fremovexattr(int fd, const char *name) {
    return (int)call5(__NR_fremovexattr, fd, (long)name, 0, 0, 0);
}
