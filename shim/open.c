// The file half of the Linux-personality shim.
//
// The flags and AT_* names cosmo takes are Linux's now, and /proc lives
// in the fork (libc/procfs/), so nothing gets translated or synthesized
// here any more. What is left is the ".exe" retry for $PATH probes on
// NT: the identity probes a $PATH walk consists of (access/stat families)
// retry a bare-name ENOENT with the host suffixes, so `git` resolves
// where only git.exe exists; shim/suffix.c extends the same lie to exec.

#include <errno.h>
#include <limits.h>
#include <sys/stat.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()/IsWindows()
// cosmo keeps eaccess/euidaccess behind _COSMO_SOURCE, which unistd.h above
// was already read without. Same declarations as libc/calls/calls.h.
int eaccess(const char *, int);
int euidaccess(const char *, int);
#include <libc/dce.h>
#include <libc/sysv/consts/at.h>

// shim/suffix.c
int __ape_shim_exe_fallback(int dirfd, const char *path, char *buf);

int __ape_shim_fstatat(int dirfd, const char *path, struct stat *st, int flags) {
    int rc = fstatat(dirfd, path, st, flags);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(dirfd, path, buf))
            rc = fstatat(dirfd, buf, st, flags);
    }
    return rc;
}

// stat/lstat are here only for the suffix retry (std's fs::metadata is how
// fish sizes up a candidate).
int __ape_shim_stat(const char *path, struct stat *st) {
    int rc = stat(path, st);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = stat(buf, st);
    }
    return rc;
}

int __ape_shim_lstat(const char *path, struct stat *st) {
    int rc = lstat(path, st);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = lstat(buf, st);
    }
    return rc;
}

int __ape_shim_access(const char *path, int amode) {
    int rc = access(path, amode);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = access(buf, amode);
    }
    return rc;
}

// The effective-identity spellings of the same question.
int __ape_shim_eaccess(const char *path, int amode) {
    int rc = eaccess(path, amode);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = eaccess(buf, amode);
    }
    return rc;
}

int __ape_shim_euidaccess(const char *path, int amode) {
    int rc = euidaccess(path, amode);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf))
            rc = euidaccess(buf, amode);
    }
    return rc;
}

int __ape_shim_faccessat(int dirfd, const char *path, int amode, int flags) {
    int rc = faccessat(dirfd, path, amode, flags);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(dirfd, path, buf))
            rc = faccessat(dirfd, buf, amode, flags);
    }
    return rc;
}
