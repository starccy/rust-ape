// The open/fcntl half of the Linux-personality shim.
//
// The same problem as errno.c, in the opposite direction. The Rust world
// passes file flags it baked in at compile time from musl's headers, and
// cosmo wants whatever the host uses. The patched libc crate points open/openat/fcntl/pipe2 and
// the *at family at the __ape_shim_* entry points below, which translate the
// Linux coding into cosmo's runtime constants and forward.
//
// All Linux values come from tables.h, generated out of the vendored libc
// crate by `cargo xtask gen-shim` and cross-checked at build time.

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>

#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <stdint.h>
#include <sys/stat.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()/IsWindows()
// cosmo keeps eaccess/euidaccess behind _COSMO_SOURCE, which unistd.h above
// was already read without. Same declarations as libc/calls/calls.h.
int eaccess(const char *, int);
int euidaccess(const char *, int);
#include <libc/dce.h>
#include <libc/sysv/consts/at.h>

// Newer cosmocc (recognizable by its Linux-coded _O_TMPFILE macro) dropped
// the Linux-only AT_EMPTY_PATH outright; older versions declare it extern
// with no macro alias, so _O_TMPFILE serves as the presence test instead.
// Zero means "unsupported here", which at_bit turns into EOPNOTSUPP.
#ifdef _O_TMPFILE
static const int shim_no_at_empty_path = 0;
#define SHIM_AT_EMPTY_PATH (&shim_no_at_empty_path)
#else
#define SHIM_AT_EMPTY_PATH (&AT_EMPTY_PATH)
#endif
#include <libc/sysv/consts/utime.h>

#include "tables.h"

// shim/epoll.c. An emulated epoll fd is a pipe end, so a duplicate of it has
// to be recorded as naming the same set. No-op on Linux and for every fd that
// isn't one. note_pipe/note_new_fd maintain the pipe-pair table behind the
// edge-triggered emulation: a fresh pipe is recorded, and any newly created
// fd number retires stale records from a previous life of that number.
void __ape_shim_epoll_note_dup(int oldfd, int newfd);
void __ape_shim_epoll_note_pipe(int rfd, int wfd);
void __ape_shim_epoll_note_new_fd(int fd);

#define LIN_O_ACCMODE 0x03 // O_RDONLY/O_WRONLY/O_RDWR, same on every platform

struct oflag {
    int linux_bit;
    const unsigned *host; // cosmo's runtime constant
    bool droppable;       // hint: drop when unsupported instead of failing
};

// cosmo may publish an unsupported flag as 0 or ~0; treat both as unsupported.
static bool unsupported(unsigned host_bit) {
    return host_bit == 0 || host_bit == (unsigned)-1;
}

#define X(name, lin, drop) { lin, &name, drop },
static const struct oflag kOflags[] = { SHIM_OFLAG_TABLE(X) };
#undef X
#define NOFLAGS (sizeof(kOflags) / sizeof(kOflags[0]))

// Linux flags -> host flags. Returns 0, or -1 with errno set.
static int oflags_to_host(int lin, int *out) {
    int host = lin & LIN_O_ACCMODE;
    lin &= ~LIN_O_ACCMODE;
    lin &= ~SHIM_LIN_O_LARGEFILE; // meaningless with 64-bit off_t
    if ((lin & SHIM_LIN_O_SYNC) == SHIM_LIN_O_SYNC) {
        if (!O_SYNC) return errno = EOPNOTSUPP, -1;
        host |= O_SYNC;
        lin &= ~SHIM_LIN_O_SYNC;
    }
    if ((lin & SHIM_LIN_O_TMPFILE) == SHIM_LIN_O_TMPFILE) {
        // Only the Linux kernel takes O_TMPFILE's raw bits and only cosmo's
        // NT layer emulates them. Elsewhere the host answers EINVAL — which
        // tempfile's fallback list doesn't recognize — and the "0 when
        // unsupported" assumption doesn't hold either (XNU publishes ~0, so
        // or-ing it in turns the whole flags word into -1). EOPNOTSUPP is
        // what sends callers down their named-file path.
        if (!IsLinux() && !IsWindows()) return errno = EOPNOTSUPP, -1;
#ifdef _O_TMPFILE
        // newer cosmocc: a Linux-coded macro with kernel semantics, so the
        // directory bit must be added too, exactly like musl's own O_TMPFILE
        // (cosmo's tmpfd.c passes _O_TMPFILE | O_DIRECTORY itself).
        host |= _O_TMPFILE | O_DIRECTORY;
#else
        // older cosmocc: a standalone runtime constant; cosmo adds the
        // directory semantics internally.
        host |= O_TMPFILE;
#endif
        lin &= ~SHIM_LIN_O_TMPFILE;
    }
    for (size_t i = 0; i < NOFLAGS; i++) {
        if (!(lin & kOflags[i].linux_bit)) continue;
        lin &= ~kOflags[i].linux_bit;
        if (!unsupported(*kOflags[i].host)) {
            host |= *kOflags[i].host;
        } else if (!kOflags[i].droppable) {
            return errno = EOPNOTSUPP, -1;
        }
    }
    if (lin) return errno = EINVAL, -1; // bits we don't know about
    *out = host;
    return 0;
}

// Host flags -> Linux flags, for F_GETFL. Best effort; unknown host bits are
// dropped rather than invented.
static int oflags_to_linux(int host) {
    int lin = host & LIN_O_ACCMODE;
    if (O_SYNC && (host & O_SYNC) == (int)O_SYNC) {
        lin |= SHIM_LIN_O_SYNC;
        host &= ~O_SYNC;
    }
    for (size_t i = 0; i < NOFLAGS; i++) {
        unsigned h = *kOflags[i].host;
        if (!unsupported(h) && (host & h) == (int)h) lin |= kOflags[i].linux_bit;
    }
    return lin;
}

static int at_fdcwd(int dirfd) {
    return dirfd == SHIM_LIN_AT_FDCWD ? AT_FDCWD : dirfd;
}

// struct flock: the layouts agree byte for byte (layouts.c pins them; the
// cosmo-only l_sysid sits in musl's tail padding), but l_type carries
// F_RDLCK/F_WRLCK/F_UNLCK, which are runtime constants on the cosmo side.
static int ltype_to_host(int16_t lin, int16_t *out) {
    if (lin == SHIM_LIN_F_RDLCK) return *out = F_RDLCK, 0;
    if (lin == SHIM_LIN_F_WRLCK) return *out = F_WRLCK, 0;
    if (lin == SHIM_LIN_F_UNLCK) return *out = F_UNLCK, 0;
    return errno = EINVAL, -1;
}

static int16_t ltype_to_linux(int16_t host) {
    if (host == F_RDLCK) return SHIM_LIN_F_RDLCK;
    if (host == F_WRLCK) return SHIM_LIN_F_WRLCK;
    if (host == F_UNLCK) return SHIM_LIN_F_UNLCK;
    return host; // unknown: pass through, keeps it diagnosable
}

// F_GETLK writes the struct back (l_type plus the blocker's coordinates);
// F_SETLK/F_SETLKW leave it untouched per POSIX.
static int lock_cmd(int fd, int cmd, void *parg, bool writes_back) {
    struct flock fl = *(struct flock *)parg;
    if (ltype_to_host(fl.l_type, &fl.l_type) < 0) return -1;
    int rc = fcntl(fd, cmd, &fl);
    if (rc != -1 && writes_back) {
        fl.l_type = ltype_to_linux(fl.l_type);
        *(struct flock *)parg = fl;
    }
    return rc;
}

// One entry per caller-visible flag; each *at shim states which it accepts.
static int at_bit(int *lin, int bit, const int *host, int *out) {
    if (!(*lin & bit)) return 0;
    *lin &= ~bit;
    if (*host == 0) return errno = EOPNOTSUPP, -1;
    *out |= *host;
    return 0;
}

int __ape_shim_open(const char *path, int lin, ...) {
    int host;
    if (oflags_to_host(lin, &host) < 0) return -1;
    unsigned mode = 0;
    if ((lin & SHIM_LIN_O_CREAT) || (lin & SHIM_LIN_O_TMPFILE) == SHIM_LIN_O_TMPFILE) {
        va_list ap;
        va_start(ap, lin);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
    int fd = open(path, host, mode);
    if (fd >= 0) __ape_shim_epoll_note_new_fd(fd);
    return fd;
}

int __ape_shim_openat(int dirfd, const char *path, int lin, ...) {
    int host;
    if (oflags_to_host(lin, &host) < 0) return -1;
    unsigned mode = 0;
    if ((lin & SHIM_LIN_O_CREAT) || (lin & SHIM_LIN_O_TMPFILE) == SHIM_LIN_O_TMPFILE) {
        va_list ap;
        va_start(ap, lin);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
    int fd = openat(at_fdcwd(dirfd), path, host, mode);
    if (fd >= 0) __ape_shim_epoll_note_new_fd(fd);
    return fd;
}

int __ape_shim_pipe2(int fds[2], int lin) {
    int host;
    if (oflags_to_host(lin, &host) < 0) return -1;
    int rc = pipe2(fds, host);
    if (rc == 0) __ape_shim_epoll_note_pipe(fds[0], fds[1]);
    return rc;
}

// fcntl: commands 0..4 (F_DUPFD..F_SETFL) are portable by definition and pass
// through; the rest of musl's numbering gets mapped onto cosmo's runtime
// values. Lock commands translate l_type through lock_cmd above.
int __ape_shim_fcntl(int fd, int cmd, ...) {
    va_list ap;
    va_start(ap, cmd);
    void *parg = va_arg(ap, void *); // widest read; reinterpreted per cmd
    va_end(ap);
    int iarg = (int)(long)parg;

    switch (cmd) {
        case 0: { // F_DUPFD
            int nfd = fcntl(fd, cmd, iarg);
            if (nfd >= 0) {
                __ape_shim_epoll_note_dup(fd, nfd);
                __ape_shim_epoll_note_new_fd(nfd);
            }
            return nfd;
        }
        case 2: // F_SETFD (FD_CLOEXEC is 1 everywhere)
            return fcntl(fd, cmd, iarg);
        case 1: // F_GETFD
            return fcntl(fd, cmd);
        case 3: { // F_GETFL: translate the returned flags back
            int host = fcntl(fd, cmd);
            return host < 0 ? host : oflags_to_linux(host);
        }
        case 4: { // F_SETFL: translate the flag argument
            int host;
            if (oflags_to_host(iarg & ~LIN_O_ACCMODE, &host) < 0) return -1;
            return fcntl(fd, cmd, host);
        }
        case SHIM_LIN_F_GETLK:
            return F_GETLK > 0 ? lock_cmd(fd, F_GETLK, parg, true) : (errno = EINVAL, -1);
        case SHIM_LIN_F_SETLK:
            return F_SETLK > 0 ? lock_cmd(fd, F_SETLK, parg, false) : (errno = EINVAL, -1);
        case SHIM_LIN_F_SETLKW:
            return F_SETLKW > 0 ? lock_cmd(fd, F_SETLKW, parg, false) : (errno = EINVAL, -1);
#ifdef F_SETOWN // newer cosmocc dropped these outright; default arm EINVALs
        case SHIM_LIN_F_SETOWN:
            return F_SETOWN > 0 ? fcntl(fd, F_SETOWN, iarg) : (errno = EINVAL, -1);
        case SHIM_LIN_F_GETOWN:
            return F_GETOWN > 0 ? fcntl(fd, F_GETOWN) : (errno = EINVAL, -1);
#endif
        case SHIM_LIN_F_DUPFD_CLOEXEC: {
            if (F_DUPFD_CLOEXEC <= 0) return errno = EINVAL, -1;
            int nfd = fcntl(fd, F_DUPFD_CLOEXEC, iarg);
            if (nfd >= 0) {
                __ape_shim_epoll_note_dup(fd, nfd);
                __ape_shim_epoll_note_new_fd(nfd);
            }
            return nfd;
        }
        default:
            return errno = EINVAL, -1;
    }
}

int __ape_shim_unlinkat(int dirfd, const char *path, int lin) {
    int host = 0;
    if (at_bit(&lin, SHIM_LIN_AT_REMOVEDIR, &AT_REMOVEDIR, &host) < 0) return -1;
    if (lin) return errno = EINVAL, -1;
    return unlinkat(at_fdcwd(dirfd), path, host);
}

// shim/suffix.c -- the NT executable-suffix retry (".exe magic"). The
// identity probes a $PATH walk consists of (access/stat families) retry a
// bare-name ENOENT with the host suffixes, so `git` resolves where only
// git.exe exists; shim/suffix.c extends the same lie to exec.
int __ape_shim_exe_fallback(int dirfd, const char *path, char *buf);

// POSIX says lstat on a path ending in ".", ".." or a slash always
// follows a symlink there; cosmo's NT conversion can report the symlink
// itself instead, so those cases are routed to plain stat.
static int lstat_must_follow(const char *p) {
    size_t n = strlen(p);
    if (!n) return 0;
    if (p[n - 1] == '/') return 1;
    const char *b = strrchr(p, '/');
    b = b ? b + 1 : p;
    return b[0] == '.' && (b[1] == 0 || (b[1] == '.' && b[2] == 0));
}


int __ape_shim_fstatat(int dirfd, const char *path, struct stat *st, int lin) {
    int host = 0;
    // Cosmo rejects an empty path before ever looking at AT_EMPTY_PATH, so
    // the call fails with EINVAL on every host, Linux included. Programs
    // that read /proc/<pid>/fd depend on it to ask each descriptor what it
    // refers to.
    if (path && !path[0] && (lin & SHIM_LIN_AT_EMPTY_PATH)) {
        if (dirfd != SHIM_LIN_AT_FDCWD) return fstat(dirfd, st);
        return stat(".", st);
    }
    if (at_bit(&lin, SHIM_LIN_AT_SYMLINK_NOFOLLOW, &AT_SYMLINK_NOFOLLOW, &host) < 0 ||
        at_bit(&lin, SHIM_LIN_AT_EMPTY_PATH, SHIM_AT_EMPTY_PATH, &host) < 0)
        return -1;
    lin &= ~SHIM_LIN_AT_NO_AUTOMOUNT; // Linux-only; a no-op everywhere else
    if (lin) return errno = EINVAL, -1;
    if ((host & AT_SYMLINK_NOFOLLOW) && lstat_must_follow(path))
        host &= ~AT_SYMLINK_NOFOLLOW;
    int rc = fstatat(at_fdcwd(dirfd), path, st, host);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(at_fdcwd(dirfd), path, buf))
            rc = fstatat(at_fdcwd(dirfd), buf, st, host);
    }
    return rc;
}

// stat/lstat carry no constants to translate; they are here only for the
// suffix retry (std's fs::metadata is how fish sizes up a candidate).
int __ape_shim_stat(const char *path, struct stat *st) {
    int rc = stat(path, st);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = stat(buf, st);
    }
    return rc;
}

int __ape_shim_lstat(const char *path, struct stat *st) {
    if (lstat_must_follow(path)) return __ape_shim_stat(path, st);
    int rc = lstat(path, st);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = lstat(buf, st);
    }
    return rc;
}

int __ape_shim_linkat(int olddirfd, const char *oldpath, int newdirfd,
                      const char *newpath, int lin) {
    int host = 0;
    if (at_bit(&lin, SHIM_LIN_AT_SYMLINK_FOLLOW, &AT_SYMLINK_FOLLOW, &host) < 0 ||
        at_bit(&lin, SHIM_LIN_AT_EMPTY_PATH, SHIM_AT_EMPTY_PATH, &host) < 0)
        return -1;
    if (lin) return errno = EINVAL, -1;
    return linkat(at_fdcwd(olddirfd), oldpath, at_fdcwd(newdirfd), newpath, host);
}

// The amode of access/faccessat. POSIX pins R_OK/W_OK/X_OK to 4/2/1 and every
// Unix keeps them there, but on Windows cosmo publishes NT's access mask
// (GENERIC_READ & co) under those names, so a Linux-coded amode arrives as
// garbage bits and the call fails with EINVAL. F_OK is 0 on every host,
// which an empty host mask reproduces by itself.
static int amode_to_host(int lin, int *out) {
    int host = 0;
    if (lin & ~(SHIM_LIN_R_OK | SHIM_LIN_W_OK | SHIM_LIN_X_OK))
        return errno = EINVAL, -1;
    if (lin & SHIM_LIN_R_OK) host |= R_OK;
    if (lin & SHIM_LIN_W_OK) host |= W_OK;
    if (lin & SHIM_LIN_X_OK) host |= X_OK;
    *out = host;
    return 0;
}

int __ape_shim_access(const char *path, int amode) {
    int host;
    if (amode_to_host(amode, &host) < 0) return -1;
    int rc = access(path, host);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = access(buf, host);
    }
    return rc;
}

// The effective-identity spellings of the same question, same amode.
int __ape_shim_eaccess(const char *path, int amode) {
    int host;
    if (amode_to_host(amode, &host) < 0) return -1;
    int rc = eaccess(path, host);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = eaccess(buf, host);
    }
    return rc;
}

int __ape_shim_euidaccess(const char *path, int amode) {
    int host;
    if (amode_to_host(amode, &host) < 0) return -1;
    int rc = euidaccess(path, host);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf))
            rc = euidaccess(buf, host);
    }
    return rc;
}

int __ape_shim_faccessat(int dirfd, const char *path, int amode, int lin) {
    int host = 0, hmode;
    // AT_EACCESS asks for the effective rather than the real identity. Where
    // cosmo has no value for it (Windows, which has one identity per process)
    // the two answers coincide, so drop the bit instead of failing the call.
    if (AT_EACCESS) {
        if (at_bit(&lin, SHIM_LIN_AT_EACCESS, &AT_EACCESS, &host) < 0) return -1;
    } else {
        lin &= ~SHIM_LIN_AT_EACCESS;
    }
    if (at_bit(&lin, SHIM_LIN_AT_SYMLINK_FOLLOW, &AT_SYMLINK_FOLLOW, &host) < 0)
        return -1;
    if (lin) return errno = EINVAL, -1;
    if (amode_to_host(amode, &hmode) < 0) return -1;
    int rc = faccessat(at_fdcwd(dirfd), path, hmode, host);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(at_fdcwd(dirfd), path, buf))
            rc = faccessat(at_fdcwd(dirfd), buf, hmode, host);
    }
    return rc;
}

int __ape_shim_utimensat(int dirfd, const char *path,
                         const struct timespec times[2], int lin) {
    int host = 0;
    if (at_bit(&lin, SHIM_LIN_AT_SYMLINK_NOFOLLOW, &AT_SYMLINK_NOFOLLOW, &host) < 0)
        return -1;
    if (lin) return errno = EINVAL, -1;
    // tv_nsec can carry UTIME_NOW/UTIME_OMIT, which are platform-coded too.
    struct timespec copy[2];
    if (times) {
        for (int i = 0; i < 2; i++) {
            copy[i] = times[i];
            if (copy[i].tv_nsec == SHIM_LIN_UTIME_NOW) copy[i].tv_nsec = UTIME_NOW;
            if (copy[i].tv_nsec == SHIM_LIN_UTIME_OMIT) copy[i].tv_nsec = UTIME_OMIT;
        }
    }
    return utimensat(at_fdcwd(dirfd), path, times ? copy : NULL, host);
}

// mkfifo(): cosmo ships mknod() but not the POSIX wrapper over it. Nothing
// needs translating -- both sides spell S_IFIFO 0010000 and the permission
// bits agree -- so this is the same one-liner musl uses. Hosts without FIFOs
// (Windows) fail inside cosmo's mknod, which is where that belongs.
int __ape_shim_mkfifo(const char *path, unsigned mode) {
    return mknod(path, S_IFIFO | mode, 0);
}
