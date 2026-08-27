// NT plumbing shared by the /proc generators. It covers dynamic symbol
// resolution for the handful of APIs cosmo doesn't import, one cached
// toolhelp snapshot (processes and threads together) answering every
// question about what exists, and the time base. Nothing here writes
// files or knows about paths.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/utsname.h>
#include <libc/str/str.h> // strcmp16
#include <libc/dce.h>
#include <libc/runtime/clktck.h>
#include <libc/nt/dll.h>
#include <libc/nt/ntdll.h>
#include <libc/nt/enum/systeminformationclass.h>
#include <libc/nt/struct/systemprocessinformation.h>
#include <libc/nt/struct/systemthreads.h>
#include <libc/nt/enum/processaccess.h>
#include <libc/nt/enum/th32cs.h>
#include <libc/nt/process.h>
#include <libc/nt/runtime.h>
#include <libc/nt/struct/processentry32.h>
#include <libc/nt/synchronization.h>

#include "procfs.h"

#define PFS_SNAP_MS 200
#define MAX_PROCS 1024
#define MAX_THREADS 8192

// ---------------------------------------------------------------------------
// Dynamic symbols. A tiny cache keyed by DLL name; misses stay cached as
// loaded modules, lookups themselves are cheap enough to repeat.

void *pfs_sym(const char16_t *dll, const char *name) {
    static struct { const char16_t *dll; int64_t mod; } cache[8];
    int64_t mod = 0;
    bool hit = false;
    for (int i = 0; i < 8; i++) {
        if (!cache[i].dll) {
            cache[i].dll = dll;
            cache[i].mod = LoadLibrary(dll);
        }
        if (cache[i].dll == dll || !strcmp16(cache[i].dll, dll)) {
            mod = cache[i].mod;
            hit = true;
            break;
        }
    }
    // A full cache must not turn into a missing symbol. A repeat load of
    // an already mapped module only bumps its reference count.
    if (!hit) mod = LoadLibrary(dll);
    if (!mod) return 0;
    return GetProcAddress(mod, name);
}

struct rtl_osversioninfo {
    uint32_t size, major, minor, build, platform;
    char16_t csd[128];
};
typedef int32_t (__msabi *RtlGetVersionF)(struct rtl_osversioninfo *);

static struct utsname g_uts;
static char g_release[80];

static void kernel_identity(void) {
    if (g_release[0]) return;
    if (uname(&g_uts)) memset(&g_uts, 0, sizeof g_uts);
    // RtlGetVersion answers the truth whatever the PE header's version
    // fields claim (cosmo's uname() hardcodes "10.0" for fear of the
    // GetVersionEx compatibility lie; verified 2026-08-25: RtlGetVersion,
    // the PEB, the registry and `cmd /c ver` all agree on 10.0.26200).
    struct rtl_osversioninfo vi = {.size = sizeof vi};
    RtlGetVersionF get = (RtlGetVersionF)pfs_sym(u"ntdll.dll", "RtlGetVersion");
    if (get && !get(&vi))
        snprintf(g_release, sizeof g_release, "%u.%u.%u", vi.major, vi.minor,
                 vi.build);
    else
        snprintf(g_release, sizeof g_release, "%s.0", g_uts.release);
}

const char *pfs_kernel_sysname(void) {
    kernel_identity();
    return g_uts.sysname[0] ? g_uts.sysname : "Windows";
}
const char *pfs_kernel_release(void) {
    kernel_identity();
    return g_release;
}
const char *pfs_kernel_version(void) {
    kernel_identity();
    return g_uts.version;
}

uint32_t pfs_self_pid(void) {
    return GetCurrentProcessId();
}

int64_t pfs_open_process(uint32_t pid) {
    int64_t h = OpenProcess(kNtProcessQueryLimitedInformation, false, pid);
    if (!h) h = OpenProcess(kNtProcessQueryInformation, false, pid);
    return h;
}

// ---------------------------------------------------------------------------
// The snapshot. Processes and threads come from the same walk so a caller
// asking both sees one instant.

struct threadent { uint32_t tid, pid; };

struct nt_threadentry32 { // cosmo has no header for this one
    uint32_t dwSize, cntUsage, th32ThreadID, th32OwnerProcessID;
    int32_t tpBasePri, tpDeltaPri;
    uint32_t dwFlags;
};

typedef bool32 (__msabi *Thread32F)(int64_t, struct nt_threadentry32 *);

static pthread_mutex_t g_snap_lock = PTHREAD_MUTEX_INITIALIZER;
static struct pfs_proc g_procs[MAX_PROCS];
static int g_nprocs;
static struct threadent g_threads[MAX_THREADS];
static int g_nthreads;
static int64_t g_snap_ms;

int64_t pfs_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

// comm lives inside parentheses and stat is split on spaces, so neither
// may appear in it
static void set_comm(struct pfs_proc *p, const char16_t *name, int len) {
    int j = 0;
    for (; j < len && name[j] && j < (int)sizeof p->comm - 1; j++) {
        char16_t c = name[j];
        p->comm[j] = (c < 32 || c > 126 || c == '(' || c == ')' || c == ' ')
                         ? '_'
                         : (char)c;
    }
    p->comm[j] = 0;
    if (!j) snprintf(p->comm, sizeof p->comm, "%u", p->pid);
}

// The whole process table in one kernel call: ids, names, thread ids and
// the counters that otherwise need an OpenProcess plus three queries per
// process, and it answers for protected processes that refuse a handle.
_Static_assert(sizeof(struct NtSystemProcessInformation) == 256,
               "SYSTEM_PROCESS_INFORMATION layout");
_Static_assert(sizeof(struct NtSystemThreads) == 80,
               "SYSTEM_THREAD_INFORMATION layout");

static bool snap_query_locked(void) {
    static char *buf;
    static uint32_t cap;
    for (int tries = 0;; tries++) {
        if (!buf) {
            if (!cap) cap = 512 * 1024;
            if (!(buf = malloc(cap))) return false;
        }
        uint32_t need = 0;
        NtStatus st = NtQuerySystemInformation(kNtSystemProcessInformation,
                                               buf, cap, &need);
        if (!st) break;
        free(buf);
        buf = 0;
        // STATUS_INFO_LENGTH_MISMATCH / STATUS_BUFFER_TOO_SMALL
        if ((uint32_t)st != 0xC0000004u && (uint32_t)st != 0xC0000023u)
            return false;
        if (tries == 3) return false;
        cap = (need > cap ? need : cap) + 256 * 1024;
    }

    g_nprocs = 0;
    g_nthreads = 0;
    for (char *q = buf;;) {
        struct NtSystemProcessInformation *e = (void *)q;
        uint32_t pid = (uint32_t)e->UniqueProcessId;
        if (g_nprocs < MAX_PROCS) {
            struct pfs_proc *p = &g_procs[g_nprocs++];
            memset(p, 0, sizeof *p);
            p->pid = pid;
            p->ppid = (uint32_t)e->InheritedFromUniqueProcessId;
            p->threads = e->NumberOfThreads;
            if (e->ImageName.Length && e->ImageName.Data) {
                set_comm(p, e->ImageName.Data, e->ImageName.Length / 2);
            } else {
                // what Toolhelp calls the idle process
                set_comm(p, pid ? u"" : u"[System Process]", pid ? 0 : 16);
            }
            p->facts = true;
            p->utime = (uint64_t)e->UserTime;
            p->stime = (uint64_t)e->KernelTime;
            p->create = (uint64_t)e->CreateTime;
            const struct NtVmCounters *vm = &e->VirtualMemoryCounters;
            p->vsize = vm->PagefileUsage ? vm->PagefileUsage : e->PrivatePageCount;
            p->rss = vm->WorkingSetSize;
            p->peak_vsize = vm->PeakPagefileUsage;
            p->peak_rss = vm->PeakWorkingSetSize;
            p->minflt = vm->PageFaultCount;
            p->io = e->IoCounters;
        }
        const struct NtSystemThreads *th = (void *)(q + sizeof *e);
        for (uint32_t i = 0; i < e->NumberOfThreads && g_nthreads < MAX_THREADS;
             i++) {
            g_threads[g_nthreads].tid = (uint32_t)(uintptr_t)th[i].ClientId.UniqueThread;
            g_threads[g_nthreads].pid = pid;
            g_nthreads++;
        }
        if (!e->NextEntryOffset) break;
        q += e->NextEntryOffset;
    }
    return true;
}

static void snap_refresh_locked(void) {
    int64_t t = pfs_now_ms();
    if (g_snap_ms && t - g_snap_ms < PFS_SNAP_MS) return;

    if (snap_query_locked()) {
        g_snap_ms = pfs_now_ms();
        return;
    }

    // Toolhelp, when the query is refused: no counters, so the pid files
    // fall back to per-process handles
    int64_t snap = CreateToolhelp32Snapshot(
        kNtTh32csSnapprocess | kNtTh32csSnapthread, 0);
    if (snap == -1) return;

    g_nprocs = 0;
    struct NtProcessEntry32 e;
    e.dwSize = sizeof e;
    for (bool32 ok = Process32First(snap, &e); ok && g_nprocs < MAX_PROCS;
         ok = Process32Next(snap, &e)) {
        struct pfs_proc *p = &g_procs[g_nprocs++];
        memset(p, 0, sizeof *p);
        p->pid = e.th32ProcessID;
        p->ppid = e.th32ParentProcessID;
        p->threads = e.cntThreads;
        set_comm(p, e.szExeFile, (int)(sizeof e.szExeFile / sizeof e.szExeFile[0]));
    }

    g_nthreads = 0;
    static Thread32F t32first, t32next;
    if (!t32first) {
        t32first = (Thread32F)pfs_sym(u"kernel32.dll", "Thread32First");
        t32next = (Thread32F)pfs_sym(u"kernel32.dll", "Thread32Next");
    }
    if (t32first && t32next) {
        struct nt_threadentry32 te;
        te.dwSize = sizeof te;
        for (bool32 ok = t32first(snap, &te); ok && g_nthreads < MAX_THREADS;
             ok = t32next(snap, &te)) {
            g_threads[g_nthreads].tid = te.th32ThreadID;
            g_threads[g_nthreads].pid = te.th32OwnerProcessID;
            g_nthreads++;
        }
    }

    CloseHandle(snap);
    g_snap_ms = pfs_now_ms();
}

int pfs_procs(const struct pfs_proc **out) {
    pthread_mutex_lock(&g_snap_lock);
    snap_refresh_locked();
    pthread_mutex_unlock(&g_snap_lock);
    *out = g_procs;
    return g_nprocs;
}

const struct pfs_proc *pfs_proc_find(uint32_t pid) {
    const struct pfs_proc *p;
    int n = pfs_procs(&p);
    for (int i = 0; i < n; i++)
        if (p[i].pid == pid) return &p[i];
    return 0;
}

int pfs_threads_of(uint32_t pid, uint32_t *out, int cap) {
    pthread_mutex_lock(&g_snap_lock);
    snap_refresh_locked();
    int n = 0;
    for (int i = 0; i < g_nthreads && n < cap; i++)
        if (g_threads[i].pid == pid) out[n++] = g_threads[i].tid;
    pthread_mutex_unlock(&g_snap_lock);
    return n;
}

// ---------------------------------------------------------------------------
// Time

// Jiffies must agree with sysconf(_SC_CLK_TCK), which cosmo sets to 30 on
// NT rather than the 100 Linux uses. Readers divide by the sysconf value,
// so a fixed 100 here would inflate every tick count by 3.3x.
uint64_t pfs_filetime_jiffies(uint64_t ft100ns) {
    static uint64_t hz;
    if (!hz) hz = __clk_tck();
    return ft100ns * hz / 10000000;
}

uint64_t pfs_now_filetime(void) {
    struct NtFileTime ft;
    GetSystemTimeAsFileTime(&ft);
    return (uint64_t)ft.dwHighDateTime << 32 | ft.dwLowDateTime;
}

uint64_t pfs_boot_filetime(void) {
    static uint64_t boot;
    if (!boot) boot = pfs_now_filetime() - GetTickCount64() * 10000;
    return boot;
}
