// Descriptors of the tree (shape 1): a content file opened for reading is
// answered with a scratch descriptor generated for that open, and a
// directory descriptor is remembered so relative names through it can be
// joined back to their virtual path.

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
#include <time.h>
#include <unistd.h>
#include <libc/calls/internal.h>
#include <libc/dce.h>
#include <libc/nt/files.h>
#include <libc/runtime/runtime.h>

#include "core.h"

// ---------------------------------------------------------------------------
// Entry: the open interception in shim/open.c. Content is never read from
// the materialized tree; a reader gets a delete-on-close scratch file that
// was generated for this open, written and rewound on the same handle. The
// on-disk copy is written once per process and goes stale by design, and
// NT's filter drivers hold the first open of a just-modified file until
// they have scanned it, so a materialize-then-open carrier would pay write
// plus scan on every change while a scratch file is opened once, by us,
// and the reader inherits the handle. -2 declines the path, which sends
// directories, links named as such, and anything unknown down the ordinary
// rewrite road.

static void track_put(int fd, const char *vpath, bool scratch);

static int scratch_fd(struct pfs_buf *b, int hostflags, const char *vpath) {
    int fd = -2;
    pthread_mutex_lock(&pc_lock);
    if (pc_ensure_root()) {
        static int seq;
        char tmp[600];
        snprintf(tmp, sizeof tmp, "%s/.open-%d", pc_root, seq++);
        pc_busy = 1;
        // O_UNLINK is cosmo's delete-on-close; ~0 would mean "unsupported
        // here" (see shim/README on cosmo's sentinel constants)
        int unlink_flag = O_UNLINK != ~0u ? (int)O_UNLINK : 0;
        fd = open(tmp, O_RDWR | O_CREAT | O_EXCL | unlink_flag |
                           (hostflags & O_CLOEXEC),
                  0600);
        if (fd >= 0) {
            for (size_t off = 0; off < b->n;) {
                long w = write(fd, b->p + off, b->n - off);
                if (w <= 0) break;
                off += (size_t)w;
            }
            lseek(fd, 0, SEEK_SET);
            if (!unlink_flag) unlink(tmp);
            track_put(fd, vpath, true); // for rewinds
            struct trackfd *t = pc_track_get(fd, true);
            if (t) {
                t->gen = pc_content_gen(vpath);
                t->size = b->n;
            }
        } else {
            fd = -2;
        }
        pc_busy = 0;
    }
    pthread_mutex_unlock(&pc_lock);
    pfs_buf_free(b);
    return fd;
}

// The content of a parsed path, when it is a content file we generate.
// Called under pc_lock with pc_busy set.
bool pc_gen_node(const struct node *n, struct pfs_buf *b) {
    switch (n->kind) {
        case K_TOP: return pfs_gen_top_file(b, n->name);
        case K_NET_FILE: return pfs_gen_net_file(b, n->name);
        case K_SYS: return pfs_gen_sys_file(b, n->rest);
        case K_PID_SUB:
            if (!n->rest[0]) return pc_pid_content(n->pid, n->name, b);
            if (!strcmp(n->name, "task")) {
                // task/<tid>/<file>: the process-wide answer, as the
                // materialized tree gives it
                const char *file = strchr(n->rest, '/');
                if (file && !strchr(file + 1, '/')) {
                    file++;
                    if (!strcmp(file, "stat") || !strcmp(file, "status") ||
                        !strcmp(file, "io"))
                        return pc_pid_content(n->pid, file, b);
                }
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
    if (n->kind == K_PID_SUB && !n->rest[0] &&
        (!strcmp(n->name, "exe") || !strcmp(n->name, "cwd") ||
         !strcmp(n->name, "root"))) {
        if (hostflags & O_NOFOLLOW) return -2; // naming the link itself
        char target[600];
        long r = pfs_pid_link(n->pid, n->name, target, sizeof target - 1);
        if (r <= 0) return -2;
        target[r] = 0;
        return open(target, hostflags);
    }

    struct pfs_buf b = {0};
    pthread_mutex_lock(&pc_lock);
    pc_busy = 1;
    bool ok = pc_gen_node(n, &b);
    pc_busy = 0;
    pthread_mutex_unlock(&pc_lock);
    if (!ok || b.oom) {
        pfs_buf_free(&b);
        return -2;
    }
    return scratch_fd(&b, hostflags, vpath);
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
        pthread_mutex_lock(&pc_lock);
        pc_busy = 1;
        bool ok = pc_gen_sysfs(path, &b);
        pc_busy = 0;
        pthread_mutex_unlock(&pc_lock);
        if (!ok || b.oom) {
            pfs_buf_free(&b);
            return -2;
        }
        return scratch_fd(&b, hostflags, path);
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
static void track_put(int fd, const char *vpath, bool scratch) {
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
    t->scratch = scratch;
    t->gen = 0;
    t->size = 0;
    snprintf(t->vpath, sizeof t->vpath, "%s", vpath);
    atomic_store_explicit(&t->fd, fd, memory_order_release);
}

// The live entry for fd, of the wanted kind, or 0.
struct trackfd *pc_track_get(int fd, bool scratch) {
    struct trackfd *t = pc_track_find(fd);
    if (!t || t->scratch != scratch) return 0;
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
    track_put(fd, vpath, false);
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
    struct trackfd *t = pc_track_get(dirfd, false);
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

// Entry: shim/open.c, on lseek(fd, 0, SEEK_SET). A reader that keeps
// /proc/<pid>/stat open and rewinds it each round (sysinfo does) gets the
// kernel's fresh text on Linux; here the scratch file is regenerated in
// place, so the rewind sees the present rather than the moment of open.
// The volatile pid files come from a slot generation that is fixed for a
// throttle window, so a rewind inside the window that produced the file
// (sysinfo rewinds right after opening) changes nothing and skips the
// write. Rewriting overwrites in place and only truncates when the text
// got shorter, since a truncate is a separate NTFS round trip.
void __ape_shim_procfs_rewind(int fd) {
    if (!IsWindows() || fd < 0 || pc_busy || !g_track_init) return;
    pthread_mutex_lock(&pc_lock);
    struct trackfd *t = pc_track_get(fd, true);
    if (t) {
        if (t->gen && t->gen == pc_content_gen(t->vpath)) {
            pthread_mutex_unlock(&pc_lock);
            return;
        }
        struct pfs_buf b = {0};
        pc_busy = 1;
        bool ok;
        if (!strncmp(t->vpath, "/sys/", 5)) {
            ok = pc_gen_sysfs(t->vpath, &b);
        } else {
            struct node n;
            pc_parse(t->vpath + 5, &n);
            ok = pc_gen_node(&n, &b);
        }
        if (ok && !b.oom) {
            lseek(fd, 0, SEEK_SET);
            size_t off = 0;
            while (off < b.n) {
                long w = write(fd, b.p + off, b.n - off);
                if (w <= 0) break;
                off += (size_t)w;
            }
            if (off < t->size) ftruncate(fd, off);
            t->size = off;
            t->gen = pc_content_gen(t->vpath);
        }
        pc_busy = 0;
        pfs_buf_free(&b);
    }
    pthread_mutex_unlock(&pc_lock);
}

void __ape_shim_procfs_list_fd(int fd) {
    if (!IsWindows() || pc_busy || !g_track_init) return;
    char vpath[200];
    vpath[0] = 0;
    pthread_mutex_lock(&pc_lock);
    struct trackfd *t = pc_track_get(fd, false);
    if (t) snprintf(vpath, sizeof vpath, "%s", t->vpath);
    pthread_mutex_unlock(&pc_lock);
    if (vpath[0]) __ape_shim_procfs_list(vpath);
}
