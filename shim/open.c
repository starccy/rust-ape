// The file half of the Linux-personality shim.
//
// The flags and AT_* names cosmo takes are Linux's now, so nothing gets
// translated here any more. What is left is behaviour: the /proc tree
// shim/procfs/ synthesizes, the bookkeeping shim/epoll.c needs when
// descriptors are created and duplicated, the ".exe" retry for $PATH
// probes on NT, a POSIX corner cosmo's NT lstat gets wrong, and mkfifo,
// which cosmo doesn't ship.

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>

#include <stdbool.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()/IsWindows()
// cosmo keeps eaccess/euidaccess behind _COSMO_SOURCE, which unistd.h above
// was already read without. Same declarations as libc/calls/calls.h.
int eaccess(const char *, int);
int euidaccess(const char *, int);
#include <libc/dce.h>
#include <libc/sysv/consts/at.h>
#include <libc/sysv/consts/o.h>

// shim/epoll.c. An emulated epoll fd is a pipe end, so a duplicate of it has
// to be recorded as naming the same set. No-op on Linux and for every fd that
// isn't one. note_pipe/note_new_fd maintain the pipe-pair table behind the
// edge-triggered emulation: a fresh pipe is recorded, and any newly created
// fd number retires stale records from a previous life of that number.
void __ape_shim_epoll_note_dup(int oldfd, int newfd);
void __ape_shim_epoll_note_pipe(int rfd, int wfd);
void __ape_shim_epoll_note_new_fd(int fd);

// shim/procfs/core/: a read of a /proc content file on NT is answered with
// a generated-on-open descriptor; -2 means "not mine", including everywhere
// but Windows.
int __ape_shim_procfs_open(const char *path, int flags);
void __ape_shim_procfs_track(int fd, const char *vpath);
bool __ape_shim_procfs_join(int dirfd, const char *rel, char *out,
                            unsigned long outsz);
long __ape_shim_procfs_memfd_lseek(int fd, long off, int whence);
int __ape_shim_procfs_memfd_fcntl(int fd, int cmd, int arg);
int __ape_shim_procfs_memfd_fstat(int fd, struct stat *st);
void __ape_shim_procfs_list(const char *vpath);
void __ape_shim_procfs_list_fd(int fd);
unsigned __ape_shim_procfs_link_mode(const char *vpath);
void __ape_shim_procfs_fix_dirent(int fd, struct dirent *e);
long __ape_shim_procfs_readlinkat(int dirfd, const char *path, char *buf,
                                  unsigned long bufsiz);
int __ape_shim_procfs_vstat(int dirfd, const char *path, struct stat *st,
                            int nofollow);

static unsigned open_mode(int flags, va_list ap) {
    if ((flags & O_CREAT) || (flags & _O_TMPFILE) == _O_TMPFILE)
        return va_arg(ap, unsigned);
    return 0;
}

int __ape_shim_open(const char *path, int flags, ...) {
    va_list ap;
    va_start(ap, flags);
    unsigned mode = open_mode(flags, ap);
    va_end(ap);
    int fd = __ape_shim_procfs_open(path, flags);
    if (fd == -2) {
        fd = open(path, flags, mode);
        if (fd >= 0) __ape_shim_procfs_track(fd, path);
    }
    if (fd >= 0) __ape_shim_epoll_note_new_fd(fd);
    return fd;
}

int __ape_shim_openat(int dirfd, const char *path, int flags, ...) {
    va_list ap;
    va_start(ap, flags);
    unsigned mode = open_mode(flags, ap);
    va_end(ap);
    // A relative name through a directory descriptor of the /proc tree
    // is the same virtual path spelled differently (shim/procfs/core/).
    char joined[300];
    const char *vpath = 0;
    if (path && path[0] == '/') {
        vpath = path;
    } else if (dirfd != AT_FDCWD &&
               __ape_shim_procfs_join(dirfd, path, joined, sizeof joined)) {
        vpath = joined;
    }
    int fd = vpath ? __ape_shim_procfs_open(vpath, flags) : -2;
    if (fd == -2) {
        fd = openat(dirfd, path, flags, mode);
        if (fd >= 0 && vpath) __ape_shim_procfs_track(fd, vpath);
    }
    if (fd >= 0) __ape_shim_epoll_note_new_fd(fd);
    return fd;
}

// A /proc content descriptor lives in memory and seeks there; its rewind
// regenerates the text (shim/procfs/core/).
off_t __ape_shim_lseek(int fd, off_t off, int whence) {
    long r = __ape_shim_procfs_memfd_lseek(fd, off, whence);
    if (r != -2) return r;
    return lseek(fd, off, whence);
}

// A /proc content descriptor has no handle behind it; its stat is answered
// from the text (shim/procfs/core/).
int __ape_shim_fstat(int fd, struct stat *st) {
    int r = __ape_shim_procfs_memfd_fstat(fd, st);
    if (r != -2) return r;
    return fstat(fd, st);
}

DIR *__ape_shim_opendir(const char *path) {
    bool proc = path && path[0] == '/' && !strncmp(path, "/proc", 5);
    DIR *d = opendir(path);
    if (d && proc) __ape_shim_procfs_track(dirfd(d), path);
    return d;
}

struct dirent *__ape_shim_readdir(DIR *d) {
    struct dirent *e = readdir(d);
    if (e) __ape_shim_procfs_fix_dirent(dirfd(d), e);
    return e;
}

DIR *__ape_shim_fdopendir(int fd) {
    __ape_shim_procfs_list_fd(fd);
    return fdopendir(fd);
}

int __ape_shim_pipe2(int fds[2], int flags) {
    int rc = pipe2(fds, flags);
    if (rc == 0) __ape_shim_epoll_note_pipe(fds[0], fds[1]);
    return rc;
}

// fcntl: what is left here is the /proc descriptors and the epoll
// bookkeeping for dups.
int __ape_shim_fcntl(int fd, int cmd, ...) {
    va_list ap;
    va_start(ap, cmd);
    void *parg = va_arg(ap, void *); // widest read; reinterpreted per cmd
    va_end(ap);
    int iarg = (int)(long)parg;

    // a /proc content descriptor has no handle; its flags live in the shim
    int mr = __ape_shim_procfs_memfd_fcntl(fd, cmd, iarg);
    if (mr != -2) return mr;

    switch (cmd) {
        case F_DUPFD:
        case F_DUPFD_CLOEXEC: {
            int nfd = fcntl(fd, cmd, iarg);
            if (nfd >= 0) {
                __ape_shim_epoll_note_dup(fd, nfd);
                __ape_shim_epoll_note_new_fd(nfd);
            }
            return nfd;
        }
        default:
            return fcntl(fd, cmd, parg);
    }
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


// A successful no-follow stat of one of the /proc tree's links: the disk
// placeholder is a plain file, the answer must be a symlink (size 0, as the
// kernel reports them). `path` is absolute or relative to a tracked dirfd.
static void procfs_link_stat(int dirfd, const char *path, struct stat *st) {
    char joined[PATH_MAX];
    const char *vpath = 0;
    if (path[0] == '/') {
        vpath = path;
    } else if (__ape_shim_procfs_join(dirfd, path, joined, sizeof joined)) {
        vpath = joined;
    }
    if (!vpath || strncmp(vpath, "/proc", 5)) return;
    unsigned mode = __ape_shim_procfs_link_mode(vpath);
    if (!mode) return;
    st->st_mode = S_IFLNK | mode;
    st->st_size = 0;
    st->st_nlink = 1;
}

// The following stat of one of those links: what the target is. The host
// followed nothing (the placeholder is a plain file), so the answer is
// built here: exe/cwd/root and self stat their target path; a descriptor
// link is the descriptor (ours: fstat it; another process's: a socket).
static void procfs_follow_stat(int dirfd, const char *path, struct stat *st) {
    char joined[PATH_MAX];
    const char *vpath = 0;
    if (path[0] == '/') {
        vpath = path;
    } else if (__ape_shim_procfs_join(dirfd, path, joined, sizeof joined)) {
        vpath = joined;
    }
    if (!vpath || strncmp(vpath, "/proc", 5)) return;
    if (!__ape_shim_procfs_link_mode(vpath)) return;
    char target[PATH_MAX];
    long n = __ape_shim_procfs_readlinkat(AT_FDCWD, vpath, target,
                                          sizeof target - 1);
    if (n <= 0) return;
    target[n] = 0;
    struct stat t;
    if (target[0] == '/') {
        if (!stat(target, &t)) *st = t;
    } else if (target[0] >= '0' && target[0] <= '9' && !strchr(vpath + 6, '/')) {
        // /proc/self -> "<pid>"
        char dir[64];
        snprintf(dir, sizeof dir, "/proc/%s", target);
        if (!stat(dir, &t)) *st = t;
    } else {
        // fd/<n> -> "pipe:[..]" / "socket:[..]" / "anon_inode:..."
        const char *fdpart = strstr(vpath, "/fd/");
        int fd = fdpart ? atoi(fdpart + 4) : -1;
        if (fd >= 0 && !strncmp(vpath, "/proc/self/", 11) && !fstat(fd, &t)) {
            *st = t;
        } else {
            st->st_mode = S_IFSOCK | 0777;
            st->st_size = 0;
        }
    }
}


int __ape_shim_fstatat(int dirfd, const char *path, struct stat *st, int flags) {
    if ((flags & AT_SYMLINK_NOFOLLOW) && lstat_must_follow(path))
        flags &= ~AT_SYMLINK_NOFOLLOW;
    // Apple Silicon: the emulated tree has nothing on disk to stat
    int vr = __ape_shim_procfs_vstat(dirfd, path, st,
                                     !!(flags & AT_SYMLINK_NOFOLLOW));
    if (vr != -2) return vr;
    int rc = fstatat(dirfd, path, st, flags);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(dirfd, path, buf))
            rc = fstatat(dirfd, buf, st, flags);
    }
    if (rc == 0 && path) {
        if (flags & AT_SYMLINK_NOFOLLOW)
            procfs_link_stat(dirfd, path, st);
        else
            procfs_follow_stat(dirfd, path, st);
    }
    return rc;
}

// stat/lstat are here only for the suffix retry (std's fs::metadata is how
// fish sizes up a candidate) and the /proc links.
int __ape_shim_stat(const char *path, struct stat *st) {
    int vr = __ape_shim_procfs_vstat(AT_FDCWD, path, st, 0);
    if (vr != -2) return vr;
    int rc = stat(path, st);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = stat(buf, st);
    }
    if (rc == 0 && path) procfs_follow_stat(AT_FDCWD, path, st);
    return rc;
}

int __ape_shim_lstat(const char *path, struct stat *st) {
    if (lstat_must_follow(path)) return __ape_shim_stat(path, st);
    int vr = __ape_shim_procfs_vstat(AT_FDCWD, path, st, 1);
    if (vr != -2) return vr;
    int rc = lstat(path, st);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = lstat(buf, st);
    }
    if (rc == 0 && path) procfs_link_stat(AT_FDCWD, path, st);
    return rc;
}

// The virtual tree's answer to the access family: everything readable,
// nothing writable, absent things absent. -2 is not ours.
static int procfs_vaccess(int dirfd, const char *path, int amode) {
    struct stat st;
    int vr = __ape_shim_procfs_vstat(dirfd, path, &st, 0);
    if (vr == -2) return -2;
    if (vr < 0) return -1;
    if (amode & W_OK) return errno = EACCES, -1;
    return 0;
}

int __ape_shim_access(const char *path, int amode) {
    int vr = procfs_vaccess(AT_FDCWD, path, amode);
    if (vr != -2) return vr;
    int rc = access(path, amode);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = access(buf, amode);
    }
    return rc;
}

// The effective-identity spellings of the same question.
int __ape_shim_eaccess(const char *path, int amode) {
    int vr = procfs_vaccess(AT_FDCWD, path, amode);
    if (vr != -2) return vr;
    int rc = eaccess(path, amode);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf)) rc = eaccess(buf, amode);
    }
    return rc;
}

int __ape_shim_euidaccess(const char *path, int amode) {
    int vr = procfs_vaccess(AT_FDCWD, path, amode);
    if (vr != -2) return vr;
    int rc = euidaccess(path, amode);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf))
            rc = euidaccess(buf, amode);
    }
    return rc;
}

int __ape_shim_faccessat(int dirfd, const char *path, int amode, int flags) {
    int vr = procfs_vaccess(dirfd, path, amode);
    if (vr != -2) return vr;
    int rc = faccessat(dirfd, path, amode, flags);
    if (rc < 0 && errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(dirfd, path, buf))
            rc = faccessat(dirfd, buf, amode, flags);
    }
    return rc;
}

// mkfifo(): cosmo ships mknod() but not the POSIX wrapper over it. Nothing
// needs translating -- both sides spell S_IFIFO 0010000 and the permission
// bits agree -- so this is the same one-liner musl uses. Hosts without FIFOs
// (Windows) fail inside cosmo's mknod, which is where that belongs.
int __ape_shim_mkfifo(const char *path, unsigned mode) {
    return mknod(path, S_IFIFO | mode, 0);
}
