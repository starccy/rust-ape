// Descriptors of the tree (shape 1): a content file opened for reading is
// answered with a memory descriptor holding the text generated for that
// open, and a directory descriptor is remembered so relative names through
// it can be joined back to their virtual path.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows() and g_fds

#include <dirent.h>
#include <errno.h>
#include <limits.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>
#include <libc/calls/internal.h>
#include <libc/dce.h>
#include <libc/nt/files.h>
#include <libc/runtime/runtime.h>

#include "core.h"

// ---------------------------------------------------------------------------
// Entry: the open interception in shim/open.c. Content never touches the
// disk. The generator's text is handed out behind a descriptor slot that
// cosmo has reserved but holds nothing in: the shim's own read, lseek,
// fstat and close entry points recognize the slot and serve the text, and
// cosmo itself leaves a reserved slot alone, treats it as close-on-exec
// and copies it with the heap on fork, which is the lifetime a /proc reader
// expects. A monitor that keeps hundreds of these open costs the host
// nothing, where a file per open used to cost a create, a write and a
// delete on the filter drivers' watch. The text is fresh at every open and
// regenerated in place by lseek(fd, 0, SEEK_SET) for readers that keep it
// open. -2 declines the path, which sends directories, links named as
// such, and anything unknown down the ordinary rewrite road.

// Linux lets two threads share one open file, racing on its position, so
// pos and the text are guarded by a lock per descriptor; a close under a
// reader is the caller's own race, as it is for any descriptor.
struct memfd {
    pthread_mutex_t lock;
    char *p;
    size_t n;
    size_t pos;
    int64_t gen; // slot generation the content came from, 0 if not a slot file
    bool dir;    // a listing's descriptor: no text, stats as a directory
    char vpath[200];
};

// Descriptor to memfd, grown by chunk so a lookup never takes a lock and a
// program with thousands of descriptors is not refused.
#define MEMFD_CHUNK 256
#define MEMFD_CHUNKS 4096
static _Atomic(_Atomic(struct memfd *) *) g_memfd[MEMFD_CHUNKS];

static struct memfd *memfd_of(int fd) {
    if (fd < 0 || fd >= MEMFD_CHUNK * MEMFD_CHUNKS) return 0;
    if ((unsigned)fd >= g_fds.n || g_fds.p[fd].kind != kFdReserved) return 0;
    _Atomic(struct memfd *) *c =
        atomic_load_explicit(&g_memfd[fd / MEMFD_CHUNK], memory_order_acquire);
    if (!c) return 0;
    return atomic_load_explicit(&c[fd % MEMFD_CHUNK], memory_order_acquire);
}

// Under pc_lock.
static bool memfd_put(int fd, struct memfd *m) {
    if (fd < 0 || fd >= MEMFD_CHUNK * MEMFD_CHUNKS) return false;
    _Atomic(struct memfd *) *c =
        atomic_load_explicit(&g_memfd[fd / MEMFD_CHUNK], memory_order_acquire);
    if (!c) {
        c = calloc(MEMFD_CHUNK, sizeof *c);
        if (!c) return false;
        atomic_store_explicit(&g_memfd[fd / MEMFD_CHUNK], c,
                              memory_order_release);
    }
    atomic_store_explicit(&c[fd % MEMFD_CHUNK], m, memory_order_release);
    return true;
}

// Takes the text out of b. Under pc_lock, since the generation stamp is read
// from the slot the text came from.
static int memfd_open(struct pfs_buf *b, int hostflags, const char *vpath) {
    struct memfd *m = calloc(1, sizeof *m);
    if (!m) {
        pfs_buf_free(b);
        return -2;
    }
    pthread_mutex_init(&m->lock, 0);
    m->p = b->p;
    m->n = b->n;
    m->gen = pc_content_gen(vpath);
    snprintf(m->vpath, sizeof m->vpath, "%s", vpath);
    memset(b, 0, sizeof *b);

    int fd = __reservefd(-1);
    if (fd == -1 || !memfd_put(fd, m)) {
        if (fd != -1) __releasefd(fd);
        free(m->p);
        free(m);
        return -2;
    }
    g_fds.p[fd].flags = hostflags & (O_ACCMODE | O_CLOEXEC);
    g_fds.p[fd].mode = S_IFREG | 0444;
    g_fds.p[fd].handle = 0;
    return fd;
}

// Entry: shim/dirstream.c, for a listing served from memory. std keeps the
// dirfd of a ReadDir and checks it is open before dropping it, so the
// listing gets a descriptor of the same kind, holding no text.
int __ape_shim_procfs_memfd_dir(const char *vpath) {
    struct pfs_buf b = {0};
    pthread_mutex_lock(&pc_lock);
    int fd = memfd_open(&b, O_RDONLY | O_CLOEXEC, vpath);
    if (fd >= 0) {
        struct memfd *m = memfd_of(fd);
        m->dir = true;
        m->gen = 1; // never regenerated
        g_fds.p[fd].mode = S_IFDIR | 0555;
    }
    pthread_mutex_unlock(&pc_lock);
    return fd;
}

// Generates the content of a virtual path into b; false when the path is
// not a content file we generate. Takes pc_lock itself.
static bool gen_vpath(const char *vpath, struct pfs_buf *b) {
    bool ok;
    pthread_mutex_lock(&pc_lock);
    pc_busy = 1;
    if (!strncmp(vpath, "/sys/", 5)) {
        ok = pc_gen_sysfs(vpath, b);
    } else {
        struct node n;
        pc_parse(vpath + 5, &n);
        ok = pc_gen_node(&n, b);
    }
    pc_busy = 0;
    pthread_mutex_unlock(&pc_lock);
    if (!ok || b->oom) {
        pfs_buf_free(b);
        return false;
    }
    return true;
}

// Entry: shim/read.c. -2 when fd is not a memory descriptor.
long __ape_shim_procfs_memfd_read(int fd, const struct iovec *iov,
                                  int iovlen) {
    if (!IsWindows()) return -2;
    struct memfd *m = memfd_of(fd);
    if (!m) return -2;
    size_t done = 0;
    pthread_mutex_lock(&m->lock);
    for (int i = 0; i < iovlen && m->pos < m->n; i++) {
        size_t k = m->n - m->pos;
        if (k > iov[i].iov_len) k = iov[i].iov_len;
        memcpy(iov[i].iov_base, m->p + m->pos, k);
        m->pos += k;
        done += k;
    }
    pthread_mutex_unlock(&m->lock);
    return (long)done;
}

// Entry: shim/open.c, for every lseek. -2 when fd is not a memory
// descriptor. A reader that keeps /proc/<pid>/stat open and rewinds it each
// round (sysinfo does) gets the kernel's fresh text on Linux; here the text
// is regenerated on the rewind, so it sees the present rather than the
// moment of open. The volatile pid files come from a slot generation that
// is fixed for a throttle window, so a rewind inside the window that
// produced the text (sysinfo rewinds right after opening) changes nothing.
int64_t __ape_shim_procfs_memfd_lseek(int fd, int64_t off, int whence) {
    if (!IsWindows()) return -2;
    struct memfd *m = memfd_of(fd);
    if (!m) return -2;
    if (off == 0 && whence == SEEK_SET && !pc_busy && !m->dir) {
        bool stale;
        pthread_mutex_lock(&pc_lock);
        stale = !m->gen || m->gen != pc_content_gen(m->vpath);
        pthread_mutex_unlock(&pc_lock);
        if (stale) {
            // generated outside m->lock, since generation takes pc_lock
            // and may be slow; swapped in under it
            struct pfs_buf b = {0};
            if (gen_vpath(m->vpath, &b)) {
                pthread_mutex_lock(&pc_lock);
                int64_t gen = pc_content_gen(m->vpath);
                pthread_mutex_unlock(&pc_lock);
                pthread_mutex_lock(&m->lock);
                free(m->p);
                m->p = b.p;
                m->n = b.n;
                m->gen = gen;
                m->pos = 0;
                pthread_mutex_unlock(&m->lock);
                return 0;
            }
        }
        pthread_mutex_lock(&m->lock);
        m->pos = 0;
        pthread_mutex_unlock(&m->lock);
        return 0;
    }
    pthread_mutex_lock(&m->lock);
    int64_t base, rc;
    switch (whence) {
        case SEEK_SET: base = 0; break;
        case SEEK_CUR: base = (int64_t)m->pos; break;
        case SEEK_END: base = (int64_t)m->n; break;
        default: base = -1; break;
    }
    if (base < 0 || base + off < 0) {
        errno = EINVAL;
        rc = -1;
    } else {
        m->pos = (size_t)(base + off);
        rc = (int64_t)m->pos;
    }
    pthread_mutex_unlock(&m->lock);
    return rc;
}

// Entry: shim/open.c, for fstat. -2 when fd is not a memory descriptor.
int __ape_shim_procfs_memfd_fstat(int fd, struct stat *st) {
    if (!IsWindows()) return -2;
    struct memfd *m = memfd_of(fd);
    if (!m) return -2;
    memset(st, 0, sizeof *st);
    pthread_mutex_lock(&m->lock);
    size_t n = m->n;
    pthread_mutex_unlock(&m->lock);
    st->st_mode = m->dir ? S_IFDIR | 0555 : S_IFREG | 0444;
    st->st_nlink = 1;
    st->st_size = (int64_t)n;
    st->st_blksize = 4096;
    st->st_blocks = (int64_t)((n + 511) / 512);
    st->st_dev = 0x70726f63; // "proc"
    st->st_ino = (uint64_t)(uintptr_t)m;
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    st->st_atim = st->st_mtim = st->st_ctim = now;
    return 0;
}

// Entry: shim/open.c, for fcntl. -2 when fd is not a memory descriptor.
// Only the descriptor flags are meaningful here: std asks F_GETFD before
// dropping a descriptor, F_GETFL says the file is read-only. The commands
// come in with the portable numbers, and O_RDONLY is 0 on both sides.
int __ape_shim_procfs_memfd_fcntl(int fd, int cmd, int arg) {
    if (!IsWindows()) return -2;
    struct memfd *m = memfd_of(fd);
    if (!m) return -2;
    switch (cmd) {
        case F_GETFD: return (g_fds.p[fd].flags & O_CLOEXEC) ? FD_CLOEXEC : 0;
        case F_SETFD:
            g_fds.p[fd].flags = (g_fds.p[fd].flags & ~O_CLOEXEC) |
                                ((arg & FD_CLOEXEC) ? O_CLOEXEC : 0);
            return 0;
        case F_GETFL: return O_RDONLY;
        case F_SETFL: return 0;
        default: errno = EINVAL; return -1;
    }
}

// Entry: shim/close-nt.c, from close() on a reserved slot. -2 when fd is
// not a memory descriptor; the caller releases the slot.
int __ape_shim_procfs_memfd_close(int fd) {
    if (!IsWindows()) return -2;
    struct memfd *m = memfd_of(fd);
    if (!m) return -2;
    pthread_mutex_lock(&pc_lock);
    memfd_put(fd, 0);
    pthread_mutex_unlock(&pc_lock);
    pthread_mutex_destroy(&m->lock);
    free(m->p);
    free(m);
    return 0;
}

// The content of a parsed path, when it is a content file we generate.
// Called under pc_lock with pc_busy set. A thread's task/<tid>/<file> gets
// the process-wide answer, since NT keeps the per-thread facts Linux would
// split out behind the same handles anyway.
bool pc_gen_node(const struct node *n, struct pfs_buf *b) {
    switch (n->kind) {
        case K_TOP: return pfs_gen_top_file(b, n->name);
        case K_NET_FILE: return pfs_gen_net_file(b, n->name);
        case K_SYS: return pfs_gen_sys_file(b, n->rest);
        case K_PID_SUB:
            if (!n->rest[0]) return pc_pid_content(n->pid, n->name, b);
            if (!strcmp(n->name, "task")) {
                const char *file = strchr(n->rest, '/');
                if (file && !strchr(file + 1, '/'))
                    return pc_pid_content(n->pid, file + 1, b);
            } else if (!strcmp(n->name, "net")) {
                if (!strchr(n->rest, '/') && pfs_proc_find(n->pid))
                    return pfs_gen_net_file(b, n->rest);
            }
            return false;
        default: return false;
    }
}

// The virtual answer for a parsed path; -2 when the path is not a content
// file we generate.
static int virtual_open(const struct node *n, const char *vpath,
                        int hostflags) {
    if ((hostflags & O_ACCMODE) != O_RDONLY) return -2;
    if (hostflags & O_DIRECTORY) return -2;

    // exe, cwd and root opened for content mean "open the target"
    const char *link = pc_link_name(n);
    if (link) {
        if (hostflags & O_NOFOLLOW) return -2; // naming the link itself
        char target[600];
        long r = pfs_pid_link(n->pid, link, target, sizeof target - 1);
        if (r <= 0) return -2;
        target[r] = 0;
        return open(target, hostflags);
    }

    struct pfs_buf b = {0};
    if (!gen_vpath(vpath, &b)) return -2;
    pthread_mutex_lock(&pc_lock);
    int fd = memfd_open(&b, hostflags, vpath);
    pthread_mutex_unlock(&pc_lock);
    return fd;
}

// Whether a parsed path names a directory of the tree: a descriptor of one
// is remembered so that dirfd-relative opens through it can be answered
// virtually too.
static bool is_dir_node(const struct node *n) {
    switch (n->kind) {
        case K_ROOT:
        case K_PID_DIR:
        case K_NET_DIR: return true;
        case K_PID_SUB:
            if (!strcmp(n->name, "fd") || !strcmp(n->name, "net"))
                return !n->rest[0];
            if (!strcmp(n->name, "task"))
                return !n->rest[0] || !strchr(n->rest, '/');
            return false;
        case K_SYS: {
            struct pfs_buf b = {0};
            bool file = pfs_gen_sys_file(&b, n->rest);
            pfs_buf_free(&b);
            return !file;
        }
        default: return false;
    }
}

int __ape_shim_procfs_open(const char *path, int hostflags) {
    if (!IsWindows() || !path || pc_busy) return -2;
    if (!strncmp(path, "/sys/", 5)) {
        if ((hostflags & O_ACCMODE) != O_RDONLY) return -2;
        if (hostflags & O_DIRECTORY) return -2;
        struct pfs_buf b = {0};
        if (!gen_vpath(path, &b)) return -2;
        pthread_mutex_lock(&pc_lock);
        int fd = memfd_open(&b, hostflags, path);
        pthread_mutex_unlock(&pc_lock);
        return fd;
    }
    if (strncmp(path, "/proc", 5) || (path[5] && path[5] != '/')) return -2;
    struct node n;
    pc_parse(path + 5, &n);
    return virtual_open(&n, path, hostflags);
}

// ---------------------------------------------------------------------------
// Directory descriptors of the tree. A reader that opens /proc/<pid> once and
// then openat()s stat, cmdline, io through it (bottom, the procfs crate)
// never spells /proc again, so the descriptor is remembered under its
// virtual path when handed out and the relative names are joined to it. The
// entry is validated against the handle cosmo holds for the fd and dropped
// by close (shim/close-nt.c), so a number that was closed behind our back
// and reused cannot be mistaken for a directory of ours.

#define NTRACK 256
static struct trackfd g_track[NTRACK];
static bool g_track_init;

struct trackfd *pc_track_find(int fd) {
    if (!g_track_init) return 0; // an all-zero table would match fd 0
    for (int i = 0; i < NTRACK; i++)
        if (atomic_load_explicit(&g_track[i].fd, memory_order_relaxed) == fd)
            return &g_track[i];
    return 0;
}

// Under pc_lock.
static void track_put(int fd, const char *vpath) {
    if (!g_track_init) {
        for (int i = 0; i < NTRACK; i++) atomic_init(&g_track[i].fd, -1);
        g_track_init = true;
    }
    if ((unsigned)fd >= g_fds.n) return;
    struct trackfd *t = pc_track_find(fd);
    if (!t) t = pc_track_find(-1);
    if (!t) return;
    atomic_store_explicit(&t->fd, -1, memory_order_relaxed);
    t->handle = g_fds.p[fd].handle;
    snprintf(t->vpath, sizeof t->vpath, "%s", vpath);
    atomic_store_explicit(&t->fd, fd, memory_order_release);
}

// The live entry for fd, or 0.
struct trackfd *pc_track_get(int fd) {
    struct trackfd *t = pc_track_find(fd);
    if (!t) return 0;
    if ((unsigned)fd >= g_fds.n || g_fds.p[fd].kind != kFdFile ||
        g_fds.p[fd].handle != t->handle)
        return 0;
    return t;
}

// From close(), which runs under pc_lock whenever the carrier closes a file
// of its own, so this must not take the lock: the slot is released with a
// plain atomic store, and a reader that races it re-validates the handle.
void __ape_shim_procfs_fd_closed(int fd) {
    if (!IsWindows() || !g_track_init || fd < 0) return;
    struct trackfd *t = pc_track_find(fd);
    if (t) atomic_store_explicit(&t->fd, -1, memory_order_relaxed);
}

// Remember `fd` as `vpath` when that names a directory of the tree.
void __ape_shim_procfs_track(int fd, const char *vpath) {
    if (!IsWindows() || fd < 0 || !vpath || pc_busy) return;
    if (strncmp(vpath, "/proc", 5) || (vpath[5] && vpath[5] != '/')) return;
    struct node n;
    pc_parse(vpath + 5, &n);
    if (!is_dir_node(&n)) return;
    if ((unsigned)fd >= g_fds.n) return;
    pthread_mutex_lock(&pc_lock);
    track_put(fd, vpath);
    pthread_mutex_unlock(&pc_lock);
}

// The virtual path of a dirfd-relative name, when dirfd is one of ours;
// false otherwise. `rel` must be a plain relative name (no leading slash).
bool __ape_shim_procfs_join(int dirfd, const char *rel, char *out,
                            unsigned long outsz) {
    if (!IsWindows() || dirfd < 0 || !rel || !rel[0] || rel[0] == '/')
        return false;
    if (pc_busy || !g_track_init) return false;
    bool ok = false;
    pthread_mutex_lock(&pc_lock);
    struct trackfd *t = pc_track_get(dirfd);
    if (t) {
        // "." and ".." are left to the host: the join is textual
        bool dots = false;
        for (const char *p = rel; *p && !dots;) {
            const char *q = p;
            while (*q && *q != '/') q++;
            if ((q - p == 1 && p[0] == '.') ||
                (q - p == 2 && p[0] == '.' && p[1] == '.'))
                dots = true;
            p = *q ? q + 1 : q;
        }
        if (!dots) {
            int len = snprintf(out, outsz, "%s/%s", t->vpath, rel);
            ok = len > 0 && (unsigned long)len < outsz;
        }
    }
    pthread_mutex_unlock(&pc_lock);
    return ok;
}

void __ape_shim_procfs_list_fd(int fd) {
    if (!IsWindows() || pc_busy || !g_track_init) return;
    char vpath[200];
    vpath[0] = 0;
    pthread_mutex_lock(&pc_lock);
    struct trackfd *t = pc_track_get(fd);
    if (t) snprintf(vpath, sizeof vpath, "%s", t->vpath);
    pthread_mutex_unlock(&pc_lock);
    if (vpath[0]) __ape_shim_procfs_list(vpath);
}
