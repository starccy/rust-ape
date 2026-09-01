// The XNU generators: the same files the NT ones emit, out of what
// libSystem answers. Shapes and field positions mirror pid.c and
// sysinfo.c so every parser sees one dialect; a fact XNU cannot answer is
// emitted as 0, never omitted.

#define _COSMO_SOURCE // for libc/dce.h, __argc/__argv

#include <netinet/in.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>
#include <libc/dce.h>
#include <libc/runtime/runtime.h>

#include "procfs.h"
#include "xnu.h"

extern char **environ;

#define NCPU_MAX 64

static uint64_t ns_jiffies(uint64_t ns) {
    return ns * (uint64_t)pfs_xnu_hz() / 1000000000ull;
}

// ---------------------------------------------------------------------------
// /proc/<pid>/*

struct facts {
    bool have_bsd, have_task, have_ru;
    struct xnu_proc_bsdinfo bi;
    struct xnu_proc_taskinfo ti;
    struct xnu_rusage_info_v2 ru;
};

static void gather(uint32_t pid, struct facts *f) {
    memset(f, 0, sizeof *f);
    f->have_bsd = pfs_xnu_bsdinfo(pid, &f->bi);
    f->have_task = pfs_xnu_taskinfo(pid, &f->ti);
    f->have_ru = pfs_xnu_rusage(pid, &f->ru);
}

static char state_of(const struct facts *f) {
    if (!f->have_bsd) return 'S';
    switch (f->bi.pbi_status) { // SRUN..SZOMB, <sys/proc.h>
        case 2: return 'R';
        case 4: return 'T';
        case 5: return 'Z';
        default: return 'S';
    }
}

static uint64_t start_jiffies(const struct facts *f) {
    uint64_t boot = pfs_xnu_boottime();
    int hz = pfs_xnu_hz();
    if (!f->have_bsd || !boot || f->bi.pbi_start_tvsec < boot) return 0;
    return (f->bi.pbi_start_tvsec - boot) * (uint64_t)hz +
           f->bi.pbi_start_tvusec * (uint64_t)hz / 1000000;
}

static void gen_stat(struct pfs_buf *b, uint32_t pid,
                     const struct pfs_proc *p, const struct facts *f) {
    uint64_t pg = (uint64_t)pfs_xnu_pagesize();
    uint64_t utime = f->have_task ? ns_jiffies(pfs_xnu_mach_ns(f->ti.pti_total_user)) : 0;
    uint64_t stime = f->have_task ? ns_jiffies(pfs_xnu_mach_ns(f->ti.pti_total_system)) : 0;
    uint32_t threads = f->have_task && f->ti.pti_threadnum > 0
                           ? (uint32_t)f->ti.pti_threadnum : 1;
    uint64_t vsize = f->have_task ? f->ti.pti_virtual_size : 0;
    uint64_t rss = f->have_task ? f->ti.pti_resident_size : 0;
    uint32_t minflt = f->have_task && f->ti.pti_faults > 0
                          ? (uint32_t)f->ti.pti_faults : 0;
    uint32_t pgid = f->have_bsd ? f->bi.pbi_pgid : pid;
    pfs_printf(b, "%u (%s) %c %u %u %u 0 -1 0 %u 0 0 0 ", pid, p->comm,
               state_of(f), p->ppid, pgid, pid, minflt);
    pfs_printf(b, "%llu %llu 0 0 20 0 %u 0 %llu %llu %llu ",
               (unsigned long long)utime, (unsigned long long)stime, threads,
               (unsigned long long)start_jiffies(f),
               (unsigned long long)vsize, (unsigned long long)(rss / pg));
    pfs_printf(b, "18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 "
                  "0 0 0 0 0 0 0 0 0 0\n");
}

static void gen_status(struct pfs_buf *b, uint32_t pid,
                       const struct pfs_proc *p, const struct facts *f) {
    unsigned uid = f->have_bsd ? f->bi.pbi_uid : 0;
    unsigned gid = f->have_bsd ? f->bi.pbi_gid : 0;
    uint64_t vsize = f->have_task ? f->ti.pti_virtual_size : 0;
    uint64_t rss = f->have_task ? f->ti.pti_resident_size : 0;
    char st = state_of(f);
    pfs_printf(b, "Name:\t%s\n", p->comm);
    pfs_printf(b, "Umask:\t0022\n");
    pfs_printf(b, "State:\t%c (%s)\n", st,
               st == 'R' ? "running" : st == 'Z' ? "zombie" : "sleeping");
    pfs_printf(b, "Tgid:\t%u\nNgid:\t0\nPid:\t%u\nPPid:\t%u\n", pid, pid,
               p->ppid);
    pfs_printf(b, "TracerPid:\t0\n");
    pfs_printf(b, "Uid:\t%u\t%u\t%u\t%u\n", uid, uid, uid, uid);
    pfs_printf(b, "Gid:\t%u\t%u\t%u\t%u\n", gid, gid, gid, gid);
    pfs_printf(b, "FDSize:\t64\nGroups:\t%u\n", gid);
    pfs_printf(b, "VmPeak:\t%8llu kB\nVmSize:\t%8llu kB\n",
               (unsigned long long)(vsize / 1024),
               (unsigned long long)(vsize / 1024));
    pfs_printf(b, "VmHWM:\t%8llu kB\nVmRSS:\t%8llu kB\n",
               (unsigned long long)(rss / 1024),
               (unsigned long long)(rss / 1024));
    pfs_printf(b, "VmData:\t       0 kB\nVmStk:\t       0 kB\n"
                  "VmExe:\t       0 kB\nVmLib:\t       0 kB\n"
                  "VmSwap:\t       0 kB\n");
    pfs_printf(b, "Threads:\t%u\n",
               f->have_task && f->ti.pti_threadnum > 0
                   ? (unsigned)f->ti.pti_threadnum : 1);
    pfs_printf(b, "SigQ:\t0/0\nSigPnd:\t0000000000000000\n"
                  "ShdPnd:\t0000000000000000\nSigBlk:\t0000000000000000\n"
                  "SigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
                  "CapInh:\t0000000000000000\nCapPrm:\t0000000000000000\n"
                  "CapEff:\t0000000000000000\n");
    pfs_printf(b, "voluntary_ctxt_switches:\t0\n"
                  "nonvoluntary_ctxt_switches:\t0\n");
}

static void gen_statm(struct pfs_buf *b, const struct facts *f) {
    uint64_t pg = (uint64_t)pfs_xnu_pagesize();
    pfs_printf(b, "%llu %llu 0 0 0 0 0\n",
               (unsigned long long)((f->have_task ? f->ti.pti_virtual_size : 0) / pg),
               (unsigned long long)((f->have_task ? f->ti.pti_resident_size : 0) / pg));
}

static void gen_io(struct pfs_buf *b, const struct facts *f) {
    uint64_t rd = f->have_ru ? f->ru.ri_diskio_bytesread : 0;
    uint64_t wr = f->have_ru ? f->ru.ri_diskio_byteswritten : 0;
    pfs_printf(b,
               "rchar: %llu\nwchar: %llu\nsyscr: 0\nsyscw: 0\n"
               "read_bytes: %llu\nwrite_bytes: %llu\n"
               "cancelled_write_bytes: 0\n",
               (unsigned long long)rd, (unsigned long long)wr,
               (unsigned long long)rd, (unsigned long long)wr);
}

// argv joined with NULs. Our own comes from the runtime verbatim; anyone
// else's is the KERN_PROCARGS2 block, which the kernel only shows for
// processes the caller could ptrace, so the rest stay empty like other
// users' files effectively do on a hardened Linux.
static void gen_cmdline(struct pfs_buf *b, uint32_t pid) {
    if (pid == pfs_self_pid()) {
        for (int i = 0; i < __argc; i++)
            pfs_put(b, __argv[i], strlen(__argv[i]) + 1);
        return;
    }
    char *raw;
    size_t len;
    if (!pfs_xnu_procargs(pid, &raw, &len)) return;
    if (len > 4) {
        int argc;
        memcpy(&argc, raw, 4);
        const char *p = raw + 4, *end = raw + len;
        while (p < end && *p) p++; // the exec path
        while (p < end && !*p) p++;
        // a loader-run process's first argument is the loader itself; the
        // program's own argv is what Linux would show. Only trust that when
        // argv[0] is also spelled as the loader; an exec that names the
        // program keeps its argv whole.
        int skip = 0;
        char path[1024];
        long pl = pfs_xnu_pidpath(pid, path, sizeof path - 1);
        if (pl > 0) {
            path[pl] = 0;
            if (pfs_xnu_is_loader(path) && argc > 1 && p < end &&
                pfs_xnu_is_loader(p))
                skip = 1;
        }
        for (int i = 0; i < argc && p < end; i++) {
            size_t l = strnlen(p, (size_t)(end - p));
            if (skip && i == skip && l == 1 && *p == '-')
                skip++; // the "ape - prog" spelling
            if (i >= skip) {
                pfs_put(b, p, l);
                pfs_put(b, "", 1);
            }
            p += l + 1;
        }
    }
    free(raw);
}

// The KERN_PROCARGS2 block carries the environment after the argv strings,
// under the same may-ptrace visibility rule as the argv itself.
static void gen_environ(struct pfs_buf *b, uint32_t pid) {
    if (pid == pfs_self_pid()) {
        for (char **e = environ; *e; e++) pfs_put(b, *e, strlen(*e) + 1);
        return;
    }
    char *raw;
    size_t len;
    if (!pfs_xnu_procargs(pid, &raw, &len)) return;
    if (len > 4) {
        int argc;
        memcpy(&argc, raw, 4);
        const char *p = raw + 4, *end = raw + len;
        while (p < end && *p) p++; // the exec path
        while (p < end && !*p) p++;
        for (int i = 0; i < argc && p < end; i++) { // past the argv
            p += strnlen(p, (size_t)(end - p));
            if (p < end) p++;
        }
        while (p < end && !*p) p++;
        while (p < end && *p) { // env strings, until the empty one
            size_t l = strnlen(p, (size_t)(end - p));
            pfs_put(b, p, l);
            pfs_put(b, "", 1);
            p += l;
            if (p < end) p++;
        }
    }
    free(raw);
}

static void gen_limits(struct pfs_buf *b) {
    static const struct { const char *name, *unit; } rows[] = {
        {"Max cpu time", "seconds"},     {"Max file size", "bytes"},
        {"Max data size", "bytes"},      {"Max stack size", "bytes"},
        {"Max core file size", "bytes"}, {"Max resident set", "bytes"},
        {"Max processes", "processes"},  {"Max open files", "files"},
        {"Max locked memory", "bytes"},  {"Max address space", "bytes"},
        {"Max file locks", "locks"},     {"Max pending signals", "signals"},
        {"Max msgqueue size", "bytes"},  {"Max nice priority", ""},
        {"Max realtime priority", ""},   {"Max realtime timeout", "us"},
    };
    pfs_printf(b, "%-25s %20s %20s %-10s\n", "Limit", "Soft Limit",
               "Hard Limit", "Units");
    for (size_t i = 0; i < sizeof rows / sizeof rows[0]; i++)
        pfs_printf(b, "%-25s %20s %20s %-10s\n", rows[i].name, "unlimited",
                   "unlimited", rows[i].unit);
}

static void gen_mountinfo(struct pfs_buf *b) {
    pfs_printf(b, "1 1 0:1 / / rw - rootfs rootfs rw\n");
    pfs_printf(b, "2 1 0:5 / /proc rw - proc proc rw\n");
    pfs_printf(b, "3 1 0:6 / /sys rw - sysfs sysfs rw\n");
}

// maps. Our own address space out of the region walk, served only for
// ourselves like on NT; everyone else keeps the empty file. The sixth
// column must exist even for anonymous rows, parsers split on it.
static void gen_maps(struct pfs_buf *b, uint32_t pid) {
    if (pid != pfs_self_pid()) return;
    uint64_t addr = 0;
    uint32_t depth = 0;
    for (int guard = 0; guard < 100000; guard++) {
        uint64_t a = addr, size = 0;
        struct xnu_vm_region_submap_info_64 info;
        memset(&info, 0, sizeof info);
        if (!pfs_xnu_region(&a, &size, &depth, &info)) break;
        if (info.is_submap) {
            depth++;
            continue;
        }
        char perms[5] = "---p";
        if (info.protection & 1) perms[0] = 'r';
        if (info.protection & 2) perms[1] = 'w';
        if (info.protection & 4) perms[2] = 'x';
        if (info.share_mode == 4 /* SM_SHARED */ ||
            info.share_mode == 5 /* SM_TRUESHARED */ ||
            info.share_mode == 7 /* SM_SHARED_ALIASED */)
            perms[3] = 's';
        char path[512];
        long pl = pfs_xnu_regionfile(a, path, sizeof path - 1);
        if (pl > 0) {
            path[pl] = 0;
            uint64_t ino = 0xcbf29ce484222325ull;
            for (const char *c = path; *c; c++)
                ino = (ino ^ (uint8_t)*c) * 0x100000001b3ull;
            pfs_printf(b, "%08llx-%08llx %s %08llx 08:01 %llu %s\n",
                       (unsigned long long)a, (unsigned long long)(a + size),
                       perms, (unsigned long long)info.offset,
                       (unsigned long long)(ino & 0xffffff), path);
        } else {
            pfs_printf(b, "%08llx-%08llx %s %08llx 00:00 0 \n",
                       (unsigned long long)a, (unsigned long long)(a + size),
                       perms, (unsigned long long)info.offset);
        }
        addr = a + size;
    }
}

bool pfs_xnu_gen_pid_file(struct pfs_buf *b, uint32_t pid, const char *name) {
    const struct pfs_proc *p = pfs_xnu_proc_find(pid);
    if (!p) return false;

    if (!strcmp(name, "cmdline")) return gen_cmdline(b, pid), true;
    if (!strcmp(name, "comm")) return pfs_printf(b, "%s\n", p->comm), true;
    if (!strcmp(name, "environ")) return gen_environ(b, pid), true;
    if (!strcmp(name, "limits")) return gen_limits(b), true;
    if (!strcmp(name, "maps")) return gen_maps(b, pid), true;
    if (!strcmp(name, "cgroup")) return pfs_printf(b, "0::/\n"), true;
    if (!strcmp(name, "mountinfo")) return gen_mountinfo(b), true;
    if (!strcmp(name, "mounts")) return pfs_xnu_gen_top_file(b, "mounts");
    if (!strcmp(name, "oom_score_adj") || !strcmp(name, "oom_score"))
        return pfs_printf(b, "0\n"), true;

    struct facts f;
    gather(pid, &f);
    if (!strcmp(name, "stat")) return gen_stat(b, pid, p, &f), true;
    if (!strcmp(name, "status")) return gen_status(b, pid, p, &f), true;
    if (!strcmp(name, "statm")) return gen_statm(b, &f), true;
    if (!strcmp(name, "io")) return gen_io(b, &f), true;
    return false;
}

bool pfs_xnu_gen_pid_volatile(uint32_t pid, struct pfs_buf out[4],
                              uint64_t *starttime) {
    const struct pfs_proc *p = pfs_xnu_proc_find(pid);
    if (!p) return false;
    struct facts f;
    gather(pid, &f);
    if (starttime) *starttime = start_jiffies(&f);
    gen_stat(&out[0], pid, p, &f);
    gen_status(&out[1], pid, p, &f);
    gen_statm(&out[2], &f);
    gen_io(&out[3], &f);
    return true;
}

long pfs_xnu_pid_link(uint32_t pid, const char *name, char *buf, size_t n) {
    if (!strcmp(name, "root")) { // no chroot to see through here
        if (n < 1) return -1;
        buf[0] = '/';
        return 1;
    }
    if (!strcmp(name, "cwd")) {
        if (pid == pfs_self_pid())
            return getcwd(buf, n) ? (long)strlen(buf) : -1;
        return pfs_xnu_cwd(pid, buf, n);
    }
    if (strcmp(name, "exe")) return -1;
    if (pid == pfs_self_pid()) {
        // the kernel would name the ape loader; the program is the answer
        const char *exe = GetProgramExecutableName();
        if (!exe || exe[0] != '/') return -1;
        size_t l = strlen(exe);
        if (l > n) l = n;
        memcpy(buf, exe, l);
        return (long)l;
    }
    // the kernel names the loader for other ape processes too; answer with
    // the program, absolutized against their cwd when spelled relative
    char tmp[1024];
    long r = pfs_xnu_pidpath(pid, tmp, sizeof tmp - 1);
    if (r <= 0) return -1;
    tmp[r] = 0;
    if (pfs_xnu_is_loader(tmp)) {
        char prog[512];
        long pr = pfs_xnu_ape_program(pid, prog, sizeof prog - 1);
        if (pr > 0) {
            prog[pr] = 0;
            const char *rel = prog;
            if (rel[0] == '.' && rel[1] == '/') rel += 2;
            if (rel[0] == '/') {
                snprintf(tmp, sizeof tmp, "%s", rel);
            } else {
                char cwd[512];
                long cl = pfs_xnu_cwd(pid, cwd, sizeof cwd - 1);
                if (cl > 0) {
                    cwd[cl] = 0;
                    snprintf(tmp, sizeof tmp, "%s/%s", cwd, rel);
                }
            }
        }
    }
    size_t l = strlen(tmp);
    if (l > n) l = n;
    memcpy(buf, tmp, l);
    return (long)l;
}

// ---------------------------------------------------------------------------
// The top-level files.

static void gen_meminfo(struct pfs_buf *b) {
    uint64_t pg = (uint64_t)pfs_xnu_pagesize();
    uint64_t mem = 0;
    size_t l = sizeof mem;
    pfs_xnu_sysctl("hw.memsize", &mem, &l);
    struct xnu_vm_statistics64 vm;
    bool have_vm = pfs_xnu_vmstat(&vm);
    struct xnu_xsw_usage sw = {0};
    l = sizeof sw;
    pfs_xnu_sysctl("vm.swapusage", &sw, &l);
    uint64_t freeb = have_vm ? (uint64_t)vm.free_count * pg : 0;
    uint64_t avail = have_vm
        ? ((uint64_t)vm.free_count + vm.inactive_count + vm.purgeable_count) * pg
        : 0;
    pfs_printf(b, "MemTotal:       %8llu kB\n", (unsigned long long)(mem / 1024));
    pfs_printf(b, "MemFree:        %8llu kB\n", (unsigned long long)(freeb / 1024));
    pfs_printf(b, "MemAvailable:   %8llu kB\n", (unsigned long long)(avail / 1024));
    pfs_printf(b, "Buffers:               0 kB\n");
    pfs_printf(b, "Cached:         %8llu kB\n",
               (unsigned long long)(have_vm ? (uint64_t)vm.external_page_count * pg / 1024 : 0));
    pfs_printf(b, "SwapCached:            0 kB\n");
    pfs_printf(b, "Active:         %8llu kB\n",
               (unsigned long long)(have_vm ? (uint64_t)vm.active_count * pg / 1024 : 0));
    pfs_printf(b, "Inactive:       %8llu kB\n",
               (unsigned long long)(have_vm ? (uint64_t)vm.inactive_count * pg / 1024 : 0));
    pfs_printf(b, "SwapTotal:      %8llu kB\n",
               (unsigned long long)(sw.xsu_total / 1024));
    pfs_printf(b, "SwapFree:       %8llu kB\n",
               (unsigned long long)(sw.xsu_avail / 1024));
    pfs_printf(b, "Dirty:                 0 kB\nWriteback:             0 kB\n"
                  "Mapped:                0 kB\nShmem:                 0 kB\n"
                  "Slab:                  0 kB\n");
    pfs_printf(b, "CommitLimit:    %8llu kB\n",
               (unsigned long long)((mem + sw.xsu_total) / 1024));
    pfs_printf(b, "Committed_AS:   %8llu kB\n",
               (unsigned long long)(have_vm
                   ? ((uint64_t)vm.active_count + vm.wire_count) * pg / 1024 : 0));
    pfs_printf(b, "VmallocTotal:          0 kB\nVmallocUsed:           0 kB\n"
                  "VmallocChunk:          0 kB\n");
}

static void gen_uptime(struct pfs_buf *b) {
    uint64_t boot = pfs_xnu_boottime();
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    uint64_t up = boot && (uint64_t)now.tv_sec > boot
                      ? (uint64_t)now.tv_sec - boot : 0;
    struct xnu_cpu_ticks t[NCPU_MAX];
    int n = pfs_xnu_cpu_ticks(t, NCPU_MAX);
    uint64_t idle = 0;
    for (int i = 0; i < n; i++) idle += t[i].idle;
    idle /= (uint64_t)pfs_xnu_hz();
    pfs_printf(b, "%llu.00 %llu.00\n", (unsigned long long)up,
               (unsigned long long)idle);
}

static void gen_stat_top(struct pfs_buf *b) {
    struct xnu_cpu_ticks t[NCPU_MAX];
    int n = pfs_xnu_cpu_ticks(t, NCPU_MAX);
    struct xnu_cpu_ticks sum = {0};
    for (int i = 0; i < n; i++) {
        sum.user += t[i].user;
        sum.nice += t[i].nice;
        sum.system += t[i].system;
        sum.idle += t[i].idle;
    }
    pfs_printf(b, "cpu  %llu %llu %llu %llu 0 0 0 0 0 0\n",
               (unsigned long long)sum.user, (unsigned long long)sum.nice,
               (unsigned long long)sum.system, (unsigned long long)sum.idle);
    for (int i = 0; i < n; i++)
        pfs_printf(b, "cpu%d %llu %llu %llu %llu 0 0 0 0 0 0\n", i,
                   (unsigned long long)t[i].user, (unsigned long long)t[i].nice,
                   (unsigned long long)t[i].system,
                   (unsigned long long)t[i].idle);
    const struct pfs_proc *p;
    int nproc = pfs_xnu_procs(&p);
    pfs_printf(b, "intr 0\nctxt 0\nbtime %llu\nprocesses %d\n"
                  "procs_running 1\nprocs_blocked 0\n",
               (unsigned long long)pfs_xnu_boottime(),
               nproc > 0 ? nproc : 1);
}

static void gen_loadavg(struct pfs_buf *b) {
    struct { // <sys/sysctl.h> struct loadavg
        uint32_t ldavg[3];
        uint32_t pad;
        int64_t fscale;
    } la = {{0, 0, 0}, 0, 1};
    size_t l = sizeof la;
    pfs_xnu_sysctl("vm.loadavg", &la, &l);
    double fs = la.fscale > 0 ? (double)la.fscale : 1.0;
    const struct pfs_proc *p;
    int nproc = pfs_xnu_procs(&p);
    pfs_printf(b, "%.2f %.2f %.2f 1/%d %d\n", la.ldavg[0] / fs,
               la.ldavg[1] / fs, la.ldavg[2] / fs, nproc > 0 ? nproc : 1,
               (int)pfs_self_pid());
}

static void gen_cpuinfo(struct pfs_buf *b) {
    int n = 0;
    size_t l = sizeof n;
    pfs_xnu_sysctl("hw.ncpu", &n, &l);
    if (n <= 0) n = 1;
    char brand[128] = "unknown";
    l = sizeof brand - 1;
    if (pfs_xnu_sysctl("machdep.cpu.brand_string", brand, &l) == 0)
        brand[l < sizeof brand ? l : sizeof brand - 1] = 0;
    for (int c = 0; c < n; c++) {
        pfs_printf(b, "processor\t: %d\n", c);
        pfs_printf(b, "model name\t: %s\n", brand);
        pfs_printf(b, "BogoMIPS\t: 0.00\n");
        pfs_printf(b, "Features\t: fp asimd\n");
        pfs_printf(b, "CPU implementer\t: 0x61\n"); // Apple
        pfs_printf(b, "CPU architecture: 8\n\n");
    }
}

static void gen_mounts(struct pfs_buf *b) {
    pfs_printf(b, "rootfs / rootfs rw 0 0\n");
    pfs_printf(b, "proc /proc proc rw 0 0\n");
    pfs_printf(b, "sysfs /sys sysfs rw 0 0\n");
    pfs_printf(b, "apfs / apfs rw 0 0\n");
}

static void gen_cmdline_top(struct pfs_buf *b) {
    char args[1024] = "";
    size_t l = sizeof args - 1;
    if (pfs_xnu_sysctl("kern.bootargs", args, &l) == 0 && l < sizeof args)
        args[l] = 0;
    pfs_printf(b, "%s\n", args);
}

bool pfs_xnu_gen_top_file(struct pfs_buf *b, const char *name) {
    if (!strcmp(name, "meminfo")) return gen_meminfo(b), true;
    if (!strcmp(name, "uptime")) return gen_uptime(b), true;
    if (!strcmp(name, "stat")) return gen_stat_top(b), true;
    if (!strcmp(name, "loadavg")) return gen_loadavg(b), true;
    if (!strcmp(name, "cpuinfo")) return gen_cpuinfo(b), true;
    if (!strcmp(name, "mounts")) return gen_mounts(b), true;
    if (!strcmp(name, "diskstats")) return true; // empty, no per-disk source yet
    if (!strcmp(name, "cmdline")) return gen_cmdline_top(b), true;
    if (!strcmp(name, "version"))
        return pfs_printf(b, "%s version %s (%s)\n", pfs_kernel_sysname(),
                          pfs_kernel_release(), pfs_kernel_version()),
               true;
    if (!strcmp(name, "filesystems"))
        return pfs_printf(b, "nodev\tsysfs\nnodev\tproc\nnodev\ttmpfs\n"
                             "\tapfs\n\thfs\n"),
               true;
    if (!strcmp(name, "swaps"))
        return pfs_printf(b, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\t"
                             "Priority\n"),
               true;
    return false;
}

// ---------------------------------------------------------------------------
// /proc/net/*. The socket tables come from pcblist64 and the interface
// counters from NET_RT_IFLIST2, the same sources netstat reads. A row's
// inode is the shared identity hash with pid 0, since the tables do not
// name the owner; the self fd/ links hash the same way so the join holds.

// The Linux table states, from XNU's TCPS_* order.
static int tcp_state_of(int t_state) {
    static const int map[] = {7, 10, 2, 3, 1, 8, 4, 11, 9, 5, 6};
    return t_state >= 0 && t_state < 11 ? map[t_state] : 7;
}

static void put_addr(struct pfs_buf *b, bool v6, const uint8_t *a,
                     uint16_t port) {
    if (v6) {
        uint32_t w[4];
        memcpy(w, a, 16);
        pfs_printf(b, "%08X%08X%08X%08X:%04X", w[0], w[1], w[2], w[3], port);
    } else {
        uint32_t w;
        memcpy(&w, a, 4);
        pfs_printf(b, "%08X:%04X", w, port);
    }
}

static void net_table(struct pfs_buf *b, const char *name) {
    bool tcp = name[0] == 't';
    bool v6 = strchr(name, '6') != 0;
    pfs_printf(b, "  sl  local_address rem_address   st tx_queue rx_queue "
                  "tr tm->when retrnsmt   uid  timeout inode\n");
    char *raw;
    size_t len;
    if (!pfs_xnu_pcblist(tcp ? "net.inet.tcp.pcblist64"
                             : "net.inet.udp.pcblist64",
                         &raw, &len))
        return;
    const char *p = raw, *end = raw + len;
    if ((size_t)(end - p) < sizeof(struct xnu_xinpgen)) {
        free(raw);
        return;
    }
    const struct xnu_xinpgen *gen = (const void *)p;
    if (!gen->xig_len || gen->xig_len > (size_t)(end - p)) {
        free(raw);
        return;
    }
    p += gen->xig_len;
    int sl = 0;
    while (p + sizeof(struct xnu_xinpgen) <= end) {
        uint32_t rlen;
        memcpy(&rlen, p, 4); // xt_len and the low half of xi_len lead both
        if (rlen < sizeof(struct xnu_xinpgen) || rlen > (size_t)(end - p))
            break;
        const struct xnu_xinpcb64 *inp;
        int state = 7; // Linux TCP_CLOSE, what UDP rows show
        if (tcp) {
            if (rlen < sizeof(struct xnu_xtcpcb64)) break;
            const struct xnu_xtcpcb64 *tp = (const void *)p;
            inp = &tp->xt_inpcb;
            state = tcp_state_of(tp->t_state);
        } else {
            if (rlen < sizeof(struct xnu_xinpcb64)) break;
            inp = (const void *)p;
        }
        p += rlen;
        bool row6 = (inp->inp_vflag & 2) != 0; // INP_IPV6
        if (row6 != v6) continue;
        uint16_t lport = __builtin_bswap16(inp->inp_lport);
        uint16_t rport = __builtin_bswap16(inp->inp_fport);
        uint8_t laddr[16] = {0}, raddr[16] = {0};
        if (v6) {
            memcpy(laddr, inp->inp_dependladdr.a6, 16);
            memcpy(raddr, inp->inp_dependfaddr.a6, 16);
        } else {
            memcpy(laddr, &inp->inp_dependladdr.a4.addr4, 4);
            memcpy(raddr, &inp->inp_dependfaddr.a4.addr4, 4);
        }
        uint64_t inode = pfs_net_inode(tcp ? 0 : 1, v6 ? 6 : 4, 0, lport,
                                       rport, laddr, raddr);
        pfs_printf(b, "%4d: ", sl++);
        put_addr(b, v6, laddr, lport);
        pfs_put(b, " ", 1);
        put_addr(b, v6, raddr, rport);
        pfs_printf(b, " %02X %08X:%08X 00:00000000 00000000 %5u        0 "
                      "%llu 1 0000000000000000 0 0 0 0 0\n",
                   state, inp->xi_socket.so_snd.sb_cc,
                   inp->xi_socket.so_rcv.sb_cc, inp->xi_socket.so_uid,
                   (unsigned long long)inode);
    }
    free(raw);
}

#define XNU_RTM_IFINFO2 0x12
#define XNU_RTA_IFP 0x10
#define XNU_IFF_UP 0x1
#define XNU_IFF_RUNNING 0x40

// The interface list parsed once per second, shared by /proc/net/dev and
// the /sys/class/net slice.
int pfs_xnu_ifstats(struct pfs_ifstat *out, int cap) {
    static struct pfs_ifstat cache[32];
    static int ncache;
    static int64_t ms;
    int64_t t = pfs_now_ms();
    if (!ms || t - ms >= 1000) {
        ncache = 0;
        char *raw;
        size_t len;
        if (pfs_xnu_iflist2(&raw, &len)) {
            const char *p = raw, *end = raw + len;
            while (p + 4 <= end && ncache < 32) {
                uint16_t mlen;
                memcpy(&mlen, p, 2);
                uint8_t type = (uint8_t)p[3];
                if (!mlen || mlen > (size_t)(end - p)) break;
                if (type == XNU_RTM_IFINFO2 &&
                    mlen >= sizeof(struct xnu_if_msghdr2)) {
                    const struct xnu_if_msghdr2 *m = (const void *)p;
                    struct pfs_ifstat *o = &cache[ncache];
                    memset(o, 0, sizeof *o);
                    snprintf(o->name, sizeof o->name, "if%u", m->ifm_index);
                    if (m->ifm_addrs & XNU_RTA_IFP) {
                        const struct xnu_sockaddr_dl *dl =
                            (const void *)(p + sizeof *m);
                        size_t nl = dl->sdl_nlen;
                        if (nl > sizeof o->name - 1) nl = sizeof o->name - 1;
                        if ((const char *)dl->sdl_data + nl <= end) {
                            memcpy(o->name, dl->sdl_data, nl);
                            o->name[nl] = 0;
                        }
                        if (dl->sdl_alen && dl->sdl_alen <= 8 &&
                            (const char *)dl->sdl_data + dl->sdl_nlen +
                                    dl->sdl_alen <= end) {
                            memcpy(o->mac, dl->sdl_data + dl->sdl_nlen,
                                   dl->sdl_alen);
                            o->maclen = dl->sdl_alen;
                        }
                    }
                    const struct xnu_if_data64 *d = &m->ifm_data;
                    o->rx_bytes = d->ifi_ibytes;
                    o->tx_bytes = d->ifi_obytes;
                    o->rx_pkts = d->ifi_ipackets;
                    o->tx_pkts = d->ifi_opackets;
                    o->rx_errs = d->ifi_ierrors;
                    o->tx_errs = d->ifi_oerrors;
                    o->rx_drop = d->ifi_iqdrops;
                    o->tx_drop = 0;
                    o->mtu = d->ifi_mtu;
                    o->up = (m->ifm_flags & XNU_IFF_UP) &&
                            (m->ifm_flags & XNU_IFF_RUNNING);
                    o->speed_mbps = d->ifi_baudrate / 1000000;
                    ncache++;
                }
                p += mlen;
            }
            free(raw);
        }
        ms = t;
    }
    int n = ncache < cap ? ncache : cap;
    memcpy(out, cache, (size_t)n * sizeof *out);
    return n;
}

static void net_dev(struct pfs_buf *b) {
    pfs_printf(b,
               "Inter-|   Receive                                       "
               "         |  Transmit\n"
               " face |bytes    packets errs drop fifo frame compressed "
               "multicast|bytes    packets errs drop fifo colls carrier "
               "compressed\n");
    struct pfs_ifstat ifs[32];
    int n = pfs_xnu_ifstats(ifs, 32);
    for (int i = 0; i < n; i++)
        pfs_printf(b,
                   "%6s: %7llu %7llu %4llu %4llu %5u %5u %10u %9u "
                   "%8llu %7llu %4llu %4llu %5u %7u %10u %10u\n",
                   ifs[i].name, (unsigned long long)ifs[i].rx_bytes,
                   (unsigned long long)ifs[i].rx_pkts,
                   (unsigned long long)ifs[i].rx_errs,
                   (unsigned long long)ifs[i].rx_drop, 0, 0, 0, 0,
                   (unsigned long long)ifs[i].tx_bytes,
                   (unsigned long long)ifs[i].tx_pkts,
                   (unsigned long long)ifs[i].tx_errs,
                   (unsigned long long)ifs[i].tx_drop, 0, 0, 0, 0);
}

bool pfs_xnu_gen_net_file(struct pfs_buf *b, const char *name) {
    if (!strcmp(name, "tcp") || !strcmp(name, "tcp6") ||
        !strcmp(name, "udp") || !strcmp(name, "udp6"))
        return net_table(b, name), true;
    if (!strcmp(name, "unix"))
        return pfs_printf(b, "Num       RefCount Protocol Flags    Type St "
                             "Inode Path\n"),
               true;
    if (!strcmp(name, "dev")) return net_dev(b), true;
    return false;
}

// ---------------------------------------------------------------------------
// /proc/self/fd, out of the kernel's own list. The entries carry the real
// descriptor numbers; a socket hashes to the same inode its table row does
// (pid 0 on both sides, the tables do not name owners here).

#define XNU_FDTYPE_VNODE 1
#define XNU_FDTYPE_SOCKET 2
#define XNU_FDTYPE_KQUEUE 5
#define XNU_FDTYPE_PIPE 6

static void xnu_socket_text(int fd, char *out, size_t n) {
    uint8_t laddr[16] = {0}, raddr[16] = {0};
    uint16_t lport = 0, rport = 0;
    uint8_t family = 4;
    uint8_t proto = 0;
    struct sockaddr_storage ss;
    socklen_t sl = sizeof ss;
    if (!getsockname(fd, (struct sockaddr *)&ss, &sl)) {
        if (ss.ss_family == AF_INET) {
            const struct sockaddr_in *a = (const void *)&ss;
            memcpy(laddr, &a->sin_addr, 4);
            lport = ntohs(a->sin_port);
        } else if (ss.ss_family == AF_INET6) {
            const struct sockaddr_in6 *a = (const void *)&ss;
            memcpy(laddr, &a->sin6_addr, 16);
            lport = ntohs(a->sin6_port);
            family = 6;
        }
    }
    sl = sizeof ss;
    if (!getpeername(fd, (struct sockaddr *)&ss, &sl)) {
        if (ss.ss_family == AF_INET) {
            const struct sockaddr_in *a = (const void *)&ss;
            memcpy(raddr, &a->sin_addr, 4);
            rport = ntohs(a->sin_port);
        } else if (ss.ss_family == AF_INET6) {
            const struct sockaddr_in6 *a = (const void *)&ss;
            memcpy(raddr, &a->sin6_addr, 16);
            rport = ntohs(a->sin6_port);
        }
    }
    int type = 0;
    sl = sizeof type;
    getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &sl);
    if (type == SOCK_DGRAM) proto = 1;
    uint64_t inode =
        pfs_net_inode(proto, family, 0, lport, rport, laddr, raddr);
    snprintf(out, n, "socket:[%llu]", (unsigned long long)inode);
}

// A socket in another process, through the kernel's per-descriptor answer;
// hashed with pid 0 like the table rows so the join with /proc/net holds.
static void other_socket_text(uint32_t pid, int fd, char *out, size_t n) {
    struct xnu_sock_id id;
    if (pfs_xnu_fdsock(pid, fd, &id)) {
        uint64_t inode = pfs_net_inode(id.proto, id.family, 0, id.lport,
                                       id.rport, id.laddr, id.raddr);
        snprintf(out, n, "socket:[%llu]", (unsigned long long)inode);
        return;
    }
    // not an inet socket, or not visible; a stable distinct number will do
    snprintf(out, n, "socket:[%llu]",
             (unsigned long long)((uint64_t)pid << 20 | (uint32_t)fd));
}

int pfs_xnu_fds_of(uint32_t pid, struct pfs_fdent *out, int cap) {
    static _Thread_local struct xnu_proc_fdinfo fds[1024];
    int nfd = pfs_xnu_listfds(pid, fds, 1024);
    bool self = pid == pfs_self_pid();
    int n = 0;
    for (int i = 0; i < nfd && n < cap; i++) {
        struct pfs_fdent *e = &out[n];
        long r;
        switch (fds[i].proc_fdtype) {
            case XNU_FDTYPE_VNODE:
                r = pfs_xnu_fdpath(pid, fds[i].proc_fd, e->text,
                                   sizeof e->text - 1);
                if (r > 0)
                    e->text[r] = 0;
                else
                    snprintf(e->text, sizeof e->text, "anon_inode:[vnode]");
                break;
            case XNU_FDTYPE_SOCKET:
                // our own descriptors answer getsockname directly; anyone
                // else's go through the kernel's fdinfo
                if (self)
                    xnu_socket_text(fds[i].proc_fd, e->text, sizeof e->text);
                else
                    other_socket_text(pid, fds[i].proc_fd, e->text,
                                      sizeof e->text);
                break;
            case XNU_FDTYPE_PIPE:
                snprintf(e->text, sizeof e->text, "pipe:[%d]",
                         1000000 + fds[i].proc_fd);
                break;
            case XNU_FDTYPE_KQUEUE:
                snprintf(e->text, sizeof e->text, "anon_inode:[kqueue]");
                break;
            default:
                snprintf(e->text, sizeof e->text, "anon_inode:[type%u]",
                         fds[i].proc_fdtype);
                break;
        }
        e->fd = fds[i].proc_fd;
        n++;
    }
    return n;
}

// ---------------------------------------------------------------------------
// The /sys slices, fully virtual: /sys/devices/system/cpu for the topology
// files monitors read, /sys/class/net for the per-interface statistics
// sysinfo insists on. Anything else under /sys stays absent, as it is on
// this host anyway.

static int ncpu_of(void) {
    int n = 0;
    size_t l = sizeof n;
    pfs_xnu_sysctl("hw.ncpu", &n, &l);
    return n > 0 ? n : 1;
}

// Splits "/sys/..." into at most 6 components; returns the count.
static int sys_split(const char *path, char comp[6][64]) {
    int n = 0;
    const char *p = path;
    while (*p == '/') p++;
    while (*p && n < 6) {
        size_t i = 0;
        while (*p && *p != '/' && i < 63) comp[n][i++] = *p++;
        comp[n][i] = 0;
        while (*p && *p != '/') p++;
        while (*p == '/') p++;
        n++;
    }
    return n;
}

static bool ifname_known(const char *name) {
    struct pfs_ifstat ifs[32];
    int n = pfs_xnu_ifstats(ifs, 32);
    for (int i = 0; i < n; i++)
        if (!strcmp(ifs[i].name, name)) return true;
    return false;
}

static const char *const sys_net_files[] = {"mtu", "operstate", "address",
                                            "carrier", "speed", 0};
static const char *const sys_net_stats[] = {
    "rx_bytes", "tx_bytes",   "rx_packets", "tx_packets",
    "rx_errors", "tx_errors", "rx_dropped", "tx_dropped", 0};
static const char *const sys_cpu_files[] = {"online", "possible", "present",
                                            0};

static int name_in(const char *const *names, const char *name) {
    for (int i = 0; names[i]; i++)
        if (!strcmp(names[i], name)) return i;
    return -1;
}

int pfs_xnu_sysfs_kind(const char *path) {
    char c[6][64];
    int n = sys_split(path, c);
    if (n < 1 || strcmp(c[0], "sys")) return -1;
    if (n == 1) return 1;
    if (!strcmp(c[1], "class")) {
        if (n == 2) return 1;
        if (strcmp(c[2], "net")) return -1;
        if (n == 3) return 1;
        if (!ifname_known(c[3])) return -1;
        if (n == 4) return 1;
        if (!strcmp(c[4], "statistics"))
            return n == 5 ? 1 : (n == 6 && name_in(sys_net_stats, c[5]) >= 0 ? 0 : -1);
        return n == 5 && name_in(sys_net_files, c[4]) >= 0 ? 0 : -1;
    }
    if (!strcmp(c[1], "devices")) {
        if (n == 2) return 1;
        if (strcmp(c[2], "system")) return -1;
        if (n == 3) return 1;
        if (strcmp(c[3], "cpu")) return -1;
        if (n == 4) return 1;
        return n == 5 && name_in(sys_cpu_files, c[4]) >= 0 ? 0 : -1;
    }
    return -1;
}

bool pfs_xnu_gen_sysfs(struct pfs_buf *b, const char *path) {
    char c[6][64];
    int n = sys_split(path, c);
    if (pfs_xnu_sysfs_kind(path) != 0) return false;
    if (!strcmp(c[1], "devices")) { // .../cpu/{online,possible,present}
        pfs_printf(b, "0-%d\n", ncpu_of() - 1);
        return true;
    }
    struct pfs_ifstat ifs[32];
    int ni = pfs_xnu_ifstats(ifs, 32);
    const struct pfs_ifstat *f = 0;
    for (int i = 0; i < ni; i++)
        if (!strcmp(ifs[i].name, c[3])) f = &ifs[i];
    if (!f) return false;
    const char *leaf = n == 6 ? c[5] : c[4];
    if (!strcmp(leaf, "rx_bytes"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->rx_bytes), true;
    if (!strcmp(leaf, "tx_bytes"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->tx_bytes), true;
    if (!strcmp(leaf, "rx_packets"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->rx_pkts), true;
    if (!strcmp(leaf, "tx_packets"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->tx_pkts), true;
    if (!strcmp(leaf, "rx_errors"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->rx_errs), true;
    if (!strcmp(leaf, "tx_errors"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->tx_errs), true;
    if (!strcmp(leaf, "rx_dropped"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->rx_drop), true;
    if (!strcmp(leaf, "tx_dropped"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->tx_drop), true;
    if (!strcmp(leaf, "mtu")) return pfs_printf(b, "%u\n", f->mtu), true;
    if (!strcmp(leaf, "operstate"))
        return pfs_printf(b, "%s\n", f->up ? "up" : "down"), true;
    if (!strcmp(leaf, "carrier"))
        return pfs_printf(b, "%d\n", f->up ? 1 : 0), true;
    if (!strcmp(leaf, "speed"))
        return pfs_printf(b, "%llu\n", (unsigned long long)f->speed_mbps),
               true;
    if (!strcmp(leaf, "address")) {
        for (uint32_t i = 0; i < f->maclen; i++)
            pfs_printf(b, "%s%02x", i ? ":" : "", f->mac[i]);
        pfs_printf(b, "\n");
        return true;
    }
    return false;
}

static bool list_add(struct pfs_virtent **p, int *n, int *cap,
                     const char *name, unsigned char type) {
    if (*n == *cap) {
        int c = *cap ? *cap * 2 : 16;
        struct pfs_virtent *q = realloc(*p, (size_t)c * sizeof **p);
        if (!q) return false;
        *p = q;
        *cap = c;
    }
    snprintf((*p)[*n].name, sizeof(*p)[*n].name, "%s", name);
    (*p)[(*n)++].type = type;
    return true;
}

int pfs_xnu_sysfs_list(const char *path, struct pfs_virtent **out) {
    if (pfs_xnu_sysfs_kind(path) != 1) return -1;
    char c[6][64];
    int n = sys_split(path, c);
    struct pfs_virtent *p = 0;
    int cnt = 0, cap = 0;
    bool ok = list_add(&p, &cnt, &cap, ".", 4) && // DT_DIR
              list_add(&p, &cnt, &cap, "..", 4);
    if (n == 1) {
        ok = ok && list_add(&p, &cnt, &cap, "class", 4) &&
             list_add(&p, &cnt, &cap, "devices", 4);
    } else if (!strcmp(c[1], "class")) {
        if (n == 2) {
            ok = ok && list_add(&p, &cnt, &cap, "net", 4);
        } else if (n == 3) {
            struct pfs_ifstat ifs[32];
            int ni = pfs_xnu_ifstats(ifs, 32);
            for (int i = 0; ok && i < ni; i++)
                ok = list_add(&p, &cnt, &cap, ifs[i].name, 4);
        } else if (n == 4) {
            ok = ok && list_add(&p, &cnt, &cap, "statistics", 4);
            for (int i = 0; ok && sys_net_files[i]; i++)
                ok = list_add(&p, &cnt, &cap, sys_net_files[i], 8); // DT_REG
        } else {
            for (int i = 0; ok && sys_net_stats[i]; i++)
                ok = list_add(&p, &cnt, &cap, sys_net_stats[i], 8);
        }
    } else {
        if (n == 2) {
            ok = ok && list_add(&p, &cnt, &cap, "system", 4);
        } else if (n == 3) {
            ok = ok && list_add(&p, &cnt, &cap, "cpu", 4);
        } else {
            for (int i = 0; ok && sys_cpu_files[i]; i++)
                ok = list_add(&p, &cnt, &cap, sys_cpu_files[i], 8);
        }
    }
    if (!ok) {
        free(p);
        return -1;
    }
    *out = p;
    return cnt;
}
