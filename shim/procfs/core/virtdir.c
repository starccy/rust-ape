// Listings served from memory (the enumeration half of shape 2). A monitor
// enumerates /proc and every /proc/<pid>/task on each refresh, and a
// skeleton on disk made that thousands of directory operations per round.
// The vendored shim/dirstream.c asks here first when a directory is
// opened by name, and walks the answer without a descriptor. The disk
// skeleton stays for what stats by path.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libc/dce.h>

#include "core.h"

struct list {
    struct pfs_virtent *p;
    int n, cap;
    bool oom;
};

static void add(struct list *l, const char *name, unsigned char type) {
    if (l->oom) return;
    if (l->n == l->cap) {
        int cap = l->cap ? l->cap * 2 : 64;
        struct pfs_virtent *p = realloc(l->p, (size_t)cap * sizeof *p);
        if (!p) {
            l->oom = true;
            return;
        }
        l->p = p;
        l->cap = cap;
    }
    struct pfs_virtent *e = &l->p[l->n++];
    snprintf(e->name, sizeof e->name, "%s", name);
    e->type = type;
}

static void add_num(struct list *l, uint32_t v) {
    char name[16];
    snprintf(name, sizeof name, "%u", v);
    add(l, name, DT_DIR);
}

static void dots(struct list *l) {
    add(l, ".", DT_DIR);
    add(l, "..", DT_DIR);
}

// The files of a process directory; a thread's directory has the same
// files but no task/, net/ or fd/ of its own.
static void pid_entries(struct list *l, bool thread) {
    dots(l);
    for (int i = 0; pfs_pid_volatile[i]; i++)
        add(l, pfs_pid_volatile[i], DT_REG);
    for (int i = 0; pfs_pid_stable[i]; i++) add(l, pfs_pid_stable[i], DT_REG);
    add(l, "exe", DT_LNK);
    add(l, "cwd", DT_LNK);
    add(l, "root", DT_LNK);
    if (thread) return;
    add(l, "task", DT_DIR);
    add(l, "net", DT_DIR);
    add(l, "fd", DT_DIR);
}

static void root_entries(struct list *l) {
    dots(l);
    const struct pfs_proc *procs;
    int n = pfs_procs(&procs);
    for (int i = 0; i < n; i++) add_num(l, procs[i].pid);
    add(l, "self", DT_LNK);
    for (int i = 0; pfs_top_files[i]; i++) add(l, pfs_top_files[i], DT_REG);
    add(l, "net", DT_DIR);
    add(l, "sys", DT_DIR);
}

static void net_entries(struct list *l) {
    dots(l);
    for (int i = 0; pfs_net_files[i]; i++) add(l, pfs_net_files[i], DT_REG);
}

// -1: not a directory listed here. -2: one, but for a process that is
// gone. Otherwise fills l.
static int list_node(const struct node *n, struct list *l) {
    switch (n->kind) {
        case K_ROOT:
            root_entries(l);
            return 0;
        case K_NET_DIR:
            net_entries(l);
            return 0;
        case K_PID_DIR:
            if (!pfs_proc_find(n->pid)) return -2;
            pid_entries(l, false);
            return 0;
        case K_PID_SUB:
            if (!strcmp(n->name, "task")) {
                if (!pfs_proc_find(n->pid)) return -2;
                if (!n->rest[0]) {
                    dots(l);
                    uint32_t tids[512];
                    int nt = pfs_threads_of(n->pid, tids, 512);
                    if (!nt) add_num(l, n->pid);
                    for (int i = 0; i < nt; i++) add_num(l, tids[i]);
                    return 0;
                }
                if (!strchr(n->rest, '/')) {
                    pid_entries(l, true);
                    return 0;
                }
                return -1;
            }
            if (!strcmp(n->name, "net") && !n->rest[0]) {
                if (!pfs_proc_find(n->pid)) return -2;
                net_entries(l);
                return 0;
            }
            return -1;
        default: return -1;
    }
}

int __ape_shim_procfs_virtual_dir(const char *path, struct pfs_virtent **out) {
    if (!IsWindows() || !path || pc_busy) return -1;
    if (strncmp(path, "/proc", 5) || (path[5] && path[5] != '/')) return -1;
    struct node n;
    pc_parse(path + 5, &n);
    struct list l = {0};
    pthread_mutex_lock(&pc_lock);
    int rc = list_node(&n, &l);
    pthread_mutex_unlock(&pc_lock);
    if (rc < 0 || l.oom) {
        free(l.p);
        return rc < 0 ? rc : -1;
    }
    *out = l.p;
    return l.n;
}
