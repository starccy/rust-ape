// The top-level files: /proc/{meminfo,uptime,stat,loadavg,cpuinfo,version,
// filesystems,mounts,swaps}. Memory and cpu time have exact NT equivalents;
// the load average does not exist on NT at all, so it is an EMA of cpu
// utilization computed here, the shape a monitor expects fed by the
// closest fact the host has.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <libc/dce.h>
#include <libc/nt/accounting.h>
#include <libc/nt/createfile.h>
#include <libc/nt/enum/creationdisposition.h>
#include <libc/nt/enum/filesharemode.h>
#include <libc/nt/files.h>
#include <libc/nt/runtime.h>
#include <libc/nt/ntdll.h>
#include <libc/nt/registry.h>
#include <libc/nt/enum/reggetvalueflags.h>
#include <libc/nt/struct/filetime.h>
#include <libc/nt/struct/memorystatusex.h>
#include <libc/nt/struct/systeminfo.h>
#include <libc/nt/systeminfo.h>

#include "procfs.h"

const char *const pfs_top_files[] = {
    "meminfo", "uptime",      "stat",   "loadavg",   "cpuinfo", "version",
    "filesystems", "mounts", "diskstats", "swaps", "cmdline", 0};

static uint64_t ft(struct NtFileTime t) {
    return (uint64_t)t.dwHighDateTime << 32 | t.dwLowDateTime;
}

static int ncpus(void) {
    struct NtSystemInfo si;
    GetSystemInfo(&si);
    return si.dwNumberOfProcessors ? (int)si.dwNumberOfProcessors : 1;
}

// ---------------------------------------------------------------------------

static void gen_meminfo(struct pfs_buf *b) {
    struct NtMemoryStatusEx ms;
    memset(&ms, 0, sizeof ms);
    ms.dwLength = sizeof ms;
    GlobalMemoryStatusEx(&ms);
    // ullTotalPageFile is the commit limit (physical + pagefile), so the
    // pagefile alone, which is Linux's swap, is the excess over physical.
    uint64_t swap_total =
        ms.ullTotalPageFile > ms.ullTotalPhys
            ? ms.ullTotalPageFile - ms.ullTotalPhys : 0;
    uint64_t swap_free =
        ms.ullAvailPageFile > ms.ullAvailPhys
            ? ms.ullAvailPageFile - ms.ullAvailPhys : 0;
    if (swap_free > swap_total) swap_free = swap_total;
    pfs_printf(b, "MemTotal:       %8llu kB\n",
               (unsigned long long)(ms.ullTotalPhys / 1024));
    pfs_printf(b, "MemFree:        %8llu kB\n",
               (unsigned long long)(ms.ullAvailPhys / 1024));
    pfs_printf(b, "MemAvailable:   %8llu kB\n",
               (unsigned long long)(ms.ullAvailPhys / 1024));
    pfs_printf(b, "Buffers:               0 kB\nCached:                0 kB\n"
                  "SwapCached:            0 kB\nActive:                0 kB\n"
                  "Inactive:              0 kB\n");
    pfs_printf(b, "SwapTotal:      %8llu kB\n",
               (unsigned long long)(swap_total / 1024));
    pfs_printf(b, "SwapFree:       %8llu kB\n",
               (unsigned long long)(swap_free / 1024));
    pfs_printf(b, "Dirty:                 0 kB\nWriteback:             0 kB\n"
                  "Mapped:                0 kB\nShmem:                 0 kB\n"
                  "Slab:                  0 kB\n");
    pfs_printf(b, "CommitLimit:    %8llu kB\n",
               (unsigned long long)(ms.ullTotalPageFile / 1024));
    pfs_printf(b, "Committed_AS:   %8llu kB\n",
               (unsigned long long)((ms.ullTotalPageFile -
                                     ms.ullAvailPageFile) / 1024));
    pfs_printf(b, "VmallocTotal:          0 kB\nVmallocUsed:           0 kB\n"
                  "VmallocChunk:          0 kB\n");
}

static void gen_uptime(struct pfs_buf *b) {
    uint64_t up100ns = pfs_now_filetime() - pfs_boot_filetime();
    struct NtFileTime i, k, u;
    uint64_t idle100ns = 0;
    if (GetSystemTimes(&i, &k, &u)) idle100ns = ft(i);
    pfs_printf(b, "%llu.%02llu %llu.%02llu\n",
               (unsigned long long)(up100ns / 10000000),
               (unsigned long long)(up100ns / 100000 % 100),
               (unsigned long long)(idle100ns / 10000000),
               (unsigned long long)(idle100ns / 100000 % 100));
}

// SystemProcessorPerformanceInformation, one entry per cpu
struct cputimes { int64_t idle, kernel, user, dpc, intr; uint32_t nintr, pad; };

static void gen_stat(struct pfs_buf *b) {
    struct NtFileTime i, k, u;
    uint64_t idle = 0, kern = 0, user = 0;
    if (GetSystemTimes(&i, &k, &u)) idle = ft(i), kern = ft(k), user = ft(u);
    // NT's kernel time includes idle; Linux's system time does not
    uint64_t sys = kern > idle ? kern - idle : 0;
    pfs_printf(b, "cpu  %llu 0 %llu %llu 0 0 0 0 0 0\n",
               (unsigned long long)pfs_filetime_jiffies(user),
               (unsigned long long)pfs_filetime_jiffies(sys),
               (unsigned long long)pfs_filetime_jiffies(idle));
    int n = ncpus();
    struct cputimes per[64];
    if (n > 64) n = 64;
    if (!NtQuerySystemInformation(8, per, (uint32_t)(n * sizeof per[0]), 0)) {
        for (int c = 0; c < n; c++) {
            uint64_t ci = per[c].idle, ck = per[c].kernel, cu = per[c].user;
            uint64_t cs = ck > ci ? ck - ci : 0;
            pfs_printf(b, "cpu%d %llu 0 %llu %llu 0 0 0 0 0 0\n", c,
                       (unsigned long long)pfs_filetime_jiffies(cu),
                       (unsigned long long)pfs_filetime_jiffies(cs),
                       (unsigned long long)pfs_filetime_jiffies(ci));
        }
    }
    uint64_t btime =
        (pfs_boot_filetime() - 116444736000000000ull) / 10000000;
    pfs_printf(b, "intr 0\nctxt 0\nbtime %llu\nprocesses 1\n"
                  "procs_running 1\nprocs_blocked 0\n",
               (unsigned long long)btime);
}

static void gen_loadavg(struct pfs_buf *b) {
    // NT has no run-queue average, so approximate what a load average
    // reports for a cpu-bound box: busy cpus, smoothed over 1/5/15 minutes.
    static uint64_t last_busy, last_total;
    static double avg1, avg5, avg15;
    struct NtFileTime i, k, u;
    if (GetSystemTimes(&i, &k, &u)) {
        uint64_t idle = ft(i), total = ft(k) + ft(u);
        uint64_t busy = total > idle ? total - idle : 0;
        if (last_total && total > last_total) {
            double dt = (double)(total - last_total) / ncpus() / 10000000.0;
            double inst =
                (double)(busy - last_busy) / (double)(total - last_total) *
                ncpus();
            if (inst < 0) inst = 0;
            // dt/(dt+tau) in place of 1-exp(-dt/tau); same limits, no libm
            avg1 += (inst - avg1) * (dt / (dt + 60.0));
            avg5 += (inst - avg5) * (dt / (dt + 300.0));
            avg15 += (inst - avg15) * (dt / (dt + 900.0));
        }
        last_busy = busy;
        last_total = total;
    }
    const struct pfs_proc *p;
    int nproc = pfs_procs(&p);
    pfs_printf(b, "%.2f %.2f %.2f 1/%d %d\n", avg1, avg5, avg15,
               nproc > 0 ? nproc : 1, (int)pfs_self_pid());
}

// ---------------------------------------------------------------------------

#ifdef __x86_64__
static void cpuid(uint32_t leaf, uint32_t sub, uint32_t o[4]) {
    asm volatile("cpuid"
                 : "=a"(o[0]), "=b"(o[1]), "=c"(o[2]), "=d"(o[3])
                 : "a"(leaf), "c"(sub));
}
#endif

static void gen_cpuinfo(struct pfs_buf *b) {
    int n = ncpus();
    uint32_t mhz_cur[64] = {0}, mhz_max[64] = {0};
    pfs_cpu_mhz(mhz_cur, mhz_max, 64);
    // Real core/package identity when the host answers; the fallback (every
    // cpu its own core) is only wrong about hyperthreads. Physical core
    // counters (sysinfo) count distinct (physical id, core id) pairs here.
    uint8_t core[64] = {0}, pkg[64] = {0};
    int topo = pfs_cpu_topology(core, pkg, 64);
#ifdef __x86_64__
    uint32_t r[4];
    char vendor[13] = "unknown";
    cpuid(0, 0, r);
    memcpy(vendor, &r[1], 4);
    memcpy(vendor + 4, &r[3], 4);
    memcpy(vendor + 8, &r[2], 4);
    vendor[12] = 0;
    cpuid(1, 0, r);
    uint32_t fam = (r[0] >> 8 & 15) + (r[0] >> 20 & 255);
    uint32_t model = (r[0] >> 4 & 15) | (r[0] >> 12 & 0xf0);
    uint32_t step = r[0] & 15;
    char brand[49] = "unknown";
    cpuid(0x80000000u, 0, r);
    if (r[0] >= 0x80000004u) {
        for (int i = 0; i < 3; i++) {
            cpuid(0x80000002u + i, 0, r);
            memcpy(brand + i * 16, r, 16);
        }
        brand[48] = 0;
    }
    char *bp = brand;
    while (*bp == ' ') bp++;
    for (int c = 0; c < n; c++) {
        pfs_printf(b, "processor\t: %d\n", c);
        pfs_printf(b, "vendor_id\t: %s\n", vendor);
        pfs_printf(b, "cpu family\t: %u\nmodel\t\t: %u\n", fam, model);
        pfs_printf(b, "model name\t: %s\n", bp);
        pfs_printf(b, "stepping\t: %u\n", step);
        pfs_printf(b, "cpu MHz\t\t: %u.000\ncache size\t: 0 KB\n",
                   c < 64 ? mhz_cur[c] : 0);
        int phys = topo && c < topo ? pkg[c] : 0;
        int cid = topo && c < topo ? core[c] : c;
        int sib = 0, cores = 0;
        if (topo) {
            uint64_t seen = 0;
            for (int k = 0; k < topo; k++) {
                if (pkg[k] != phys) continue;
                sib++;
                if (!(seen >> core[k] & 1)) cores++;
                seen |= 1ull << core[k];
            }
        } else {
            sib = cores = n;
        }
        pfs_printf(b, "physical id\t: %d\nsiblings\t: %d\n", phys, sib);
        pfs_printf(b, "core id\t\t: %d\ncpu cores\t: %d\n", cid, cores);
        pfs_printf(b, "fpu\t\t: yes\nfpu_exception\t: yes\n");
        pfs_printf(b, "flags\t\t: fpu cx8 sep cmov mmx fxsr sse sse2 ht "
                      "syscall nx lm sse3 cx16 sse4_1 sse4_2 popcnt\n");
        pfs_printf(b, "bogomips\t: 0.00\naddress sizes\t: 48 bits physical, "
                      "48 bits virtual\n\n");
    }
#else
    for (int c = 0; c < n; c++)
        pfs_printf(b, "processor\t: %d\nBogoMIPS\t: 0.00\n\n", c);
#endif
}

// /proc/diskstats out of IOCTL_DISK_PERFORMANCE (works with a 0-access
// open; a probe confirmed the 88-byte struct and that no diskperf filter
// arming is needed on this vintage of NT). Drives are named in the sd*
// convention monitors expect. Fields with no NT equivalent (merges, the
// weighted-time refinement) read as their nearest neighbor or 0; sectors
// are the 512-byte units Linux reports regardless of hardware.
static void gen_diskstats(struct pfs_buf *b) {
    int misses = 0;
    for (int d = 0; d < 16 && misses < 2; d++) {
        char16_t path[24] = u"\\\\.\\PhysicalDrive0";
        path[17] = (char16_t)('0' + d);
        int64_t h = CreateFile(path, 0, kNtFileShareRead | kNtFileShareWrite,
                               0, kNtOpenExisting, 0, 0);
        if (h == -1) {
            misses++;
            continue;
        }
        struct {
            int64_t BytesRead, BytesWritten, ReadTime, WriteTime, IdleTime;
            uint32_t ReadCount, WriteCount, QueueDepth, SplitCount;
            int64_t QueryTime;
            uint32_t StorageDeviceNumber;
            char16_t StorageManagerName[8];
        } p;
        uint32_t got = 0;
        if (DeviceIoControl(h, 0x70020 /* IOCTL_DISK_PERFORMANCE */, 0, 0,
                            &p, sizeof p, &got, 0) &&
            got >= 72) {
            unsigned long long rd_ms = (unsigned long long)(p.ReadTime / 10000);
            unsigned long long wr_ms =
                (unsigned long long)(p.WriteTime / 10000);
            pfs_printf(b,
                       "   8 %7d sd%c %u 0 %llu %llu %u 0 %llu %llu %u %llu "
                       "%llu\n",
                       d * 16, 'a' + d, p.ReadCount,
                       (unsigned long long)(p.BytesRead / 512), rd_ms,
                       p.WriteCount,
                       (unsigned long long)(p.BytesWritten / 512), wr_ms,
                       p.QueueDepth, rd_ms + wr_ms, rd_ms + wr_ms);
        }
        CloseHandle(h);
    }
}

static void gen_mounts(struct pfs_buf *b) {
    pfs_printf(b, "rootfs / rootfs rw 0 0\n");
    pfs_printf(b, "proc /proc proc rw 0 0\n");
    pfs_printf(b, "sysfs /sys sysfs rw 0 0\n");
    uint32_t drives = GetLogicalDrives();
    for (int i = 0; i < 26; i++) {
        if (!(drives & 1u << i)) continue;
        // uppercase, the case NT itself answers paths in, so a mount
        // point here prefixes what /proc/self/exe and friends return
        pfs_printf(b, "%c: /%c ntfs rw 0 0\n", 'A' + i, 'A' + i);
    }
}

// The NT kernel's boot options, HKLM\SYSTEM\CurrentControlSet\Control
// \SystemStartOptions (" NOEXECUTE=OPTIN  NOVGA" and the like, from the
// BCD); empty when it cannot be read. Nothing is made up here.
static void gen_cmdline(struct pfs_buf *b) {
    char16_t w[1024];
    uint32_t cb = sizeof w;
    if (RegGetValue(kNtHkeyLocalMachine, u"SYSTEM\\CurrentControlSet\\Control",
                    u"SystemStartOptions", kNtRrfRtRegSz, 0, w, &cb))
        return;
    size_t n = cb / 2;
    while (n && (!w[n - 1] || w[n - 1] == ' ')) n--;
    size_t i = 0;
    while (i < n && w[i] == ' ') i++;
    for (; i < n; i++) {
        char c = w[i] < 128 ? (char)w[i] : '?';
        pfs_put(b, &c, 1);
    }
    pfs_put(b, "\n", 1);
}

bool pfs_gen_top_file(struct pfs_buf *b, const char *name) {
    if (!strcmp(name, "meminfo")) return gen_meminfo(b), true;
    if (!strcmp(name, "uptime")) return gen_uptime(b), true;
    if (!strcmp(name, "stat")) return gen_stat(b), true;
    if (!strcmp(name, "loadavg")) return gen_loadavg(b), true;
    if (!strcmp(name, "cpuinfo")) return gen_cpuinfo(b), true;
    if (!strcmp(name, "mounts")) return gen_mounts(b), true;
    if (!strcmp(name, "diskstats")) return gen_diskstats(b), true;
    if (!strcmp(name, "cmdline")) return gen_cmdline(b), true;
    if (!strcmp(name, "version")) // the shape of Linux's, the facts uname's
        return pfs_printf(b, "%s version %s (%s)\n", pfs_kernel_sysname(),
                          pfs_kernel_release(), pfs_kernel_version()),
               true;
    if (!strcmp(name, "filesystems"))
        return pfs_printf(b, "nodev\tsysfs\nnodev\tproc\nnodev\ttmpfs\n"
                             "\text4\n\tntfs\n\tvfat\n"),
               true;
    if (!strcmp(name, "swaps"))
        return pfs_printf(b, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\t"
                             "Priority\n"),
               true;
    return false;
}
