// The root of the materialized tree, the lock every generation runs under,
// the pfs_buf the generators emit into, and the path model the rest of the
// carrier parses /proc paths with.

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
// pfs_buf

static void reserve(struct pfs_buf *b, size_t extra) {
    if (b->oom || b->n + extra <= b->cap) return;
    size_t cap = b->cap ? b->cap * 2 : 1024;
    while (cap < b->n + extra) cap *= 2;
    char *p = realloc(b->p, cap);
    if (!p) {
        b->oom = 1;
        return;
    }
    b->p = p;
    b->cap = cap;
}

void pfs_put(struct pfs_buf *b, const void *data, size_t n) {
    reserve(b, n);
    if (b->oom) return;
    memcpy(b->p + b->n, data, n);
    b->n += n;
}

void pfs_printf(struct pfs_buf *b, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    va_list ap2;
    va_copy(ap2, ap);
    int need = vsnprintf(0, 0, fmt, ap);
    va_end(ap);
    if (need > 0) {
        reserve(b, (size_t)need + 1);
        if (!b->oom) {
            vsnprintf(b->p + b->n, (size_t)need + 1, fmt, ap2);
            b->n += (size_t)need;
        }
    }
    va_end(ap2);
}

void pfs_buf_free(struct pfs_buf *b) {
    free(b->p);
    memset(b, 0, sizeof *b);
}

// ---------------------------------------------------------------------------
// The skeleton root, from the bandwhich-era implementation: a per-process
// tree named by pid, cleaned at exit, swept by age when an owner dies badly
// (NT recycles pids briskly and kill(pid,0) answers EPERM for the departed,
// so liveness is unknowable here; mtime is the test that works).

char pc_root[512];
size_t pc_rootlen;
pthread_mutex_t pc_lock = PTHREAD_MUTEX_INITIALIZER;
_Thread_local int pc_busy;

void pc_write_file(const char *path, const char *data, size_t n) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd == -1) return;
    for (size_t off = 0; off < n;) {
        long r = write(fd, data + off, n - off);
        if (r <= 0) break;
        off += (size_t)r;
    }
    close(fd);
}

void pc_rm_rf(const char *path) {
    DIR *d = opendir(path);
    if (d) {
        char sub[600];
        for (struct dirent *e; (e = readdir(d));) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            snprintf(sub, sizeof sub, "%s/%s", path, e->d_name);
            if (e->d_type == DT_DIR)
                pc_rm_rf(sub);
            else
                unlink(sub);
        }
        closedir(d);
    }
    rmdir(path);
}

static void cleanup(void) {
    if (pc_rootlen) pc_rm_rf(pc_root);
}

static void sweep_stale(const char *tmp) {
    DIR *d = opendir(tmp);
    if (!d) return;
    time_t now = time(0);
    char path[600];
    for (struct dirent *e; (e = readdir(d));) {
        if (strncmp(e->d_name, "rust-ape-proc-", 14)) continue;
        int n = snprintf(path, sizeof path, "%s/%s", tmp, e->d_name);
        if (n <= 0 || (size_t)n >= sizeof path) continue;
        if (!strcmp(path, pc_root)) continue;
        struct stat st;
        if (stat(path, &st) == -1 || !S_ISDIR(st.st_mode)) continue;
        if (now - st.st_mtime > STALE_SECS) pc_rm_rf(path);
    }
    closedir(d);
}

bool pc_ensure_root(void) {
    if (pc_rootlen) return true;
    const char *tmp = getenv("TMPDIR");
    if (!tmp || !*tmp) tmp = getenv("TEMP");
    if (!tmp || !*tmp) tmp = getenv("TMP");
    // A program that empties its environment before touching /proc still
    // needs a root. cosmo's answer comes from the NT temp path, which the
    // system keeps regardless of what environ says.
    if (!tmp || !*tmp) tmp = __get_tmpdir();
    if (!tmp || !*tmp) return false;
    char base[sizeof pc_root];
    size_t len = strlcpy(base, tmp, sizeof base);
    if (len >= sizeof base) return false;
    while (len > 1 && base[len - 1] == '/') base[--len] = 0;
    tmp = base;
    int n = snprintf(pc_root, sizeof pc_root, "%s/rust-ape-proc-%d", tmp,
                     (int)getpid());
    if (n <= 0 || (size_t)n >= sizeof pc_root) return false;
    pc_rm_rf(pc_root); // a stale tree from a crashed run of this same pid
    sweep_stale(tmp);
    if (mkdir(pc_root, 0755) == -1 && errno != EEXIST) return false;
    pc_rootlen = (size_t)n;
    atexit(cleanup);
    return true;
}

// ---------------------------------------------------------------------------
// Path model. `sub` is what follows "/proc" -- "" or "/...". Parsing also
// resolves the self alias, so every physical path it yields already names
// the real pid and openat through a dirfd of it just works.

static size_t comp(const char *s, char *out, size_t cap) {
    size_t i = 0;
    while (s[i] && s[i] != '/') {
        if (i < cap - 1) out[i] = s[i];
        i++;
    }
    out[i < cap - 1 ? i : cap - 1] = 0;
    return i;
}

void pc_parse(const char *sub, struct node *n) {
    memset(n, 0, sizeof *n);
    while (*sub == '/') sub++;
    if (!*sub) {
        n->kind = K_ROOT;
        return;
    }
    char first[64];
    size_t len = comp(sub, first, sizeof first);
    const char *rest = sub + len;
    while (*rest == '/') rest++;

    if (!strcmp(first, "self")) {
        n->was_self = true;
        n->pid = pfs_self_pid();
    } else if (first[0] >= '0' && first[0] <= '9') {
        n->pid = (uint32_t)strtoul(first, 0, 10);
        // the spoofed cosmo pid (see pfs_self_pid) is an alias of ourselves
        if (n->pid == (uint32_t)getpid()) {
            n->was_self = true;
            n->pid = pfs_self_pid();
        }
    } else if (!strcmp(first, "net")) {
        if (!*rest) {
            n->kind = K_NET_DIR;
        } else {
            n->kind = K_NET_FILE;
            comp(rest, n->name, sizeof n->name);
        }
        return;
    } else if (!strcmp(first, "sys")) {
        n->kind = K_SYS;
        snprintf(n->rest, sizeof n->rest, "%s", rest);
        return;
    } else {
        n->kind = *rest ? K_OTHER : K_TOP;
        snprintf(n->name, sizeof n->name, "%s", first);
        return;
    }

    if (!*rest) {
        n->kind = K_PID_DIR;
        return;
    }
    n->kind = K_PID_SUB;
    size_t nl = comp(rest, n->name, sizeof n->name);
    const char *deeper = rest + nl;
    while (*deeper == '/') deeper++;
    snprintf(n->rest, sizeof n->rest, "%s", deeper);
}

// The physical suffix under pc_root for a parsed path.
void pc_phys_suffix(const struct node *n, const char *sub, char *out,
                        size_t cap) {
    if (n->pid || n->was_self) {
        // rebuild so "self" becomes the pid
        const char *after = sub;
        while (*after == '/') after++;
        while (*after && *after != '/') after++; // skip first component
        snprintf(out, cap, "/%u%s", n->pid, after);
    } else {
        snprintf(out, cap, "%s", *sub ? sub : "/");
    }
}

// ---------------------------------------------------------------------------

uint64_t pc_fnv64(const void *data, size_t n, uint64_t h) {
    const uint8_t *b = data;
    for (size_t i = 0; i < n; i++) {
        h ^= b[i];
        h *= 0x100000001b3ull;
    }
    return h;
}
