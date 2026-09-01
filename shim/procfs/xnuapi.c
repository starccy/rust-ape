// XNU plumbing shared by the /proc generators, the counterpart of ntapi.c.
// Everything reaches libSystem through the ape loader's Syslib table, the
// only ABI Apple keeps stable: sysctl by name through its v10 entries, and
// the libproc and mach functions resolved once with its v6 dlsym. Raw
// syscalls are never used; when the table is too old the whole layer
// declines and /proc simply does not exist.

#define _COSMO_SOURCE // for libc/dce.h and the syslib table

#include <errno.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/utsname.h>
#include <time.h>
#include <unistd.h>
#include <libc/dce.h>
#include <libc/runtime/clktck.h>
#include <libc/runtime/runtime.h>
#include <libc/runtime/syslib.internal.h>

#include "procfs.h"
#include "xnu.h"

#define XNU_RTLD_NOW 2
#define XNU_PROC_ALL_PIDS 1
#define XNU_PROC_PIDTBSDINFO 3
#define XNU_PROC_PIDTASKINFO 4
#define XNU_RUSAGE_INFO_V2 2
#define XNU_PROC_PIDPATHINFO_MAXSIZE 4096
#define XNU_HOST_VM_INFO64 4
#define XNU_PROCESSOR_CPU_LOAD_INFO 2
#define XNU_CTL_KERN 1
#define XNU_KERN_ARGMAX 8
#define XNU_KERN_PROCARGS2 49
#define XNU_CTL_NET 4
#define XNU_PF_ROUTE 17
#define XNU_NET_RT_IFLIST2 6
#define XNU_PROC_PIDLISTFDS 1
#define XNU_PROC_PIDFDVNODEPATHINFO 2
#define XNU_PROC_PIDFDSOCKETINFO 3
#define XNU_PROC_PIDLISTTHREADS 6
#define XNU_PROC_PIDVNODEPATHINFO 9
#define XNU_SOCKINFO_IN 1
#define XNU_SOCKINFO_TCP 2
#define XNU_SOCK_DGRAM 2
#define XNU_INI_IPV6 2

struct xnu_timebase {
    uint32_t numer, denom;
};

// The resolved entry points. All of libproc and mach is re-exported by
// libSystem, so one dlopen answers everything.
static struct {
    bool tried, ok;
    int (*listpids)(uint32_t, uint32_t, void *, int);
    int (*pidinfo)(int, int, uint64_t, void *, int);
    int (*pidpath)(int, void *, uint32_t);
    int (*pidfdinfo)(int, int, int, void *, int);
    int (*pid_rusage)(int, int, void *);
    int (*timebase)(struct xnu_timebase *);
    uint32_t (*host_self)(void);
    int (*host_stats64)(uint32_t, int, void *, unsigned *);
    int (*host_cpu_info)(uint32_t, int, uint32_t *, int **, uint32_t *);
    int (*vm_dealloc)(uint32_t, uintptr_t, size_t);
    int (*region_recurse)(uint32_t, uint64_t *, uint64_t *, uint32_t *,
                          void *, uint32_t *);
    int (*regionfile)(int, uint64_t, void *, uint32_t);
    uint32_t *task_self;
    uint32_t tb_numer, tb_denom;
} g;

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;

static bool init_locked(void) {
    if (g.tried) return g.ok;
    g.tried = true;
    // dlsym arrived in v6, sysctl in v10; both are wanted, and any loader
    // recent enough to matter has both.
    if (!IsXnuSilicon() || !__syslib || __syslib->__version < 10) return false;
    void *h = __syslib->__dlopen("/usr/lib/libSystem.B.dylib", XNU_RTLD_NOW);
    if (!h) return false;
    g.listpids = __syslib->__dlsym(h, "proc_listpids");
    g.pidinfo = __syslib->__dlsym(h, "proc_pidinfo");
    g.pidpath = __syslib->__dlsym(h, "proc_pidpath");
    g.pidfdinfo = __syslib->__dlsym(h, "proc_pidfdinfo");
    g.pid_rusage = __syslib->__dlsym(h, "proc_pid_rusage");
    g.timebase = __syslib->__dlsym(h, "mach_timebase_info");
    g.host_self = __syslib->__dlsym(h, "mach_host_self");
    g.host_stats64 = __syslib->__dlsym(h, "host_statistics64");
    g.host_cpu_info = __syslib->__dlsym(h, "host_processor_info");
    g.vm_dealloc = __syslib->__dlsym(h, "vm_deallocate");
    g.region_recurse = __syslib->__dlsym(h, "mach_vm_region_recurse");
    g.regionfile = __syslib->__dlsym(h, "proc_regionfilename");
    g.task_self = __syslib->__dlsym(h, "mach_task_self_");
    g.tb_numer = g.tb_denom = 1;
    struct xnu_timebase tb;
    if (g.timebase && !g.timebase(&tb) && tb.denom) {
        g.tb_numer = tb.numer;
        g.tb_denom = tb.denom;
    }
    g.ok = g.listpids && g.pidinfo;
    return g.ok;
}

bool pfs_xnu_ready(void) {
    pthread_mutex_lock(&g_lock);
    bool r = init_locked();
    pthread_mutex_unlock(&g_lock);
    return r;
}

long pfs_xnu_sysctl(const char *name, void *buf, size_t *len) {
    if (!pfs_xnu_ready()) return -ENOSYS;
    return __syslib->__sysctlbyname(name, buf, len, 0, 0);
}

uint64_t pfs_xnu_mach_ns(uint64_t mach) {
    pfs_xnu_ready();
    return mach * g.tb_numer / g.tb_denom;
}

uint64_t pfs_xnu_boottime(void) {
    static uint64_t boot;
    if (!boot) {
        struct {
            int64_t sec;
            int32_t usec, pad;
        } tv = {0};
        size_t len = sizeof tv;
        if (pfs_xnu_sysctl("kern.boottime", &tv, &len) == 0 && tv.sec > 0)
            boot = (uint64_t)tv.sec;
    }
    return boot;
}

long pfs_xnu_pagesize(void) {
    static long pg;
    if (!pg) {
        pg = sysconf(_SC_PAGESIZE);
        if (pg <= 0) pg = 16384;
    }
    return pg;
}

int pfs_xnu_hz(void) {
    static int hz;
    if (!hz) {
        hz = (int)__clk_tck();
        if (hz <= 0) hz = 100;
    }
    return hz;
}

// ---------------------------------------------------------------------------
// Per-pid queries. Buffers carry their expected size and short answers are
// treated as absent, the negotiation these unstable layouts are built for.

bool pfs_xnu_bsdinfo(uint32_t pid, struct xnu_proc_bsdinfo *out) {
    if (!pfs_xnu_ready()) return false;
    return g.pidinfo((int)pid, XNU_PROC_PIDTBSDINFO, 0, out, sizeof *out) ==
           (int)sizeof *out;
}

bool pfs_xnu_taskinfo(uint32_t pid, struct xnu_proc_taskinfo *out) {
    if (!pfs_xnu_ready()) return false;
    return g.pidinfo((int)pid, XNU_PROC_PIDTASKINFO, 0, out, sizeof *out) ==
           (int)sizeof *out;
}

bool pfs_xnu_rusage(uint32_t pid, struct xnu_rusage_info_v2 *out) {
    if (!pfs_xnu_ready() || !g.pid_rusage) return false;
    memset(out, 0, sizeof *out);
    return g.pid_rusage((int)pid, XNU_RUSAGE_INFO_V2, out) == 0;
}

long pfs_xnu_pidpath(uint32_t pid, char *buf, size_t n) {
    if (!pfs_xnu_ready() || !g.pidpath) return -1;
    // the call rejects buffers larger than PROC_PIDPATHINFO_MAXSIZE
    char tmp[XNU_PROC_PIDPATHINFO_MAXSIZE];
    int r = g.pidpath((int)pid, tmp, sizeof tmp);
    if (r <= 0) return -1;
    if ((size_t)r > n) r = (int)n;
    memcpy(buf, tmp, (size_t)r);
    return r;
}

bool pfs_xnu_vmstat(struct xnu_vm_statistics64 *out) {
    if (!pfs_xnu_ready() || !g.host_stats64 || !g.host_self) return false;
    unsigned count = sizeof *out / 4;
    memset(out, 0, sizeof *out);
    return g.host_stats64(g.host_self(), XNU_HOST_VM_INFO64, out, &count) == 0;
}

int pfs_xnu_cpu_ticks(struct xnu_cpu_ticks *out, int cap) {
    if (!pfs_xnu_ready() || !g.host_cpu_info || !g.host_self) return 0;
    uint32_t ncpu = 0, cnt = 0;
    int *info = 0;
    if (g.host_cpu_info(g.host_self(), XNU_PROCESSOR_CPU_LOAD_INFO, &ncpu,
                        &info, &cnt) != 0 ||
        !info)
        return 0;
    int n = (int)ncpu;
    if (n > cap) n = cap;
    for (int i = 0; i < n; i++) {
        // CPU_STATE_USER..CPU_STATE_NICE, <mach/machine.h>
        out[i].user = (uint32_t)info[i * 4 + 0];
        out[i].system = (uint32_t)info[i * 4 + 1];
        out[i].idle = (uint32_t)info[i * 4 + 2];
        out[i].nice = (uint32_t)info[i * 4 + 3];
    }
    if (g.vm_dealloc && g.task_self)
        g.vm_dealloc(*g.task_self, (uintptr_t)info, (size_t)cnt * 4);
    return n;
}

// The lock-free body, for callers that already hold g_lock (the snapshot
// corrects loader comms while it runs).
static bool raw_procargs(uint32_t pid, char **out, size_t *len) {
    static int argmax;
    if (!argmax) {
        size_t l = sizeof argmax;
        if (__syslib->__sysctlbyname("kern.argmax", &argmax, &l, 0, 0) != 0 ||
            argmax <= 0)
            argmax = 256 * 1024;
    }
    char *buf = malloc((size_t)argmax);
    if (!buf) return false;
    int mib[3] = {XNU_CTL_KERN, XNU_KERN_PROCARGS2, (int)pid};
    size_t n = (size_t)argmax;
    if (__syslib->__sysctl(mib, 3, buf, &n, 0, 0) != 0) {
        free(buf);
        return false;
    }
    *out = buf;
    *len = n;
    return true;
}

bool pfs_xnu_procargs(uint32_t pid, char **out, size_t *len) {
    if (!pfs_xnu_ready()) return false;
    return raw_procargs(pid, out, len);
}

int pfs_xnu_threads(uint32_t pid, uint64_t *out, int cap) {
    if (!pfs_xnu_ready()) return 0;
    int r = g.pidinfo((int)pid, XNU_PROC_PIDLISTTHREADS, 0, out,
                      cap * (int)sizeof *out);
    return r > 0 ? r / (int)sizeof *out : 0;
}

long pfs_xnu_cwd(uint32_t pid, char *buf, size_t n) {
    if (!pfs_xnu_ready()) return -1;
    static _Thread_local struct xnu_proc_vnodepathinfo vi;
    int r = g.pidinfo((int)pid, XNU_PROC_PIDVNODEPATHINFO, 0, &vi, sizeof vi);
    if (r != (int)sizeof vi) return -1;
    size_t l = strnlen(vi.pvi_cdir.vip_path, sizeof vi.pvi_cdir.vip_path);
    if (!l) return -1;
    if (l > n) l = n;
    memcpy(buf, vi.pvi_cdir.vip_path, l);
    return (long)l;
}

bool pfs_xnu_fdsock(uint32_t pid, int fd, struct xnu_sock_id *out) {
    if (!pfs_xnu_ready() || !g.pidfdinfo) return false;
    // headroom past our copy: the kernel writes its full socket_fdinfo,
    // whose trailing union covers protocols nothing here reads
    static _Thread_local union {
        struct xnu_socket_fdinfo si;
        char pad[3072];
    } u;
    int r = g.pidfdinfo((int)pid, fd, XNU_PROC_PIDFDSOCKETINFO, &u, sizeof u);
    if (r < (int)sizeof u.si) return false;
    const struct xnu_socket_fdinfo *s = &u.si;
    if (s->psi.soi_kind != XNU_SOCKINFO_IN &&
        s->psi.soi_kind != XNU_SOCKINFO_TCP)
        return false;
    const struct xnu_in_sockinfo *in = &s->psi.soi_proto.pri_in;
    bool v6 = (in->insi_vflag & XNU_INI_IPV6) != 0;
    memset(out, 0, sizeof *out);
    out->proto = s->psi.soi_type == XNU_SOCK_DGRAM ? 1 : 0;
    out->family = v6 ? 6 : 4;
    out->lport = __builtin_bswap16((uint16_t)in->insi_lport);
    out->rport = __builtin_bswap16((uint16_t)in->insi_fport);
    if (v6) {
        memcpy(out->laddr, in->insi_laddr.a6, 16);
        memcpy(out->raddr, in->insi_faddr.a6, 16);
    } else {
        memcpy(out->laddr, &in->insi_laddr.a4.addr4, 4);
        memcpy(out->raddr, &in->insi_faddr.a4.addr4, 4);
    }
    return true;
}

// ---------------------------------------------------------------------------
// The ape loader disguise. The kernel names a loader-run process after the
// loader binary; the program it runs is argv[1] of the exec (one further
// along in the "ape - prog argv0" spelling). Resolved once per incarnation.

bool pfs_xnu_is_loader(const char *path) {
    const char *b = strrchr(path, '/');
    b = b ? b + 1 : path;
    return !strncmp(b, ".ape-", 5) || !strcmp(b, "ape");
}

struct ape_prog {
    uint32_t pid;
    uint64_t start;
    int len; // -1: known to not run under the loader
    char prog[512];
};
static struct ape_prog g_ape[64];
static pthread_mutex_t g_ape_lock = PTHREAD_MUTEX_INITIALIZER;

long pfs_xnu_ape_program(uint32_t pid, char *buf, size_t n) {
    if (!g.ok) return -1; // set once at init; a stale read only declines
    struct xnu_proc_bsdinfo bi;
    if (g.pidinfo((int)pid, XNU_PROC_PIDTBSDINFO, 0, &bi, sizeof bi) !=
        (int)sizeof bi)
        return -1;
    struct ape_prog *c = &g_ape[pid % 64];
    pthread_mutex_lock(&g_ape_lock);
    if (c->pid == pid && c->start == bi.pbi_start_tvsec) {
        long r = -1;
        if (c->len > 0) {
            r = (size_t)c->len < n ? c->len : (long)n;
            memcpy(buf, c->prog, (size_t)r);
        }
        pthread_mutex_unlock(&g_ape_lock);
        return r;
    }
    pthread_mutex_unlock(&g_ape_lock);

    // resolve outside the cache lock; the work is idempotent
    int len = -1;
    char prog[512] = "";
    char path[XNU_PROC_PIDPATHINFO_MAXSIZE];
    int pr = g.pidpath ? g.pidpath((int)pid, path, sizeof path) : 0;
    if (pr > 0) {
        path[pr < (int)sizeof path ? pr : (int)sizeof path - 1] = 0;
        if (pfs_xnu_is_loader(path)) {
            char *raw;
            size_t rl;
            if (raw_procargs(pid, &raw, &rl)) {
                if (rl > 4) {
                    int argc;
                    memcpy(&argc, raw, 4);
                    const char *p = raw + 4, *end = raw + rl;
                    while (p < end && *p) p++; // the exec path
                    while (p < end && !*p) p++;
                    const char *argv[3] = {0};
                    for (int k = 0; k < argc && k < 3 && p < end; k++) {
                        argv[k] = p;
                        p += strnlen(p, (size_t)(end - p));
                        if (p < end) p++;
                    }
                    // an exec that names the program keeps it in argv[0];
                    // the script header spells the loader there and the
                    // program one along
                    const char *cand = 0;
                    if (argc > 0 && argv[0] && !pfs_xnu_is_loader(argv[0])) {
                        cand = argv[0];
                    } else {
                        cand = argc > 1 ? argv[1] : 0;
                        if (cand && !strcmp(cand, "-"))
                            cand = argc > 2 ? argv[2] : 0;
                    }
                    if (cand && cand[0]) {
                        snprintf(prog, sizeof prog, "%s", cand);
                        len = (int)strlen(prog);
                    }
                }
                free(raw);
            }
        }
    }

    pthread_mutex_lock(&g_ape_lock);
    c->pid = pid;
    c->start = bi.pbi_start_tvsec;
    c->len = len;
    memcpy(c->prog, prog, sizeof prog);
    pthread_mutex_unlock(&g_ape_lock);
    if (len <= 0) return -1;
    long r = (size_t)len < n ? len : (long)n;
    memcpy(buf, prog, (size_t)r);
    return r;
}

int pfs_xnu_listfds(uint32_t pid, struct xnu_proc_fdinfo *out, int cap) {
    if (!pfs_xnu_ready()) return 0;
    int r = g.pidinfo((int)pid, XNU_PROC_PIDLISTFDS, 0, out,
                      cap * (int)sizeof *out);
    return r > 0 ? r / (int)sizeof *out : 0;
}

long pfs_xnu_fdpath(uint32_t pid, int fd, char *buf, size_t n) {
    if (!pfs_xnu_ready() || !g.pidfdinfo) return -1;
    static _Thread_local struct xnu_vnode_fdinfowithpath vi;
    int r = g.pidfdinfo((int)pid, fd, XNU_PROC_PIDFDVNODEPATHINFO, &vi,
                        sizeof vi);
    if (r != (int)sizeof vi) return -1;
    size_t l = strnlen(vi.vip_path, sizeof vi.vip_path);
    if (l > n) l = n;
    memcpy(buf, vi.vip_path, l);
    return (long)l;
}

bool pfs_xnu_iflist2(char **out, size_t *len) {
    if (!pfs_xnu_ready()) return false;
    int mib[6] = {XNU_CTL_NET, XNU_PF_ROUTE, 0, 0, XNU_NET_RT_IFLIST2, 0};
    for (int tries = 0; tries < 3; tries++) {
        size_t n = 0;
        if (__syslib->__sysctl(mib, 6, 0, &n, 0, 0) != 0 || !n) return false;
        char *buf = malloc(n + n / 4);
        if (!buf) return false;
        size_t cap = n + n / 4;
        if (__syslib->__sysctl(mib, 6, buf, &cap, 0, 0) == 0) {
            *out = buf;
            *len = cap;
            return true;
        }
        free(buf); // the list grew between the calls
    }
    return false;
}

bool pfs_xnu_pcblist(const char *name, char **out, size_t *len) {
    if (!pfs_xnu_ready()) return false;
    for (int tries = 0; tries < 3; tries++) {
        size_t n = 0;
        if (__syslib->__sysctlbyname(name, 0, &n, 0, 0) != 0 || !n)
            return false;
        char *buf = malloc(n + n / 4);
        if (!buf) return false;
        size_t cap = n + n / 4;
        if (__syslib->__sysctlbyname(name, buf, &cap, 0, 0) == 0) {
            *out = buf;
            *len = cap;
            return true;
        }
        free(buf);
    }
    return false;
}

bool pfs_xnu_region(uint64_t *addr, uint64_t *size, uint32_t *depth,
                    struct xnu_vm_region_submap_info_64 *info) {
    if (!pfs_xnu_ready() || !g.region_recurse || !g.task_self) return false;
    uint32_t cnt = sizeof *info / 4;
    return g.region_recurse(*g.task_self, addr, size, depth, info, &cnt) == 0;
}

long pfs_xnu_regionfile(uint64_t addr, char *buf, size_t n) {
    if (!pfs_xnu_ready() || !g.regionfile) return -1;
    int r = g.regionfile(getpid(), addr, buf, (uint32_t)n);
    return r > 0 ? r : -1;
}

// ---------------------------------------------------------------------------
// The snapshot: the pid list plus one bsdinfo per process, cached like the
// NT one so a sweep asks the kernel once per window however many files it
// reads. Thread ids are not enumerated here; task/ pretends one thread per
// process until someone needs better.

#define XNU_SNAP_MS 200
#define XNU_MAX_PROCS 2048

static struct pfs_proc g_procs[XNU_MAX_PROCS];
static int g_nprocs;
static int64_t g_snap_ms;

static void set_comm(struct pfs_proc *p, const char *name) {
    int j = 0;
    for (; name[j] && j < (int)sizeof p->comm - 1; j++) {
        char c = name[j];
        p->comm[j] =
            (c < 32 || c > 126 || c == '(' || c == ')' || c == ' ') ? '_' : c;
    }
    p->comm[j] = 0;
    if (!j) snprintf(p->comm, sizeof p->comm, "%u", p->pid);
}

static void snap_locked(void) {
    int64_t t = pfs_now_ms();
    if (g_snap_ms && t - g_snap_ms < XNU_SNAP_MS) return;
    if (!init_locked()) return;
    static int pids[XNU_MAX_PROCS];
    int bytes = g.listpids(XNU_PROC_ALL_PIDS, 0, pids, (int)sizeof pids);
    int n = bytes > 0 ? bytes / (int)sizeof(int) : 0;
    g_nprocs = 0;
    for (int i = 0; i < n && g_nprocs < XNU_MAX_PROCS; i++) {
        if (pids[i] <= 0) continue;
        struct xnu_proc_bsdinfo bi;
        if (g.pidinfo(pids[i], XNU_PROC_PIDTBSDINFO, 0, &bi, sizeof bi) !=
            (int)sizeof bi)
            continue; // gone, or not ours to see
        struct pfs_proc *p = &g_procs[g_nprocs++];
        memset(p, 0, sizeof *p);
        p->pid = (uint32_t)pids[i];
        p->ppid = bi.pbi_ppid;
        p->threads = 1;
        set_comm(p, bi.pbi_name[0] ? bi.pbi_name : bi.pbi_comm);
        // a loader-run process carries the loader's name; answer with the
        // program's, like the self entry below and the NT side do
        if (!strncmp(p->comm, ".ape-", 5) || !strcmp(p->comm, "ape")) {
            char prog[512];
            long r = pfs_xnu_ape_program(p->pid, prog, sizeof prog - 1);
            if (r > 0) {
                prog[r] = 0;
                const char *base = strrchr(prog, '/');
                set_comm(p, base && base[1] ? base + 1 : prog);
            }
        }
    }
    // The kernel names this process after the ape loader; answer with the
    // program it ran, like the NT side does.
    for (int i = 0; i < g_nprocs; i++) {
        if (g_procs[i].pid != (uint32_t)getpid()) continue;
        const char *exe = GetProgramExecutableName();
        const char *base = exe ? strrchr(exe, '/') : 0;
        if (base && base[1]) set_comm(&g_procs[i], base + 1);
        break;
    }
    g_snap_ms = pfs_now_ms();
}

int pfs_xnu_procs(const struct pfs_proc **out) {
    pthread_mutex_lock(&g_lock);
    snap_locked();
    pthread_mutex_unlock(&g_lock);
    *out = g_procs;
    return g_nprocs;
}

const struct pfs_proc *pfs_xnu_proc_find(uint32_t pid) {
    const struct pfs_proc *p;
    int n = pfs_xnu_procs(&p);
    for (int i = 0; i < n; i++)
        if (p[i].pid == pid) return &p[i];
    return 0;
}

// ---------------------------------------------------------------------------
// Kernel identity, aligned with what uname() answers on this host, the same
// rule the NT side follows.

static struct utsname g_uts;
static bool g_uts_have;

static void kernel_identity(void) {
    if (g_uts_have) return;
    if (uname(&g_uts)) memset(&g_uts, 0, sizeof g_uts);
    g_uts_have = true;
}

const char *pfs_xnu_kernel_sysname(void) {
    kernel_identity();
    return g_uts.sysname[0] ? g_uts.sysname : "Darwin";
}

const char *pfs_xnu_kernel_release(void) {
    kernel_identity();
    return g_uts.release[0] ? g_uts.release : "0.0.0";
}

const char *pfs_xnu_kernel_version(void) {
    kernel_identity();
    return g_uts.version;
}
