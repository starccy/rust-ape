// A server's share list as a directory, on NT.
//
// NT has no "\\server" directory, so //server (the Path::parent of a
// share root, and what a file picker or `ls ..` reaches from one) was
// ENOENT. The shares are enumerated over the network instead, and a
// directory holding one empty subdirectory per share is materialized
// under the temp directory; mkntpath.c diverts the "//server" spelling
// there, so stat, opendir and readdir all see a plain directory. Only a
// path that names the server and nothing else is diverted, so real
// share paths are untouched. Special shares (IPC$, ADMIN$, drive
// letters) are left out, as is anything that is not a disk share. An
// unreachable name costs a network lookup before it fails.
//
// The directory can be the cwd (cd .. from a share root lands there),
// which needs two more pieces: getcwd reports it as "//server", and a
// relative path resolved from inside it is joined back onto "//server"
// so a share name reaches the real share rather than its empty
// placeholder. The same join serves ".." from a share root, which NT
// would otherwise clamp at the root.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include <dirent.h>
#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <libc/calls/syscall_support-nt.internal.h>
#include <libc/dce.h>
#include <libc/nt/createfile.h>
#include <libc/nt/enum/accessmask.h>
#include <libc/nt/enum/creationdisposition.h>
#include <libc/nt/enum/fileflagandattributes.h>
#include <libc/nt/enum/filesharemode.h>
#include <libc/nt/files.h>
#include <libc/nt/runtime.h>
#include <libc/nt/thunk/msabi.h>
#include <libc/runtime/runtime.h>
#include <libc/str/str.h>

// shim/procfs/ntapi.c
void *pfs_sym(const char16_t *dll, const char *name);
// shim/mkntpath.c
int __ape_shim_unc_any(void);

#define STALE_SECS 3600

struct share_info_1 {
    char16_t *netname;
    uint32_t type;
    char16_t *remark;
};

typedef uint32_t (*__msabi NetShareEnumF)(char16_t *, uint32_t, uint8_t **,
                                          uint32_t, uint32_t *, uint32_t *,
                                          uint32_t *);
typedef uint32_t (*__msabi NetApiBufferFreeF)(void *);

static pthread_mutex_t us_lock = PTHREAD_MUTEX_INITIALIZER;
static char us_root[512];
static size_t us_rootlen;
static char us_root_unix[512];  // the same as getcwd would spell it
static size_t us_root_unixlen;

static int IsAlpha(int c) {
    return ('A' <= c && c <= 'Z') || ('a' <= c && c <= 'z');
}

// A win32 path in getcwd's unix spelling: prefixes dropped, "C:" as
// "/C", slashes forward.
static void nt_to_unix(const char16_t *p16, char *out, size_t outsz) {
    char tmp[PATH_MAX];
    if (tprecode16to8(tmp, sizeof tmp, p16).ax >= sizeof tmp - 1) {
        out[0] = 0;
        return;
    }
    char *p = tmp;
    if (!strncmp(p, "\\\\?\\UNC\\", 8)) {
        p += 6;
        p[0] = '\\';
        p[1] = '\\';
    } else if (!strncmp(p, "\\\\?\\", 4) && IsAlpha(p[4]) && p[5] == ':') {
        p += 4;
    }
    if (IsAlpha(p[0]) && p[1] == ':' && p[2] == '\\') {
        p[1] = p[0];
        p[0] = '\\';
    }
    size_t i;
    for (i = 0; p[i] && i + 1 < outsz; i++) out[i] = p[i] == '\\' ? '/' : p[i];
    out[i] = 0;
}

// Whether a unix-spelled path is inside the materialized tree; rest then
// points at "server[/share/...]".
static bool under_root(const char *path, const char **rest) {
    size_t n = us_root_unixlen;
    if (!n || strncasecmp(path, us_root_unix, n) || path[n] != '/' ||
        !path[n + 1])
        return false;
    *rest = path + n + 1;
    return true;
}

static void rm_rf(const char *path) {
    DIR *d = opendir(path);
    if (d) {
        char sub[600];
        for (struct dirent *e; (e = readdir(d));) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            int n = snprintf(sub, sizeof sub, "%s/%s", path, e->d_name);
            if (n <= 0 || (size_t)n >= sizeof sub) continue;
            if (e->d_type == DT_DIR)
                rm_rf(sub);
            else
                unlink(sub);
        }
        closedir(d);
    }
    rmdir(path);
}

static void cleanup(void) {
    if (us_rootlen) rm_rf(us_root);
}

static void sweep_stale(const char *tmp) {
    DIR *d = opendir(tmp);
    if (!d) return;
    time_t now = time(0);
    char path[600];
    for (struct dirent *e; (e = readdir(d));) {
        if (strncmp(e->d_name, "rust-ape-unc-", 13)) continue;
        int n = snprintf(path, sizeof path, "%s/%s", tmp, e->d_name);
        if (n <= 0 || (size_t)n >= sizeof path) continue;
        if (!strcmp(path, us_root)) continue;
        struct stat st;
        if (stat(path, &st) == -1 || !S_ISDIR(st.st_mode)) continue;
        if (now - st.st_mtime > STALE_SECS) rm_rf(path);
    }
    closedir(d);
}

static bool ensure_root(void) {
    if (us_rootlen) return true;
    const char *tmp = getenv("TMPDIR");
    if (!tmp || !*tmp) tmp = getenv("TEMP");
    if (!tmp || !*tmp) tmp = getenv("TMP");
    if (!tmp || !*tmp) tmp = __get_tmpdir();
    if (!tmp || !*tmp) return false;
    char base[sizeof us_root];
    size_t len = strlcpy(base, tmp, sizeof base);
    if (len >= sizeof base) return false;
    while (len > 1 && base[len - 1] == '/') base[--len] = 0;
    int n = snprintf(us_root, sizeof us_root, "%s/rust-ape-unc-%d", base,
                     (int)getpid());
    if (n <= 0 || (size_t)n >= sizeof us_root) return false;
    rm_rf(us_root);
    sweep_stale(base);
    if (mkdir(us_root, 0755) == -1 && errno != EEXIST) return false;
    us_rootlen = (size_t)n;
    char16_t r16[PATH_MAX];
    if (__mkntpath(us_root, r16) > 0) {
        nt_to_unix(r16, us_root_unix, sizeof us_root_unix);
        us_root_unixlen = strlen(us_root_unix);
    }
    atexit(cleanup);
    return true;
}

// Materializes <root>/<server>/<share>/ for every disk share the server
// offers. False when the server cannot be enumerated.
static bool populate(const char *server, size_t n, const char *dir) {
    static NetShareEnumF enumf;
    static NetApiBufferFreeF freef;
    if (!enumf) {
        enumf = (NetShareEnumF)pfs_sym(u"netapi32.dll", "NetShareEnum");
        freef = (NetApiBufferFreeF)pfs_sym(u"netapi32.dll", "NetApiBufferFree");
    }
    if (!enumf || !freef) return false;
    char name8[260];
    if (n + 3 > sizeof name8) return false;
    name8[0] = '\\';
    name8[1] = '\\';
    memcpy(name8 + 2, server, n);
    name8[n + 2] = 0;
    char16_t name16[260];
    if (tprecode8to16(name16, sizeof name16 / sizeof *name16, name8).ax >=
        sizeof name16 / sizeof *name16 - 1)
        return false;
    // NetShareEnum reports an unreachable server by way of an RPC
    // exception, which cosmo's vectored handler turns into SIGSEGV before
    // the RPC runtime can catch it. The IPC$ share is reachable exactly
    // when the server is, and asking for its attributes is a plain call.
    char16_t ipc16[280];
    size_t nl = 0;
    while (name16[nl]) nl++;
    memcpy(ipc16, name16, nl * sizeof(char16_t));
    memcpy(ipc16 + nl, u"\\IPC$", 6 * sizeof(char16_t));
    int64_t h = CreateFile(ipc16, kNtFileReadAttributes,
                           kNtFileShareRead | kNtFileShareWrite |
                               kNtFileShareDelete,
                           0, kNtOpenExisting, kNtFileFlagBackupSemantics, 0);
    if (h == -1) return false;
    CloseHandle(h);
    uint8_t *buf = 0;
    uint32_t got = 0, total = 0, resume = 0;
    if (enumf(name16, 1, &buf, 0xffffffff, &got, &total, &resume)) return false;
    if (mkdir(dir, 0755) == -1 && errno != EEXIST) {
        freef(buf);
        return false;
    }
    const struct share_info_1 *si = (const struct share_info_1 *)buf;
    for (uint32_t i = 0; i < got; i++) {
        if (si[i].type & 0x80000000u) continue;  // special (IPC$, C$, ...)
        if (si[i].type & 3) continue;             // print, device, ipc
        char share8[260];
        if (tprecode16to8(share8, sizeof share8, si[i].netname).ax >=
            sizeof share8 - 1)
            continue;
        char sub[600];
        int len = snprintf(sub, sizeof sub, "%s/%s", dir, share8);
        if (len <= 0 || (size_t)len >= sizeof sub) continue;
        mkdir(sub, 0755);
    }
    freef(buf);
    return true;
}

// The materialized directory for "//server" (server is n bytes, not
// NUL-terminated), as a win32 path mkntpath accepts. 0 when the server
// has no listable shares.
int __ape_shim_unc_server_dir(const char *server, size_t n, char *out,
                              size_t outsz) {
    if (!IsWindows()) return 0;
    for (size_t i = 0; i < n; i++)
        if (server[i] == '/' || server[i] == '\\' || server[i] == ':') return 0;
    // A server's list is reused for a few seconds, since one directory walk
    // asks for it many times over and each answer is a network round trip.
    static struct { char name[256]; time_t when; bool ok; } cache[8];
    static int next;
    pthread_mutex_lock(&us_lock);
    int ok = 0;
    if (ensure_root()) {
        char dir[600];
        int len = snprintf(dir, sizeof dir, "%s/%.*s", us_root, (int)n, server);
        if (len > 0 && (size_t)len < sizeof dir) {
            time_t now = time(0);
            int slot = -1;
            for (int i = 0; i < 8; i++)
                if (cache[i].when && strlen(cache[i].name) == n &&
                    !strncasecmp(cache[i].name, server, n))
                    slot = i;
            if (slot >= 0 && now - cache[slot].when <= 5) {
                ok = cache[slot].ok;
            } else {
                ok = populate(server, n, dir);
                if (slot < 0 && n < sizeof cache[0].name) {
                    slot = next++ % 8;
                    memcpy(cache[slot].name, server, n);
                    cache[slot].name[n] = 0;
                }
                if (slot >= 0) {
                    cache[slot].when = now;
                    cache[slot].ok = ok;
                }
            }
            if (ok) ok = strlcpy(out, dir, outsz) < outsz;
        }
    }
    pthread_mutex_unlock(&us_lock);
    return ok;
}

// A cwd inside the materialized tree is really "//server[/...]", which
// is what getcwd should say. Rewrites buf in place; 0 when not there.
int __ape_shim_unc_cwd(char *buf, size_t size) {
    const char *rest;
    if (!us_root_unixlen || !under_root(buf, &rest)) return 0;
    size_t n = strlen(rest);
    if (n + 3 > size) return 0;
    memmove(buf + 2, rest, n + 1);
    buf[0] = '/';
    buf[1] = '/';
    return (int)n + 3;
}

static bool has_dotdot(const char *p) {
    while (*p) {
        while (*p == '/' || *p == '\\') p++;
        if (p[0] == '.' && p[1] == '.' && (!p[2] || p[2] == '/' || p[2] == '\\'))
            return true;
        while (*p && *p != '/' && *p != '\\') p++;
    }
    return false;
}

// A relative path joined onto the cwd, when NT would resolve it wrong:
// from a share, ".." must be able to reach the server directory (NT
// clamps it at the share root), and from the materialized server
// directory every name must map to the real share, not the empty
// placeholder. Returns 0 when NT's own resolution is right, which is
// every process that never saw a share.
int __ape_shim_unc_rel(const char *path, char *out, size_t outsz) {
    if (!IsWindows()) return 0;
    if (!us_root_unixlen && !__ape_shim_unc_any()) return 0;
    char16_t cwd16[PATH_MAX];
    uint32_t n = GetCurrentDirectory(PATH_MAX, cwd16);
    if (!n || n >= PATH_MAX) return 0;
    char cwd[PATH_MAX];
    nt_to_unix(cwd16, cwd, sizeof cwd);
    const char *rest;
    if (under_root(cwd, &rest)) {
        // "//server/rest/path" with "." and ".." resolved on a stack that
        // never pops the server, so ".." from the server directory stays
        // there (there is no "//" directory to climb into).
        size_t srv = 0;
        while (rest[srv] && rest[srv] != '/') srv++;
        if (srv + 3 > outsz) return 0;
        out[0] = '/';
        out[1] = '/';
        memcpy(out + 2, rest, srv);
        size_t o = srv + 2, base = o;
        const char *parts[2] = {rest + srv, path};
        bool trailing = false;
        for (int k = 0; k < 2; k++) {
            const char *p = parts[k];
            while (*p) {
                while (*p == '/' || *p == '\\') p++;
                const char *q = p;
                while (*q && *q != '/' && *q != '\\') q++;
                size_t len = (size_t)(q - p);
                trailing = len && (*q == '/' || *q == '\\');
                if (len == 1 && p[0] == '.') {
                } else if (len == 2 && p[0] == '.' && p[1] == '.') {
                    while (o > base && out[o - 1] != '/') o--;
                    if (o > base) o--;
                } else if (len) {
                    if (o + len + 2 > outsz) return 0;
                    out[o++] = '/';
                    memcpy(out + o, p, len);
                    o += len;
                }
                p = q;
            }
        }
        if (trailing && o > base && o + 1 < outsz) out[o++] = '/';
        out[o] = 0;
        return 1;
    }
    if (cwd[0] == '/' && cwd[1] == '/' && has_dotdot(path)) {
        int len = snprintf(out, outsz, "%s/%s", cwd, path);
        return len > 0 && (size_t)len < outsz;
    }
    return 0;
}
