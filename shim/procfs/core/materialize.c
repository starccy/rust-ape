// The materialized skeleton (shape 2): one ensure per path shape, each
// behind its own throttle, and the entries that reach it, the path rewrite
// and the dirfd-relative join.

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
// Materialization, one ensure per path shape, each behind its own throttle.

static void gen_to_file(const char *path, bool ok, struct pfs_buf *b) {
    if (ok && !b->oom)
        pc_write_file(path, b->p ? b->p : "", b->n);
    pfs_buf_free(b);
}

static void ensure_top_file(const char *name) {
    // top files change on nearly every look (uptime, stat, meminfo), so a
    // content digest would never skip; a throttle is what bounds the writes
    static int64_t last[16];
    int i = 0;
    while (pfs_top_files[i] && strcmp(pfs_top_files[i], name)) i++;
    if (!pfs_top_files[i]) return;
    int64_t t = pfs_now_ms();
    if (i < 16 && last[i] && t - last[i] < PID_DIR_MS) return;
    if (i < 16) last[i] = t;
    struct pfs_buf b = {0};
    char path[600];
    snprintf(path, sizeof path, "%s/%s", pc_root, name);
    gen_to_file(path, pfs_gen_top_file(&b, name), &b);
}

static void ensure_static(void) {
    static bool done;
    if (done) return;
    done = true;
    char path[600];
    snprintf(path, sizeof path, "%s/net", pc_root);
    mkdir(path, 0755);
    for (int i = 0; pfs_net_files[i]; i++) {
        snprintf(path, sizeof path, "%s/net/%s", pc_root, pfs_net_files[i]);
        pc_write_file(path, "", 0);
    }
    snprintf(path, sizeof path, "%s/sys", pc_root);
    mkdir(path, 0755);
    for (int i = 0; pfs_sys_dirs[i]; i++) {
        snprintf(path, sizeof path, "%s/sys/%s", pc_root, pfs_sys_dirs[i]);
        mkdir(path, 0755);
    }
    for (int i = 0; pfs_sys_files[i]; i++) {
        struct pfs_buf b = {0};
        snprintf(path, sizeof path, "%s/sys/%s", pc_root, pfs_sys_files[i]);
        gen_to_file(path, pfs_gen_sys_file(&b, pfs_sys_files[i]), &b);
    }
    for (int i = 0; pfs_top_files[i]; i++) ensure_top_file(pfs_top_files[i]);
    snprintf(path, sizeof path, "%s/self", pc_root);
    mkdir(path, 0755); // listed under /proc; opens resolve to the pid dir
}

static void ensure_net(void) {
    static int64_t last;
    int64_t t = pfs_now_ms();
    if (last && t - last < NET_MS) return;
    last = t;
    char path[600];
    for (int i = 0; pfs_net_files[i]; i++) {
        struct pfs_buf b = {0};
        snprintf(path, sizeof path, "%s/net/%s", pc_root, pfs_net_files[i]);
        gen_to_file(path, pfs_gen_net_file(&b, pfs_net_files[i]), &b);
    }
}

// One slot per recently seen process. Slots are found by hashed probing --
// NT pids are multiples of 4, so masking their low bits would use a quarter
// of any table -- and carry what lets a refresh be skipped: the last
// generation of the volatile files and, per file, the digest of what is on
// disk; which stable files and links have been written; and the process's
// start time, which tells a recycled pid from the process it used to be.
//
// Materialization is per file, on first access by name: a sweep that opens
// stat, cmdline and io of every process (bottom) writes those three and no
// other, and on later sweeps only the ones whose content moved. The price is
// that listing /proc/<pid> shows just the entries that have been asked for
// by name -- nothing reads a process directory that way except a human's
// ls, and a lookup by name always materializes what it names first.
struct pidslot {
    uint32_t pid;
    int64_t ms;          // when vol[] was generated (also the liveness check)
    uint64_t start;
    struct pfs_buf vol[4]; // the last generation of the volatile four
    struct pfs_buf fixed[3]; // cmdline, comm, environ: fixed per incarnation
    uint32_t written;      // bit per volatile, stable and link entry on disk
    uint64_t fd_digest;    // socket inode list
    int64_t fd_ms;         // when fd/ was last synced
    int64_t net_ms;        // when net/ was last filled for a walker
    bool present;          // the directory exists for this incarnation
    uint64_t task_digest;  // thread id set behind task/, 0 when not built
};
#define NSLOTS 2048

static void slot_reset(struct pidslot *s) {
    for (int i = 0; i < 4; i++) pfs_buf_free(&s->vol[i]);
    for (int i = 0; i < 3; i++) pfs_buf_free(&s->fixed[i]);
    memset(s, 0, sizeof *s);
}

static struct pidslot *slot_of(uint32_t pid) {
    static struct pidslot slots[NSLOTS];
    uint32_t h = (pid * 2654435761u) >> 16;
    struct pidslot *oldest = 0;
    for (int i = 0; i < 8; i++) {
        struct pidslot *s = &slots[(h + i) & (NSLOTS - 1)];
        if (s->pid == pid) return s;
        if (!oldest || s->ms < oldest->ms) oldest = s;
    }
    slot_reset(oldest);
    oldest->pid = pid;
    return oldest;
}

// Threads, only for a caller that actually walks /proc/<pid>/task. The
// tree is a skeleton: content is never read from it (the open interception
// answers task/<tid>/<file> with the process-wide file), so the entries are
// empty placeholders and the tree is only touched when the thread id set
// changes, and then by adding and removing the ids that differ. Monitors
// walk task/ for every process on every refresh, so a rebuild per visit
// costs thousands of file operations per second.
static uint64_t tid_set_digest(const uint32_t *tids, int n) {
    // order independent, so a snapshot listing threads differently is not
    // a change
    uint64_t h = 0xcbf29ce484222325ull;
    for (int i = 0; i < n; i++) {
        uint64_t x = (uint64_t)tids[i] * 0x9e3779b97f4a7c15ull;
        h += x ^ (x >> 29);
    }
    return h ? h : 1;
}

static bool tid_listed(const uint32_t *tids, int n, uint32_t tid) {
    for (int i = 0; i < n; i++)
        if (tids[i] == tid) return true;
    return false;
}

static void materialize_task(struct pidslot *s, uint32_t pid,
                             const char *dir) {
    char path[600];
    uint32_t tids[512];
    int nt = pfs_threads_of(pid, tids, 512);
    if (!nt) {
        tids[0] = pid;
        nt = 1;
    }
    uint64_t d = tid_set_digest(tids, nt);
    if (d == s->task_digest) return;
    s->task_digest = d;

    // drop the threads that are gone, including a previous incarnation's
    // leftovers when the pid was recycled
    snprintf(path, sizeof path, "%s/task", dir);
    if (mkdir(path, 0755) && errno == EEXIST) {
        DIR *dp = opendir(path);
        if (dp) {
            for (struct dirent *e; (e = readdir(dp));) {
                if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
                uint32_t tid = (uint32_t)strtoul(e->d_name, 0, 10);
                if (tid_listed(tids, nt, tid)) continue;
                snprintf(path, sizeof path, "%s/task/%u", dir, tid);
                pc_rm_rf(path);
            }
            closedir(dp);
        }
    }
    static const char *const names[] = {"stat", "status", "io"};
    for (int k = 0; k < nt; k++) {
        snprintf(path, sizeof path, "%s/task/%u", dir, tids[k]);
        if (mkdir(path, 0755) && errno == EEXIST) continue;
        for (int i = 0; i < 3; i++) {
            snprintf(path, sizeof path, "%s/task/%u/%s", dir, tids[k],
                     names[i]);
            pc_write_file(path, "", 0);
        }
    }
}

// The per-process net/ directory: identical to the global one -- there is
// one namespace here -- and filled only for a caller that actually walks
// into it, so a sweep of every pid directory does not multiply the write
// load by the table count.
static void materialize_pid_net(const char *dir) {
    char path[600];
    snprintf(path, sizeof path, "%s/net", dir);
    mkdir(path, 0755);
    for (int i = 0; pfs_net_files[i]; i++) {
        struct pfs_buf b = {0};
        snprintf(path, sizeof path, "%s/net/%s", dir, pfs_net_files[i]);
        gen_to_file(path, pfs_gen_net_file(&b, pfs_net_files[i]), &b);
    }
}

static int name_index(const char *const *names, const char *name) {
    for (int i = 0; names[i]; i++)
        if (!strcmp(names[i], name)) return i;
    return -1;
}

// Bring one process's directory, and the entry `name` inside it (0 for the
// directory itself), up to date. Everything is behind the same throttle:
// within PID_DIR_MS of the last generation nothing is asked of NT again, and
// a file whose digest matches what is on disk is not rewritten.
// The slot of a process with its volatile generation no older than
// PID_DIR_MS; 0 for a process that is not there. One OpenProcess pass per
// throttle window serves every file of the process, however many readers.
static struct pidslot *pid_refresh(uint32_t pid, int64_t t, bool *fresh) {
    struct pidslot *s = slot_of(pid);
    *fresh = s->ms && t - s->ms < PID_DIR_MS;
    if (*fresh) return s->start ? s : 0; // start 0: known absent
    for (int i = 0; i < 4; i++) {
        pfs_buf_free(&s->vol[i]);
        memset(&s->vol[i], 0, sizeof s->vol[i]);
    }
    uint64_t start = 0;
    bool alive = pfs_gen_pid_volatile(pid, s->vol, &start);
    s->ms = t;
    if (!alive) {
        s->start = 0;
        s->present = false;
        return 0;
    }
    if (start != s->start) { // a recycled pid is a new process
        s->start = start;
        s->present = false;
        for (int i = 0; i < 3; i++) {
            pfs_buf_free(&s->fixed[i]);
            memset(&s->fixed[i], 0, sizeof s->fixed[i]);
        }
    }
    return s;
}

void pc_ensure_pid(uint32_t pid, const char *name) {
    int64_t t = pfs_now_ms();
    bool fresh;
    struct pidslot *s = pid_refresh(pid, t, &fresh);
    if (!s) return; // absent process, absent directory

    char dir[600], path[600], link[600];
    snprintf(dir, sizeof dir, "%s/%u", pc_root, pid);

    if (!s->present) {
        // a new incarnation, or a slot that was evicted and came back. The
        // directory may still hold the previous process's placeholders;
        // they are harmless (content never comes from them) and get
        // overwritten on first access by name, so nothing is cleared, and
        // task/, net/ and fd/ appear when first named -- a monitor's cold
        // sweep touches every process once, and four mkdirs and an pc_rm_rf
        // per process were half of it.
        s->present = true;
        s->task_digest = 0;
        s->written = 0;
        s->fd_digest = 0;
        s->fd_ms = 0;
        s->net_ms = 0;
        mkdir(dir, 0755);
    }
    if (!name) return;

    // Content files are written once per incarnation, and that copy is only
    // for stat-shaped access and directory listings: every read of content
    // goes through the open interception (open.c) and never touches it. A
    // rewrite per change would cost a write plus, on the next open, the
    // filter drivers' scan of a modified file (~0.6ms on a Defender host).
    int vi = name_index(pfs_pid_volatile, name);
    if (vi >= 0) {
        if (!(s->written & (1u << (24 + vi))) && !s->vol[vi].oom) {
            s->written |= 1u << (24 + vi);
            snprintf(path, sizeof path, "%s/%s", dir, name);
            pc_write_file(path, s->vol[vi].p ? s->vol[vi].p : "", s->vol[vi].n);
        }
        return;
    }

    int si = name_index(pfs_pid_stable, name);
    if (si >= 0) {
        if (!(s->written & (1u << si))) {
            s->written |= 1u << si;
            struct pfs_buf b = {0};
            snprintf(path, sizeof path, "%s/%s", dir, name);
            gen_to_file(path, pfs_gen_pid_file(&b, pid, name), &b);
        }
        return;
    }

    static const char *const links[] = {"exe", "cwd", "root", 0};
    int li = name_index(links, name);
    if (li >= 0) {
        uint32_t bit = 1u << (16 + li);
        if (!(s->written & bit)) {
            s->written |= bit;
            long r = pfs_pid_link(pid, name, link, sizeof link);
            snprintf(path, sizeof path, "%s/%s", dir, name);
            if (r >= 0) pc_write_file(path, link, (size_t)r);
        }
        return;
    }

    if (!strcmp(name, "fd")) {
        if (s->fd_ms && t - s->fd_ms < PID_DIR_MS) return;
        s->fd_ms = t;
        if (pid == pfs_self_pid()) {
            // ourselves: the full table out of g_fds, real fd numbers
            static struct pfs_fdent ents[512]; // under pc_lock
            int nfd = pfs_self_fds(ents, 512);
            uint64_t fdd = 0xcbf29ce484222325ull;
            for (int k = 0; k < nfd; k++) {
                fdd = pc_fnv64(&ents[k].fd, sizeof ents[k].fd, fdd);
                fdd = pc_fnv64(ents[k].text, strlen(ents[k].text), fdd);
            }
            if (fdd != s->fd_digest) {
                s->fd_digest = fdd;
                snprintf(path, sizeof path, "%s/fd", dir);
                pc_rm_rf(path);
                mkdir(path, 0755);
                for (int k = 0; k < nfd; k++) {
                    snprintf(path, sizeof path, "%s/fd/%d", dir, ents[k].fd);
                    pc_write_file(path, ents[k].text, strlen(ents[k].text));
                }
            }
        } else {
            uint64_t inodes[256];
            int nfd = pfs_net_fds_of(pid, inodes, 256);
            uint64_t fdd = pc_fnv64(inodes, (size_t)nfd * sizeof inodes[0],
                                 0xcbf29ce484222325ull);
            if (fdd != s->fd_digest) {
                s->fd_digest = fdd;
                snprintf(path, sizeof path, "%s/fd", dir);
                pc_rm_rf(path); // the set changed shape; start empty
                mkdir(path, 0755);
                for (int k = 0; k < nfd; k++) {
                    int m = snprintf(link, sizeof link, "socket:[%llu]",
                                     (unsigned long long)inodes[k]);
                    snprintf(path, sizeof path, "%s/fd/%d", dir, k);
                    pc_write_file(path, link, (size_t)m);
                }
            }
        }
        return;
    }

    if (!strcmp(name, "task")) {
        materialize_task(s, pid, dir);
        return;
    }

    if (!strcmp(name, "net")) {
        if (t - s->net_ms >= NET_MS) {
            s->net_ms = t;
            materialize_pid_net(dir);
        }
        return;
    }
}

static void ensure_root_listing(void) {
    static int64_t last;
    int64_t t = pfs_now_ms();
    if (last && t - last < ROOT_LIST_MS) return;
    last = t;

    const struct pfs_proc *procs;
    int n = pfs_procs(&procs);
    if (!n) return;

    // retire directories of departed processes
    DIR *d = opendir(pc_root);
    if (d) {
        char path[600];
        for (struct dirent *e; (e = readdir(d));) {
            if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
            uint32_t pid = (uint32_t)strtoul(e->d_name, 0, 10);
            bool live = false;
            for (int i = 0; i < n && !live; i++) live = procs[i].pid == pid;
            if (live) continue;
            snprintf(path, sizeof path, "%s/%s", pc_root, e->d_name);
            pc_rm_rf(path);
        }
        closedir(d);
    }
    // and list the living; contents arrive when a directory is opened
    char path[600];
    for (int i = 0; i < n; i++) {
        snprintf(path, sizeof path, "%s/%u", pc_root, procs[i].pid);
        mkdir(path, 0755);
    }
    // the top files ride the same cadence, for dirfd-relative readers
    for (int i = 0; pfs_top_files[i]; i++) ensure_top_file(pfs_top_files[i]);
}

void pc_ensure_node(const struct node *n) {
    switch (n->kind) {
        case K_ROOT: ensure_root_listing(); break;
        case K_TOP: ensure_top_file(n->name); break;
        case K_NET_DIR:
        case K_NET_FILE: ensure_net(); break;
        case K_PID_DIR: pc_ensure_pid(n->pid, 0); break;
        case K_PID_SUB: pc_ensure_pid(n->pid, n->name); break;
        case K_SYS:
            if (!strcmp(n->rest, "kernel/random/uuid")) {
                struct pfs_buf b = {0};
                char p[600];
                snprintf(p, sizeof p, "%s/sys/%s", pc_root, n->rest);
                gen_to_file(p, pfs_gen_sys_file(&b, n->rest), &b);
            }
            break;
        default: break;
    }
}

// ---------------------------------------------------------------------------
// Entry: the path rewrite in shim/mkntpath.c. Ensures whatever the path is
// about to touch, then maps it under pc_root.

int __ape_shim_procfs_rewrite(const char *path, char *out,
                              unsigned long outsz) {
    if (!IsWindows() || !path) return 0;
    if (pc_busy) return 0; // our own writes, already real

    const struct pc_sysfs *s = pc_sysfs_match(path);
    if (s) {
        pthread_mutex_lock(&pc_lock);
        pc_busy = 1;
        int ok = 0;
        if (pc_ensure_root()) {
            s->ensure();
            int len = snprintf(out, outsz, "%s/%s%s", pc_root, s->dir,
                               path[s->len] ? path + s->len : "/");
            ok = len > 0 && (unsigned long)len < outsz;
        }
        pc_busy = 0;
        pthread_mutex_unlock(&pc_lock);
        return ok;
    }

    if (strncmp(path, "/proc", 5)) return 0;
    if (path[5] && path[5] != '/') return 0; // /proctor is not /proc

    pthread_mutex_lock(&pc_lock);
    pc_busy = 1;
    int ok = 0;
    if (pc_ensure_root()) {
        ensure_static();
        struct node n;
        pc_parse(path + 5, &n);
        pc_ensure_node(&n);
        char suffix[300];
        pc_phys_suffix(&n, path + 5, suffix, sizeof suffix);
        int len = snprintf(out, outsz, "%s%s", pc_root, suffix);
        ok = len > 0 && (unsigned long)len < outsz;
    }
    pc_busy = 0;
    pthread_mutex_unlock(&pc_lock);
    return ok;
}

// ---------------------------------------------------------------------------
// The slot generation a volatile pid file's content belongs to: the ms
// stamp of its slot, which the generator has just refreshed. 0 for
// anything not served from a slot. Called under pc_lock.
int64_t pc_content_gen(const char *vpath) {
    if (strncmp(vpath, "/proc/", 6)) return 0;
    struct node n;
    pc_parse(vpath + 6, &n);
    if (n.kind != K_PID_SUB) return 0;
    const char *file = 0;
    if (!n.rest[0]) {
        file = n.name;
    } else if (!strcmp(n.name, "task")) {
        const char *f = strchr(n.rest, '/');
        if (f && !strchr(f + 1, '/')) file = f + 1;
    }
    if (!file || name_index(pfs_pid_volatile, file) < 0) return 0;
    struct pidslot *s = slot_of(n.pid);
    if (s->pid != n.pid || !s->start) return 0;
    // a slot past its window is due for a refresh, so its content is not
    // the current answer even though nobody has regenerated it yet
    if (pfs_now_ms() - s->ms >= PID_DIR_MS) return 0;
    return s->ms;
}

// Content of one /proc/<pid> file, from the slot's cached generation for
// the volatile four, generated on the spot for the rest. False: absent.
// Called under pc_lock.
bool pc_pid_content(uint32_t pid, const char *name, struct pfs_buf *out) {
    int vi = name_index(pfs_pid_volatile, name);
    if (vi >= 0) {
        bool fresh;
        struct pidslot *s = pid_refresh(pid, pfs_now_ms(), &fresh);
        if (!s || s->vol[vi].oom) return false;
        pfs_put(out, s->vol[vi].p, s->vol[vi].n);
        return true;
    }
    int si = name_index(pfs_pid_stable, name);
    if (si < 0) return false;
    // cmdline, comm, and another process's environ: one NT query per
    // incarnation (our own environ changes under setenv, so stays live)
    if (si < 2 || (si == 2 && pid != pfs_self_pid())) {
        bool fresh;
        struct pidslot *s = pid_refresh(pid, pfs_now_ms(), &fresh);
        if (!s) return false;
        struct pfs_buf *c = &s->fixed[si];
        if (!c->p && !c->oom && !pfs_gen_pid_file(c, pid, name)) return false;
        if (c->oom) return false;
        pfs_put(out, c->p, c->n);
        return true;
    }
    if (!pfs_proc_find(pid)) return false;
    return pfs_gen_pid_file(out, pid, name);
}

// ---------------------------------------------------------------------------
// Entry: shim/open.c, on opendir()/fdopendir() of a process directory. The
// content files are placeholders written on first access by name, and
// reads leave no trace, so a listing would show only what was stat'ed.
// Enumeration is the one shape that needs the whole set, and no monitor
// enumerates a process directory (they read by name), so the full set is
// written here, once per process, for whoever does.

static void list_pid(uint32_t pid) {
    pthread_mutex_lock(&pc_lock);
    pc_busy = 1;
    if (pc_ensure_root()) {
        pc_ensure_pid(pid, 0);
        for (int i = 0; pfs_pid_volatile[i]; i++)
            pc_ensure_pid(pid, pfs_pid_volatile[i]);
        for (int i = 0; pfs_pid_stable[i]; i++)
            pc_ensure_pid(pid, pfs_pid_stable[i]);
        static const char *const rest[] = {"exe", "cwd", "root", "task",
                                           "net", "fd"};
        for (size_t i = 0; i < sizeof rest / sizeof rest[0]; i++)
            pc_ensure_pid(pid, rest[i]);
    }
    pc_busy = 0;
    pthread_mutex_unlock(&pc_lock);
}

void __ape_shim_procfs_list(const char *vpath) {
    if (!IsWindows() || !vpath || pc_busy) return;
    if (strncmp(vpath, "/proc", 5) || (vpath[5] && vpath[5] != '/')) return;
    struct node n;
    pc_parse(vpath + 5, &n);
    if (n.kind == K_PID_DIR) list_pid(n.pid);
}

// ---------------------------------------------------------------------------
// Entry: shim/mkntpathat.c, with the combined win32 path of a dirfd-relative
// access. A caller that holds a directory descriptor into our tree reaches
// files without ever spelling /proc -- cosmo resolves the descriptor to its
// real path and joins from there -- so the join result is the one place such
// an access can be recognized. When it lands in our tree, the virtual thing
// it names is brought up to date; the path itself is not touched. This is
// what fills the lazily-materialized pieces (task/) and keeps content read
// through a long-held dirfd no staler than one throttle window.

void __ape_shim_procfs_relative(const char16_t *file, unsigned long n) {
    if (!IsWindows() || !file || pc_busy || !pc_rootlen) return;

    char marker[64];
    int mlen = snprintf(marker, sizeof marker, "rust-ape-proc-%d\\",
                        (int)getpid());
    if (mlen <= 0) return;

    // find the marker in the char16 path
    unsigned long i = 0, hit = (unsigned long)-1;
    for (; i + (unsigned long)mlen <= n; i++) {
        unsigned long j = 0;
        while (j < (unsigned long)mlen && file[i + j] < 128 &&
               (char)file[i + j] == marker[j])
            j++;
        if (j == (unsigned long)mlen) {
            hit = i + (unsigned long)mlen;
            break;
        }
    }
    if (hit == (unsigned long)-1) return;

    char virt[600];
    int k = snprintf(virt, sizeof virt, "/proc/");
    for (i = hit; i < n && file[i] && k < (int)sizeof virt - 1; i++)
        virt[k++] = file[i] == '\\' ? '/' : (file[i] < 128 ? (char)file[i] : '_');
    virt[k] = 0;

    pthread_mutex_lock(&pc_lock);
    pc_busy = 1;
    struct node node;
    pc_parse(virt + 5, &node);
    pc_ensure_node(&node);
    pc_busy = 0;
    pthread_mutex_unlock(&pc_lock);
}
