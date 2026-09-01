// The tree's symlinks (shape 3): plain files on disk holding the link
// text, answered here for readlink, lstat and readdir.

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

// Whether `sub` (what follows "/proc") is spelled with the "self" alias.
// pc_parse also folds /proc/<getpid()> into self so its content resolves,
// but only the literal spelling is a symlink; the pid directory is a
// directory.
static bool spelled_self(const char *sub) {
    while (*sub == '/') sub++;
    return !strncmp(sub, "self", 4) && (!sub[4] || sub[4] == '/');
}

// exe, cwd and root of /proc/<pid> and of /proc/<pid>/task/<tid>, where
// Linux shows the process's own, or 0.
const char *pc_link_name(const struct node *n) {
    if (n->kind != K_PID_SUB) return 0;
    const char *name = n->name;
    if (!strcmp(name, "task")) {
        name = strchr(n->rest, '/');
        if (!name || strchr(name + 1, '/')) return 0;
        name++;
    } else if (n->rest[0]) {
        return 0;
    }
    if (!strcmp(name, "exe") || !strcmp(name, "cwd") || !strcmp(name, "root"))
        return name;
    return 0;
}

// ---------------------------------------------------------------------------
// Entry: every /proc-shaped readlink, from the vendored shim/readlinkat.c.
// Negative return means "not mine"; the caller falls through to the host.

static long link_of_node(const struct node *n, const char *sub, char *buf,
                         unsigned long bufsiz) {
    if (n->kind == K_PID_DIR && spelled_self(sub)) {
        // /proc/self itself is a symlink to the pid
        const char *after = sub;
        while (*after == '/') after++;
        while (*after && *after != '/') after++;
        if (!*after) {
            int len = snprintf(buf, bufsiz, "%u", n->pid);
            return len > 0 && (unsigned long)len <= bufsiz ? len : -1;
        }
    }
    if (n->kind != K_PID_SUB) return -1;
    const char *link = pc_link_name(n);
    if (link) return pfs_pid_link(n->pid, link, buf, bufsiz);
    if (!strcmp(n->name, "fd") && n->rest[0] && !strchr(n->rest, '/')) {
        int k = atoi(n->rest);
        static struct pfs_fdent ents[512]; // single-shot scratch
        int nfd = n->pid == pfs_self_pid()
                      ? pfs_self_fds(ents, 512)
                      : pfs_other_fds(n->pid, ents, 512);
        if (nfd >= 0) {
            for (int i = 0; i < nfd; i++) {
                if (ents[i].fd != k) continue;
                int len = snprintf(buf, bufsiz, "%s", ents[i].text);
                return len > 0 && (unsigned long)len <= bufsiz ? len : -1;
            }
            return -1;
        }
        // NT: only the socket tables see into other processes
        uint64_t inodes[256];
        nfd = pfs_net_fds_of(n->pid, inodes, 256);
        if (k < 0 || k >= nfd) return -1;
        int len = snprintf(buf, bufsiz, "socket:[%llu]",
                           (unsigned long long)inodes[k]);
        return len > 0 && (unsigned long)len <= bufsiz ? len : -1;
    }
    return -1;
}

// The permission bits of `vpath` when it names one of the tree's symlinks
// (shape 3), 0 otherwise. lstat reports those as S_IFLNK so `ls -l` and
// symlink_metadata() see what Linux shows; the placeholder on disk is a
// plain file. Pure parsing, no NT call, so it costs nothing measurable.
unsigned __ape_shim_procfs_link_mode(const char *vpath) {
    if (!PC_HOSTED() || !vpath) return 0;
    if (strncmp(vpath, "/proc", 5) || (vpath[5] && vpath[5] != '/')) return 0;
    struct node n;
    pc_parse(vpath + 5, &n);
    if (n.kind == K_PID_DIR && spelled_self(vpath + 5)) {
        // /proc/self itself, and only spelled with that one component
        const char *after = vpath + 5;
        while (*after == '/') after++;
        while (*after && *after != '/') after++;
        while (*after == '/') after++;
        return *after ? 0 : 0777;
    }
    if (n.kind != K_PID_SUB) return 0;
    if (pc_link_name(&n)) return 0777;
    if (!strcmp(n.name, "fd") && n.rest[0] && !strchr(n.rest, '/'))
        return 0700; // lrwx------ on Linux
    return 0;
}

// readdir through a directory descriptor of the tree: the placeholder's
// d_type is DT_REG, and std's DirEntry::file_type() trusts d_type, so the
// entries that are links get DT_LNK here (uutils ls prints "->" only for
// those). Only descriptors we track are looked at; a plain scan of the
// table per entry, no NT call.
void __ape_shim_procfs_fix_dirent(int fd, struct dirent *e) {
    if (!PC_HOSTED() || !e || fd < 0 || pc_busy) return;
    if (e->d_type == DT_LNK) return;
    struct trackfd *t = pc_track_find(fd);
    if (!t) return;
    char vpath[PATH_MAX];
    pthread_mutex_lock(&pc_lock);
    t = pc_track_get(fd);
    int len = t ? snprintf(vpath, sizeof vpath, "%s/%s", t->vpath, e->d_name) : 0;
    pthread_mutex_unlock(&pc_lock);
    if (len <= 0 || (size_t)len >= sizeof vpath) return;
    if (__ape_shim_procfs_link_mode(vpath)) e->d_type = DT_LNK;
}

// The descriptor's own path, which is how we recognize one of ours: the
// directory name is unique to this process and cannot occur on the way to
// it. Answers in the \\?\ namespace, normalized to slashes.
static bool fd_phys_path(int fd, char *out, size_t cap) {
    if (fd < 0 || (size_t)fd >= g_fds.n) return false;
    if (g_fds.p[fd].kind != kFdFile) return false;
    char16_t w[600];
    uint32_t len = GetFinalPathNameByHandle(g_fds.p[fd].handle, w, 600, 0);
    if (!len || len >= 600) return false;
    size_t k = 0;
    for (uint32_t i = 0; i < len && k < cap - 1; i++)
        if (w[i] < 128) out[k++] = w[i] == '\\' ? '/' : (char)w[i];
    out[k] = 0;
    return true;
}

long __ape_shim_procfs_readlinkat(int dirfd, const char *path, char *buf,
                                  unsigned long bufsiz) {
    if (!PC_HOSTED() || !bufsiz) return -1;

    char marker[64];
    snprintf(marker, sizeof marker, "/rust-ape-proc-%d/", (int)getpid());

    if (path && path[0] == '/') {
        // absolute /proc path
        if (strncmp(path, "/proc", 5) || (path[5] && path[5] != '/'))
            return -1;
        struct node n;
        pc_parse(path + 5, &n);
        return link_of_node(&n, path + 5, buf, bufsiz);
    }

    if (path && path[0]) {
        // relative to a directory descriptor the tree handed out
        char joined[600];
        if (__ape_shim_procfs_join(dirfd, path, joined, sizeof joined)) {
            struct node jn;
            pc_parse(joined + 5, &jn);
            return link_of_node(&jn, joined + 5, buf, bufsiz);
        }
        if (!IsWindows()) return -1;
        // NT only: a dirfd of the disk skeleton, recognized by its path;
        // reconstruct the virtual path and recurse
        char phys[600];
        if (!fd_phys_path(dirfd, phys, sizeof phys)) return -1;
        char *hit = strstr(phys, marker);
        if (!hit) return -1;
        char virt[600];
        snprintf(virt, sizeof virt, "/proc/%s/%s", hit + strlen(marker),
                 path);
        struct node n;
        pc_parse(virt + 5, &n);
        return link_of_node(&n, virt + 5, buf, bufsiz);
    }

    // empty path: the descriptor itself. On NT ours are plain files
    // holding the link text, so the answer is their contents.
    if (!IsWindows()) return -1;
    char phys[600];
    if (!fd_phys_path(dirfd, phys, sizeof phys)) return -1;
    if (!strstr(phys, marker)) return -1;
    int f = open(phys, O_RDONLY);
    if (f == -1) return -1;
    long r = read(f, buf, bufsiz);
    close(f);
    return r > 0 ? r : -1;
}
