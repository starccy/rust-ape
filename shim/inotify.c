// inotify() for the Linux-personality shim.
//
// Cosmopolitan ships the __NR_inotify_* numbers and nothing else: no
// wrappers, no constants, no emulation. The `inotify` crate declares the
// three entry points itself rather than going through the libc crate, so the
// failure is a link error naming inotify_init1, inotify_add_watch and
// inotify_rm_watch, and it takes the whole `notify` crate down with it --
// watchexec, cargo-watch, mdbook, trunk, every LSP that reloads on save.
//
// Two implementations sit behind the three names:
//
//   Linux  -- the raw syscalls, with real kernel semantics.
//   else   -- a thread per instance that stats the watched paths on a timer,
//             diffs each scan against the last, and writes struct
//             inotify_event records into a pipe. The read end of that pipe is
//             what gets handed back as the inotify fd, so read(), poll(),
//             close() and O_CLOEXEC all behave without special casing, and it
//             lands on shim/epoll.c the same way any other pipe does.
//
// What the emulation cannot promise:
//
//   * Latency is the scan interval, 200ms by default and settable through
//     RUST_APE_INOTIFY_INTERVAL_MS. The kernel reports the moment the write
//     lands; this reports the next time it looks.
//   * A change that is made and undone between two scans is not reported at
//     all. Write, then write back, then nothing happened as far as this can
//     tell.
//   * Renames arrive as IN_DELETE plus IN_CREATE, never IN_MOVED_FROM /
//     IN_MOVED_TO with a matching cookie. Nothing links the two halves once
//     you are only comparing directory listings.
//   * IN_ACCESS, IN_OPEN and the IN_CLOSE_* pair have no counterpart in a
//     stat. They are accepted and never delivered.
//   * Every scan stats every entry of every watched directory, which is what
//     a poll-based watcher costs. Watching a large tree is linear work on a
//     timer, where the kernel's is free.
//
// Windows could do better than polling, through ReadDirectoryChangesW. cosmo
// does not import it, so it would have to come from LoadLibraryA plus
// GetProcAddress, and then be driven with overlapped I/O and bridged onto a
// pollable descriptor -- across two architectures with two calling
// conventions. That is a lot of machinery for a watcher whose consumers all
// debounce anyway.

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

// Linux's IN_* values, hardcoded: there is no cosmo side to read them from.
#define LIN_IN_ACCESS 0x00000001u
#define LIN_IN_MODIFY 0x00000002u
#define LIN_IN_ATTRIB 0x00000004u
#define LIN_IN_CLOSE_WRITE 0x00000008u
#define LIN_IN_CLOSE_NOWRITE 0x00000010u
#define LIN_IN_OPEN 0x00000020u
#define LIN_IN_MOVED_FROM 0x00000040u
#define LIN_IN_MOVED_TO 0x00000080u
#define LIN_IN_CREATE 0x00000100u
#define LIN_IN_DELETE 0x00000200u
#define LIN_IN_DELETE_SELF 0x00000400u
#define LIN_IN_MOVE_SELF 0x00000800u
#define LIN_IN_Q_OVERFLOW 0x00004000u
#define LIN_IN_IGNORED 0x00008000u
#define LIN_IN_ONLYDIR 0x01000000u
#define LIN_IN_DONT_FOLLOW 0x02000000u
#define LIN_IN_MASK_CREATE 0x10000000u
#define LIN_IN_MASK_ADD 0x20000000u
#define LIN_IN_ISDIR 0x40000000u
#define LIN_IN_ONESHOT 0x80000000u

#define LIN_IN_ALL_EVENTS 0x00000fffu

// inotify_init1's flags, which are O_NONBLOCK and O_CLOEXEC as Linux spells
// them. Mapped to whatever the host calls them before any fd is touched.
#define LIN_IN_NONBLOCK 0x000800u
#define LIN_IN_CLOEXEC 0x080000u

// The wire format, identical on both architectures.
struct shim_inotify_event {
    int32_t wd;
    uint32_t mask;
    uint32_t cookie;
    uint32_t len;
};

#define SHIM_IN_EVENT_ALIGN sizeof(struct shim_inotify_event)

// ---------------------------------------------------------------------------
// The emulation.

struct shim_in_kid {
    char *name;
    uint64_t ino;
    int64_t mtime;
    int64_t mtime_ns;
    int64_t size;
    uint32_t mode;
    int isdir;
};

struct shim_in_watch {
    int wd;
    char *path;
    uint32_t mask;
    int isdir;
    int dropped; // reported IN_IGNORED, waiting to be reaped

    // For a directory watch: the last listing, sorted by name. For anything
    // else: the last stat of the path itself.
    struct shim_in_kid *kids;
    size_t nkids;
    struct shim_in_kid self;
};

struct shim_in_inst {
    int used;
    int rfd; // handed to the caller as the inotify fd
    int wfd;
    int next_wd;
    int dead;     // the reader closed its end; stop scanning
    int overflow; // an event was dropped, tell the reader when there is room
    int scanning;
    pthread_t thread;
    struct shim_in_watch *watches;
    size_t nwatches;
    size_t cap;
    pthread_mutex_t lock;
};

// The kernel's default max_user_instances is 128 per uid; a process holds one
// or two.
#define SHIM_IN_MAX_INSTANCES 16
// Matches the kernel's default max_user_watches per instance.
#define SHIM_IN_MAX_WATCHES 8192

static struct shim_in_inst g_insts[SHIM_IN_MAX_INSTANCES];
static pthread_mutex_t g_insts_lock = PTHREAD_MUTEX_INITIALIZER;

static long scan_interval_ms(void) {
    static long cached = -1;
    if (cached < 0) {
        const char *v = getenv("RUST_APE_INOTIFY_INTERVAL_MS");
        long ms = v && *v ? strtol(v, NULL, 10) : 0;
        cached = ms > 0 ? ms : 200;
    }
    return cached;
}

// Caller must hold g_insts_lock.
static struct shim_in_inst *inst_by_fd_locked(int fd) {
    for (int i = 0; i < SHIM_IN_MAX_INSTANCES; i++)
        if (g_insts[i].used && !g_insts[i].dead && g_insts[i].rfd == fd)
            return &g_insts[i];
    return NULL;
}

static void kid_free(struct shim_in_kid *k) {
    free(k->name);
    k->name = NULL;
}

static void watch_free(struct shim_in_watch *w) {
    for (size_t i = 0; i < w->nkids; i++) kid_free(&w->kids[i]);
    free(w->kids);
    free(w->path);
    kid_free(&w->self);
    memset(w, 0, sizeof(*w));
}

static void kid_from_stat(struct shim_in_kid *k, const struct stat *st) {
    k->ino = (uint64_t)st->st_ino;
    k->mtime = (int64_t)st->st_mtim.tv_sec;
    k->mtime_ns = (int64_t)st->st_mtim.tv_nsec;
    k->size = (int64_t)st->st_size;
    k->mode = (uint32_t)st->st_mode;
    k->isdir = S_ISDIR(st->st_mode) ? 1 : 0;
}

static int kid_cmp(const void *a, const void *b) {
    return strcmp(((const struct shim_in_kid *)a)->name,
                  ((const struct shim_in_kid *)b)->name);
}

// Writes one record. A pipe write at or under PIPE_BUF is all-or-nothing, and
// these are far under it, so a full queue drops whole events rather than
// splitting one. Rust installs SIG_IGN for SIGPIPE before main, which is what
// turns a closed reader into EPIPE here instead of a signal.
static void emit(struct shim_in_inst *in, int wd, uint32_t mask,
                 const char *name) {
    if (in->dead) return;

    char buf[sizeof(struct shim_inotify_event) + NAME_MAX + 1 +
             SHIM_IN_EVENT_ALIGN];
    struct shim_inotify_event ev = {.wd = wd, .mask = mask, .cookie = 0};
    size_t len = 0;
    if (name && *name) {
        size_t n = strlen(name);
        if (n > NAME_MAX) n = NAME_MAX;
        // The kernel pads the name with NULs up to an alignment boundary and
        // counts the padding in len; readers stop at the first NUL.
        len = (n + 1 + SHIM_IN_EVENT_ALIGN - 1) & ~(SHIM_IN_EVENT_ALIGN - 1);
        memcpy(buf + sizeof(ev), name, n);
        memset(buf + sizeof(ev) + n, 0, len - n);
    }
    ev.len = (uint32_t)len;
    memcpy(buf, &ev, sizeof(ev));

    ssize_t w = write(in->wfd, buf, sizeof(ev) + len);
    if (w >= 0) return;
    if (errno == EAGAIN || errno == EWOULDBLOCK) {
        in->overflow = 1;
        return;
    }
    if (errno != EINTR) in->dead = 1;
}

// Directory watch: list, sort, walk the two sorted lists in step.
static void scan_dir(struct shim_in_inst *in, struct shim_in_watch *w) {
    DIR *d = opendir(w->path);
    if (!d) {
        if (errno == ENOENT || errno == ENOTDIR) {
            if (w->mask & LIN_IN_DELETE_SELF)
                emit(in, w->wd, LIN_IN_DELETE_SELF, NULL);
            if (w->mask) emit(in, w->wd, LIN_IN_IGNORED, NULL);
            w->dropped = 1;
        }
        return;
    }

    struct shim_in_kid *now = NULL;
    size_t n = 0, cap = 0;
    struct dirent *de;
    while ((de = readdir(d))) {
        if (!strcmp(de->d_name, ".") || !strcmp(de->d_name, "..")) continue;
        char child[PATH_MAX];
        int len = snprintf(child, sizeof(child), "%s/%s", w->path, de->d_name);
        if (len < 0 || (size_t)len >= sizeof(child)) continue;
        struct stat st;
        if (lstat(child, &st) < 0) continue; // vanished mid-scan
        if (n == cap) {
            size_t ncap = cap ? cap * 2 : 16;
            struct shim_in_kid *p = realloc(now, ncap * sizeof(*p));
            if (!p) break;
            now = p;
            cap = ncap;
        }
        memset(&now[n], 0, sizeof(now[n]));
        now[n].name = strdup(de->d_name);
        if (!now[n].name) break;
        kid_from_stat(&now[n], &st);
        n++;
    }
    closedir(d);
    if (n > 1) qsort(now, n, sizeof(*now), kid_cmp);

    size_t i = 0, j = 0;
    while (i < w->nkids || j < n) {
        int c;
        if (i == w->nkids) c = 1;
        else if (j == n) c = -1;
        else c = strcmp(w->kids[i].name, now[j].name);

        if (c < 0) {
            uint32_t m = LIN_IN_DELETE | (w->kids[i].isdir ? LIN_IN_ISDIR : 0);
            if (w->mask & LIN_IN_DELETE) emit(in, w->wd, m, w->kids[i].name);
            i++;
        } else if (c > 0) {
            uint32_t m = LIN_IN_CREATE | (now[j].isdir ? LIN_IN_ISDIR : 0);
            if (w->mask & LIN_IN_CREATE) emit(in, w->wd, m, now[j].name);
            j++;
        } else {
            struct shim_in_kid *o = &w->kids[i], *p = &now[j];
            uint32_t isdir = p->isdir ? LIN_IN_ISDIR : 0;
            if (o->ino != p->ino) {
                // Replaced in place: the name survived, the file did not.
                if (w->mask & LIN_IN_DELETE)
                    emit(in, w->wd, LIN_IN_DELETE | isdir, o->name);
                if (w->mask & LIN_IN_CREATE)
                    emit(in, w->wd, LIN_IN_CREATE | isdir, p->name);
            } else {
                if ((o->mtime != p->mtime || o->mtime_ns != p->mtime_ns ||
                     o->size != p->size) &&
                    (w->mask & LIN_IN_MODIFY))
                    emit(in, w->wd, LIN_IN_MODIFY | isdir, p->name);
                if (o->mode != p->mode && (w->mask & LIN_IN_ATTRIB))
                    emit(in, w->wd, LIN_IN_ATTRIB | isdir, p->name);
            }
            i++;
            j++;
        }
    }

    for (size_t k = 0; k < w->nkids; k++) kid_free(&w->kids[k]);
    free(w->kids);
    w->kids = now;
    w->nkids = n;
}

static void scan_file(struct shim_in_inst *in, struct shim_in_watch *w) {
    struct stat st;
    int rc = (w->mask & LIN_IN_DONT_FOLLOW) ? lstat(w->path, &st)
                                            : stat(w->path, &st);
    if (rc < 0) {
        if (errno == ENOENT || errno == ENOTDIR) {
            if (w->mask & LIN_IN_DELETE_SELF)
                emit(in, w->wd, LIN_IN_DELETE_SELF, NULL);
            emit(in, w->wd, LIN_IN_IGNORED, NULL);
            w->dropped = 1;
        }
        return;
    }
    struct shim_in_kid now;
    memset(&now, 0, sizeof(now));
    kid_from_stat(&now, &st);
    if (now.ino != w->self.ino) {
        if (w->mask & LIN_IN_ATTRIB) emit(in, w->wd, LIN_IN_ATTRIB, NULL);
    } else {
        if ((now.mtime != w->self.mtime || now.mtime_ns != w->self.mtime_ns ||
             now.size != w->self.size) &&
            (w->mask & LIN_IN_MODIFY))
            emit(in, w->wd, LIN_IN_MODIFY, NULL);
        if (now.mode != w->self.mode && (w->mask & LIN_IN_ATTRIB))
            emit(in, w->wd, LIN_IN_ATTRIB, NULL);
    }
    now.name = w->self.name;
    w->self = now;
}

static void *scan_thread(void *arg) {
    struct shim_in_inst *in = arg;
    long ms = scan_interval_ms();
    for (;;) {
        struct timespec ts = {.tv_sec = ms / 1000,
                              .tv_nsec = (ms % 1000) * 1000000L};
        nanosleep(&ts, NULL);

        pthread_mutex_lock(&in->lock);
        if (in->dead) {
            pthread_mutex_unlock(&in->lock);
            break;
        }
        if (in->overflow) {
            in->overflow = 0;
            emit(in, -1, LIN_IN_Q_OVERFLOW, NULL);
        }
        for (size_t i = 0; i < in->nwatches; i++) {
            struct shim_in_watch *w = &in->watches[i];
            if (w->dropped) continue;
            if (w->isdir) scan_dir(in, w);
            else scan_file(in, w);
        }
        // Reap what reported IN_IGNORED on this pass.
        for (size_t i = 0; i < in->nwatches;) {
            if (!in->watches[i].dropped) {
                i++;
                continue;
            }
            watch_free(&in->watches[i]);
            memmove(&in->watches[i], &in->watches[i + 1],
                    (in->nwatches - i - 1) * sizeof(in->watches[0]));
            in->nwatches--;
        }
        int done = in->dead;
        pthread_mutex_unlock(&in->lock);
        if (done) break;
    }

    pthread_mutex_lock(&g_insts_lock);
    pthread_mutex_lock(&in->lock);
    for (size_t i = 0; i < in->nwatches; i++) watch_free(&in->watches[i]);
    free(in->watches);
    in->watches = NULL;
    in->nwatches = in->cap = 0;
    close(in->wfd);
    in->wfd = -1;
    in->scanning = 0;
    in->used = 0;
    pthread_mutex_unlock(&in->lock);
    pthread_mutex_unlock(&g_insts_lock);
    return NULL;
}

static int emu_init1(int flags) {
    if (flags & ~(int)(LIN_IN_NONBLOCK | LIN_IN_CLOEXEC))
        return errno = EINVAL, -1;

    pthread_mutex_lock(&g_insts_lock);
    struct shim_in_inst *in = NULL;
    for (int i = 0; i < SHIM_IN_MAX_INSTANCES; i++)
        if (!g_insts[i].used) {
            in = &g_insts[i];
            break;
        }
    if (!in) {
        pthread_mutex_unlock(&g_insts_lock);
        return errno = EMFILE, -1;
    }

    int fds[2];
    if (pipe(fds) < 0) {
        pthread_mutex_unlock(&g_insts_lock);
        return -1;
    }
    // The read end carries whatever the caller asked for; the write end is
    // always non-blocking, so a reader that stops draining costs events
    // rather than wedging the scan thread.
    if (flags & LIN_IN_NONBLOCK)
        fcntl(fds[0], F_SETFL, fcntl(fds[0], F_GETFL, 0) | O_NONBLOCK);
    if (flags & LIN_IN_CLOEXEC) {
        fcntl(fds[0], F_SETFD, FD_CLOEXEC);
        fcntl(fds[1], F_SETFD, FD_CLOEXEC);
    }
    fcntl(fds[1], F_SETFL, fcntl(fds[1], F_GETFL, 0) | O_NONBLOCK);

    memset(in, 0, sizeof(*in));
    pthread_mutex_init(&in->lock, NULL);
    in->used = 1;
    in->rfd = fds[0];
    in->wfd = fds[1];
    in->next_wd = 1;
    pthread_mutex_unlock(&g_insts_lock);
    return fds[0];
}

static int emu_add_watch(int fd, const char *path, uint32_t mask) {
    if (!path) return errno = EFAULT, -1;
    if (!(mask & LIN_IN_ALL_EVENTS)) return errno = EINVAL, -1;

    struct stat st;
    int rc = (mask & LIN_IN_DONT_FOLLOW) ? lstat(path, &st) : stat(path, &st);
    if (rc < 0) return -1;
    int isdir = S_ISDIR(st.st_mode) ? 1 : 0;
    if ((mask & LIN_IN_ONLYDIR) && !isdir) return errno = ENOTDIR, -1;

    pthread_mutex_lock(&g_insts_lock);
    struct shim_in_inst *in = inst_by_fd_locked(fd);
    if (!in) {
        pthread_mutex_unlock(&g_insts_lock);
        return errno = EBADF, -1;
    }
    pthread_mutex_lock(&in->lock);
    pthread_mutex_unlock(&g_insts_lock);

    for (size_t i = 0; i < in->nwatches; i++) {
        struct shim_in_watch *w = &in->watches[i];
        if (w->dropped || strcmp(w->path, path)) continue;
        if (mask & LIN_IN_MASK_CREATE) {
            pthread_mutex_unlock(&in->lock);
            return errno = EEXIST, -1;
        }
        w->mask = (mask & LIN_IN_MASK_ADD) ? (w->mask | mask) : mask;
        int wd = w->wd;
        pthread_mutex_unlock(&in->lock);
        return wd;
    }

    if (in->nwatches >= SHIM_IN_MAX_WATCHES) {
        pthread_mutex_unlock(&in->lock);
        return errno = ENOSPC, -1;
    }
    if (in->nwatches == in->cap) {
        size_t ncap = in->cap ? in->cap * 2 : 8;
        struct shim_in_watch *p = realloc(in->watches, ncap * sizeof(*p));
        if (!p) {
            pthread_mutex_unlock(&in->lock);
            return errno = ENOMEM, -1;
        }
        in->watches = p;
        in->cap = ncap;
    }

    struct shim_in_watch *w = &in->watches[in->nwatches];
    memset(w, 0, sizeof(*w));
    w->wd = in->next_wd++;
    w->path = strdup(path);
    if (!w->path) {
        pthread_mutex_unlock(&in->lock);
        return errno = ENOMEM, -1;
    }
    w->mask = mask;
    w->isdir = isdir;
    kid_from_stat(&w->self, &st);
    in->nwatches++;

    // Take the first listing here rather than on the first scan, so that
    // everything already present counts as "was there", not as a burst of
    // IN_CREATE.
    if (isdir) {
        uint32_t saved = w->mask;
        w->mask = 0; // silence the diff against an empty list
        scan_dir(in, w);
        w->mask = saved;
    }

    int wd = w->wd;
    int start = !in->scanning && !in->dead;
    if (start) in->scanning = 1;
    pthread_mutex_unlock(&in->lock);

    if (start && pthread_create(&in->thread, NULL, scan_thread, in) == 0)
        pthread_detach(in->thread);
    else if (start) {
        pthread_mutex_lock(&in->lock);
        in->scanning = 0;
        pthread_mutex_unlock(&in->lock);
        return errno = EAGAIN, -1;
    }
    return wd;
}

static int emu_rm_watch(int fd, int wd) {
    pthread_mutex_lock(&g_insts_lock);
    struct shim_in_inst *in = inst_by_fd_locked(fd);
    if (!in) {
        pthread_mutex_unlock(&g_insts_lock);
        return errno = EBADF, -1;
    }
    pthread_mutex_lock(&in->lock);
    pthread_mutex_unlock(&g_insts_lock);

    for (size_t i = 0; i < in->nwatches; i++) {
        if (in->watches[i].wd != wd || in->watches[i].dropped) continue;
        emit(in, wd, LIN_IN_IGNORED, NULL);
        watch_free(&in->watches[i]);
        memmove(&in->watches[i], &in->watches[i + 1],
                (in->nwatches - i - 1) * sizeof(in->watches[0]));
        in->nwatches--;
        pthread_mutex_unlock(&in->lock);
        return 0;
    }
    pthread_mutex_unlock(&in->lock);
    return errno = EINVAL, -1;
}

// ---------------------------------------------------------------------------
// Which implementation answers. Same shape as shim/epoll.c: Linux takes the
// syscalls unless RUST_APE_INOTIFY_EMULATE is set, which exists so the
// emulation can be run against the real thing on a machine that has both.

static int use_syscalls(void) {
    static int cached = -1;
    if (cached < 0) {
        const char *v = getenv("RUST_APE_INOTIFY_EMULATE");
        cached = IsLinux() && !(v && *v);
    }
    return cached;
}

int inotify_init1(int flags) {
    if (use_syscalls())
        return (int)__ape_syscall_ret(
            __ape_raw_syscall(__NR_inotify_init1, flags, 0, 0, 0, 0));
    return emu_init1(flags);
}

int inotify_init(void) { return inotify_init1(0); }

int inotify_add_watch(int fd, const char *path, uint32_t mask) {
    if (use_syscalls())
        return (int)__ape_syscall_ret(__ape_raw_syscall(
            __NR_inotify_add_watch, fd, (long)path, (long)mask, 0, 0));
    return emu_add_watch(fd, path, mask);
}

int inotify_rm_watch(int fd, int wd) {
    if (use_syscalls())
        return (int)__ape_syscall_ret(
            __ape_raw_syscall(__NR_inotify_rm_watch, fd, wd, 0, 0, 0));
    return emu_rm_watch(fd, wd);
}
