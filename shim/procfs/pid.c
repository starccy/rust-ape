// /proc/<pid>/* content. The facts come from three places, the shared
// toolhelp snapshot (name, parent, thread count), an OpenProcess handle with
// query rights (times, memory, io, image path, command line), and for our own
// process the C runtime itself (argv, environ, cwd), which is both cheaper
// and exact. A field NT cannot answer is emitted as 0 rather than omitted,
// since every parser of these files splits on position, and a short line
// aborts the parse where a zero merely reads as idle.

#define _COSMO_SOURCE // for libc/dce.h, __argc/__argv

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <libc/dce.h>
#include <libc/nt/accounting.h>
#include <libc/nt/files.h>
#include <libc/nt/memory.h>
#include <libc/nt/enum/processaccess.h>
#include <libc/nt/enum/processinfoclass.h>
#include <libc/nt/struct/processbasicinformation.h>
#include <libc/nt/nt/process.h>
#include <libc/nt/process.h>
#include <libc/nt/runtime.h>
#include <libc/nt/struct/filetime.h>
#include <libc/nt/struct/iocounters.h>
#include <libc/nt/struct/memorybasicinformation.h>
#include <libc/nt/struct/processmemorycounters.h>
#include <libc/runtime/runtime.h>

#include "procfs.h"
#include "xnu.h"

extern char **environ;

// The split the carrier materializes by. The volatile four move while a
// process runs and share one OpenProcess pass; the stable rest is fixed for
// a process's lifetime and only ever needs writing once.
const char *const pfs_pid_volatile[] = {"stat", "status", "statm", "io", 0};
const char *const pfs_pid_stable[] = {
    "cmdline", "comm",      "environ", "limits",        "maps",
    "cgroup",  "mountinfo", "mounts",  "oom_score_adj", "oom_score", 0};

static uint64_t ft(struct NtFileTime t) {
    return (uint64_t)t.dwHighDateTime << 32 | t.dwLowDateTime;
}

// The per-process counters, from the snapshot when it carries them, else
// what one OpenProcess handle answers, gathered in a single pass.
struct pidfacts {
    bool have;
    uint64_t utime, stime; // jiffies
    uint64_t starttime;    // jiffies since boot
    uint64_t vsize, rss;   // bytes
    uint64_t peak_vsize, peak_rss;
    uint32_t minflt;
    struct NtIoCounters io;
};

static void gather(uint32_t pid, struct pidfacts *f) {
    memset(f, 0, sizeof *f);
    const struct pfs_proc *p = pfs_proc_find(pid);
    if (p && p->facts) {
        f->have = true;
        f->utime = pfs_filetime_jiffies(p->utime);
        f->stime = pfs_filetime_jiffies(p->stime);
        uint64_t boot = pfs_boot_filetime();
        if (p->create > boot)
            f->starttime = pfs_filetime_jiffies(p->create - boot);
        f->vsize = p->vsize;
        f->rss = p->rss;
        f->peak_vsize = p->peak_vsize;
        f->peak_rss = p->peak_rss;
        f->minflt = p->minflt;
        f->io = p->io;
        return;
    }
    int64_t h = pfs_open_process(pid);
    if (!h) return;
    f->have = true;

    struct NtFileTime tc, tx, tk, tu;
    if (GetProcessTimes(h, &tc, &tx, &tk, &tu)) {
        f->utime = pfs_filetime_jiffies(ft(tu));
        f->stime = pfs_filetime_jiffies(ft(tk));
        uint64_t boot = pfs_boot_filetime(), created = ft(tc);
        if (created > boot) f->starttime = pfs_filetime_jiffies(created - boot);
    }
    struct NtProcessMemoryCountersEx mc;
    memset(&mc, 0, sizeof mc);
    mc.cb = sizeof mc;
    if (GetProcessMemoryInfo(h, &mc, sizeof mc)) {
        f->vsize = mc.PagefileUsage ? mc.PagefileUsage : mc.PrivateUsage;
        f->rss = mc.WorkingSetSize;
        f->peak_vsize = mc.PeakPagefileUsage;
        f->peak_rss = mc.PeakWorkingSetSize;
        f->minflt = mc.PageFaultCount;
    }
    GetProcessIoCounters(h, &f->io);
    CloseHandle(h);
}

static char state_of(uint32_t pid) {
    return pid == pfs_self_pid() ? 'R' : 'S';
}

// ---------------------------------------------------------------------------
// stat: "<pid> (comm) <state>" then 49 positional fields, Linux 6.x's count.

static void gen_stat(struct pfs_buf *b, uint32_t pid,
                     const struct pfs_proc *p, const struct pidfacts *f) {
    pfs_printf(b, "%u (%s) %c %u %u %u 0 -1 0 %u 0 0 0 ", pid, p->comm,
               state_of(pid), p->ppid, pid, pid, f->minflt);
    pfs_printf(b, "%llu %llu 0 0 20 0 %u 0 %llu %llu %llu ",
               (unsigned long long)f->utime, (unsigned long long)f->stime,
               p->threads, (unsigned long long)f->starttime,
               (unsigned long long)f->vsize,
               (unsigned long long)(f->rss / 4096));
    pfs_printf(b, "18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 "
                  "0 0 0 0 0 0 0 0 0 0\n");
}

static void gen_status(struct pfs_buf *b, uint32_t pid,
                       const struct pfs_proc *p, const struct pidfacts *f) {
    bool self = pid == pfs_self_pid();
    unsigned uid = self ? (unsigned)getuid() : 0;
    unsigned gid = self ? (unsigned)getgid() : 0;
    pfs_printf(b, "Name:\t%s\n", p->comm);
    pfs_printf(b, "Umask:\t0022\n");
    pfs_printf(b, "State:\t%c (%s)\n", state_of(pid),
               self ? "running" : "sleeping");
    pfs_printf(b, "Tgid:\t%u\nNgid:\t0\nPid:\t%u\nPPid:\t%u\n", pid, pid,
               p->ppid);
    pfs_printf(b, "TracerPid:\t0\n");
    pfs_printf(b, "Uid:\t%u\t%u\t%u\t%u\n", uid, uid, uid, uid);
    pfs_printf(b, "Gid:\t%u\t%u\t%u\t%u\n", gid, gid, gid, gid);
    pfs_printf(b, "FDSize:\t64\nGroups:\t%u\n", gid);
    pfs_printf(b, "VmPeak:\t%8llu kB\nVmSize:\t%8llu kB\n",
               (unsigned long long)(f->peak_vsize / 1024),
               (unsigned long long)(f->vsize / 1024));
    pfs_printf(b, "VmHWM:\t%8llu kB\nVmRSS:\t%8llu kB\n",
               (unsigned long long)(f->peak_rss / 1024),
               (unsigned long long)(f->rss / 1024));
    pfs_printf(b, "VmData:\t       0 kB\nVmStk:\t       0 kB\n"
                  "VmExe:\t       0 kB\nVmLib:\t       0 kB\n"
                  "VmSwap:\t       0 kB\n");
    pfs_printf(b, "Threads:\t%u\n", p->threads);
    pfs_printf(b, "SigQ:\t0/0\nSigPnd:\t0000000000000000\n"
                  "ShdPnd:\t0000000000000000\nSigBlk:\t0000000000000000\n"
                  "SigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
                  "CapInh:\t0000000000000000\nCapPrm:\t0000000000000000\n"
                  "CapEff:\t0000000000000000\n");
    pfs_printf(b, "voluntary_ctxt_switches:\t0\n"
                  "nonvoluntary_ctxt_switches:\t0\n");
}

static void gen_statm(struct pfs_buf *b, const struct pidfacts *f) {
    pfs_printf(b, "%llu %llu 0 0 0 0 0\n",
               (unsigned long long)(f->vsize / 4096),
               (unsigned long long)(f->rss / 4096));
}

// ---------------------------------------------------------------------------
// cmdline: argv joined with NULs. Our own comes from the runtime verbatim.
// Anyone else's is one command-line string in their PEB, asked for with
// NtQueryInformationProcess and split the way Windows itself would split it.

#define ProcessCommandLineInformation 60

typedef char16_t **(__msabi *CommandLineToArgvF)(const char16_t *, int *);
typedef int64_t (__msabi *LocalFreeF)(int64_t);

static void put_utf8(struct pfs_buf *b, const char16_t *w, int len) {
    for (int i = 0; i < len; i++) {
        uint32_t c = w[i];
        if (c >= 0xd800 && c < 0xdc00 && i + 1 < len && w[i + 1] >= 0xdc00 &&
            w[i + 1] < 0xe000) {
            c = 0x10000 + ((c - 0xd800) << 10) + (w[++i] - 0xdc00);
        }
        char u[4];
        int n;
        if (c < 0x80) u[0] = c, n = 1;
        else if (c < 0x800) u[0] = 0xc0 | c >> 6, u[1] = 0x80 | (c & 63), n = 2;
        else if (c < 0x10000)
            u[0] = 0xe0 | c >> 12, u[1] = 0x80 | (c >> 6 & 63),
            u[2] = 0x80 | (c & 63), n = 3;
        else
            u[0] = 0xf0 | c >> 18, u[1] = 0x80 | (c >> 12 & 63),
            u[2] = 0x80 | (c >> 6 & 63), u[3] = 0x80 | (c & 63), n = 4;
        pfs_put(b, u, n);
    }
}

static void gen_cmdline(struct pfs_buf *b, uint32_t pid) {
    if (pid == pfs_self_pid()) {
        for (int i = 0; i < __argc; i++)
            pfs_put(b, __argv[i], strlen(__argv[i]) + 1);
        return;
    }
    int64_t h = pfs_open_process(pid);
    if (!h) return;
    // a UNICODE_STRING header whose Buffer points into the same allocation
    struct { uint16_t len, max; uint32_t pad; char16_t *buf; } *us;
    static _Alignas(8) char raw[32768]; // guarded by the caller's lock
    uint32_t rlen = 0;
    us = (void *)raw;
    if (!NtQueryInformationProcess(h, ProcessCommandLineInformation, raw,
                                   sizeof raw, &rlen) &&
        us->buf && us->len) {
        int nw = us->len / 2;
        static CommandLineToArgvF to_argv;
        static LocalFreeF local_free;
        if (!to_argv) {
            to_argv = (CommandLineToArgvF)pfs_sym(u"shell32.dll",
                                                  "CommandLineToArgvW");
            local_free = (LocalFreeF)pfs_sym(u"kernel32.dll", "LocalFree");
        }
        us->buf[nw] = 0;
        char16_t **argv;
        int argc = 0;
        if (to_argv && (argv = to_argv(us->buf, &argc))) {
            for (int i = 0; i < argc; i++) {
                int n = 0;
                while (argv[i][n]) n++;
                // argv[0] in the shape exe and cwd use ("/C/x/y"). Readers
                // with unix path semantics take the basename after the last
                // slash and stop at a colon, so "C:\x\y.exe" would name
                // the process "C".
                if (i == 0) {
                    for (int j = 0; j < n; j++)
                        if (argv[0][j] == '\\') argv[0][j] = '/';
                    if (n >= 2 && argv[0][1] == ':') {
                        argv[0][1] = argv[0][0];
                        argv[0][0] = '/';
                    }
                }
                put_utf8(b, argv[i], n);
                pfs_put(b, "", 1);
            }
            if (local_free) local_free((int64_t)argv);
        } else {
            put_utf8(b, us->buf, nw);
            pfs_put(b, "", 1);
        }
    }
    CloseHandle(h);
}

// ---------------------------------------------------------------------------
// Another process's cwd and environment. NT keeps both only in the target's
// own user-mode memory (PEB -> RTL_USER_PROCESS_PARAMETERS), so they are
// read out with ReadProcessMemory the way Process Explorer does it. Needs
// PROCESS_VM_READ, which the same user's ordinary processes grant and
// system/protected/other users' ones refuse; those fall back to what a
// plain read could say before (cwd ENOENT, environ empty, the closest a
// read comes to the EACCES Linux answers for other users). Only 64-bit
// targets are read, since a WOW64 process has a 32-bit PEB with other
// offsets; it reports as absent rather than misread.

typedef bool32 (__msabi *ReadMemF)(int64_t, uint64_t, void *, uint64_t,
                                   uint64_t *);

static bool read_mem(int64_t h, uint64_t addr, void *out, size_t n) {
    static ReadMemF read;
    if (!read) read = (ReadMemF)pfs_sym(u"kernel32.dll", "ReadProcessMemory");
    uint64_t got = 0;
    return read && read(h, addr, out, n, &got) && got == n;
}

// Opens `pid` for memory reads and returns its process parameters block;
// 0 with no handle when either is not to be had.
static uint64_t open_params(uint32_t pid, int64_t *hout) {
    int64_t h = OpenProcess(kNtProcessQueryInformation | kNtProcessVmRead,
                            false, pid);
    if (!h) return 0;
    struct NtProcessBasicInformation pbi;
    uint32_t rlen = 0;
    uint64_t wow64 = 0, params = 0;
    if (NtQueryInformationProcess(h, kNtProcessBasicInformation, &pbi,
                                  sizeof pbi, &rlen) ||
        (!NtQueryInformationProcess(h, kNtProcessWow64Information, &wow64,
                                    sizeof wow64, &rlen) &&
         wow64) ||
        !read_mem(h, (uint64_t)(uintptr_t)pbi.PebBaseAddress + 0x20, &params,
                  8) ||
        !params) {
        CloseHandle(h);
        return 0;
    }
    *hout = h;
    return params;
}

// A UNICODE_STRING at `addr` in the target, as chars into w[cap]; -1 if it
// cannot be read whole.
static int read_ustring(int64_t h, uint64_t addr, char16_t *w, size_t cap) {
    struct { uint16_t len, max; uint32_t pad; uint64_t buf; } us;
    if (!read_mem(h, addr, &us, sizeof us)) return -1;
    size_t n = us.len / 2;
    if (!us.buf || n > cap) return -1;
    if (n && !read_mem(h, us.buf, w, n * 2)) return -1;
    return (int)n;
}

// "C:\x\y" as "/C/x/y", the shape cosmo spells absolute paths in; a bare
// drive form is not absolute to a caller with unix Path semantics (fish
// asserts on exactly that). Returns the byte count written, no terminator.
static size_t put_ntpath(char *buf, size_t n, const char16_t *w, size_t len) {
    size_t k = 0;
    for (size_t i = 0; i < len && k < n; i++)
        buf[k++] = w[i] == '\\' ? '/' : (w[i] < 128 ? (char)w[i] : '_');
    if (k >= 2 && buf[1] == ':') {
        buf[1] = buf[0];
        buf[0] = '/';
    }
    return k;
}

static long other_cwd(uint32_t pid, char *buf, size_t n) {
    int64_t h;
    uint64_t params = open_params(pid, &h);
    if (!params) return -1;
    char16_t w[600];
    int len = read_ustring(h, params + 0x38, w, 600); // CurrentDirectory.DosPath
    CloseHandle(h);
    if (len <= 0) return -1;
    if (len > 3 && w[len - 1] == '\\') len--; // NT keeps a trailing slash
    return (long)put_ntpath(buf, n, w, (size_t)len);
}

static void gen_environ(struct pfs_buf *b, uint32_t pid) {
    if (pid == pfs_self_pid()) {
        for (char **e = environ; *e; e++) pfs_put(b, *e, strlen(*e) + 1);
        return;
    }
    int64_t h;
    uint64_t params = open_params(pid, &h);
    if (!params) return;
    uint64_t env = 0, size = 0;
    if (read_mem(h, params + 0x80, &env, 8) && env) {
        // EnvironmentSize (Vista+) bounds the block; without it, walk pages
        // until the double NUL or the mapping ends
        if (!read_mem(h, params + 0x3f0, &size, 8) || size > (1u << 20))
            size = 1u << 20;
        static char16_t page[2048]; // guarded by the caller's lock
        uint64_t off = 0;
        bool done = false;
        while (!done && off < size) {
            size_t want = sizeof page;
            if (size - off < want) want = (size_t)(size - off);
            if (!read_mem(h, env + off, page, want)) break;
            size_t nw = want / 2, i = 0;
            while (i < nw) {
                size_t j = i;
                while (j < nw && page[j]) j++;
                if (j == i) { done = true; break; } // empty string: the end
                if (j == nw && off + want < size) {
                    // a string straddles the page: restart it next round
                    break;
                }
                put_utf8(b, page + i, (int)(j - i));
                pfs_put(b, "", 1);
                i = j + 1;
            }
            if (i == 0 && !done) break; // one string longer than a page
            off += i * 2;
        }
    }
    CloseHandle(h);
}

static void gen_io(struct pfs_buf *b, const struct pidfacts *f) {
    pfs_printf(b,
               "rchar: %llu\nwchar: %llu\nsyscr: %llu\nsyscw: %llu\n"
               "read_bytes: %llu\nwrite_bytes: %llu\n"
               "cancelled_write_bytes: 0\n",
               (unsigned long long)f->io.ReadTransferCount,
               (unsigned long long)f->io.WriteTransferCount,
               (unsigned long long)f->io.ReadOperationCount,
               (unsigned long long)f->io.WriteOperationCount,
               (unsigned long long)f->io.ReadTransferCount,
               (unsigned long long)f->io.WriteTransferCount);
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

// ---------------------------------------------------------------------------
// maps. Our own address space out of VirtualQuery, image regions attributed
// to the module whose allocation they are. Served only for ourselves, since
// backtrace symbolization is the consumer and it reads its own; everyone
// else keeps the empty file, like environ.

typedef int32_t (__msabi *EnumModsF)(int64_t, int64_t *, uint32_t,
                                     uint32_t *);
typedef uint32_t (__msabi *ModNameF)(int64_t, int64_t, char16_t *, uint32_t);

static void gen_maps(struct pfs_buf *b, uint32_t pid) {
    if (pid != pfs_self_pid()) return;

    static struct { uint64_t base; char path[512]; } mods[64];
    int nmods = 0; // statics guarded by the caller's lock
    static EnumModsF em;
    static ModNameF mn;
    if (!em) {
        em = (EnumModsF)pfs_sym(u"kernel32.dll", "K32EnumProcessModules");
        mn = (ModNameF)pfs_sym(u"kernel32.dll", "K32GetModuleFileNameExW");
    }
    int64_t handles[64];
    uint32_t need = 0;
    if (em && mn && em(-1 /* ourselves */, handles, sizeof handles, &need)) {
        nmods = (int)(need / 8);
        if (nmods > 64) nmods = 64;
        for (int i = 0; i < nmods; i++) {
            mods[i].base = (uint64_t)handles[i];
            char16_t w[512];
            uint32_t len = mn(-1, handles[i], w, 512);
            size_t k = 0;
            for (uint32_t j = 0; j < len && k < sizeof mods[i].path - 1; j++)
                mods[i].path[k++] = w[j] == '\\'
                                        ? '/'
                                        : (w[j] < 128 ? (char)w[j] : '_');
            mods[i].path[k] = 0;
            // "C:/x" -> "/C/x", the shape absolute paths take here
            if (k >= 2 && mods[i].path[1] == ':') {
                mods[i].path[1] = mods[i].path[0];
                mods[i].path[0] = '/';
            }
        }
    }

    struct NtMemoryBasicInformation mbi;
    uint64_t addr = 0;
    uint64_t stackmark = (uint64_t)(uintptr_t)&mbi;
    while (addr < 0x7fffffff0000ull &&
           VirtualQuery((void *)addr, &mbi, sizeof mbi) == sizeof mbi) {
        uint64_t base = (uint64_t)mbi.BaseAddress, size = mbi.RegionSize;
        uint64_t next = base + size;
        if (next <= addr) break;
        addr = next;
        if (mbi.State != 0x1000 /* MEM_COMMIT */) continue;

        uint32_t pr = mbi.Protect & 0xff;
        char perms[5] = "---p";
        if (pr & 0xee) perms[0] = 'r';        // every readable protection
        if (pr & 0xcc) perms[1] = 'w';        // RW, WRITECOPY and their X kin
        if (pr & 0xf0) perms[2] = 'x';        // the EXECUTE_* four
        if (mbi.Type == 0x40000 /* MEM_MAPPED */) perms[3] = 's';

        const char *name = 0;
        uint64_t off = 0;
        if (mbi.Type == 0x1000000 /* MEM_IMAGE */) {
            for (int i = 0; i < nmods; i++) {
                if (mods[i].base == (uint64_t)mbi.AllocationBase) {
                    name = mods[i].path;
                    off = base - mods[i].base;
                    break;
                }
            }
        }
        if (!name && stackmark >= base && stackmark < next) name = "[stack]";

        if (name && name[0] == '/') {
            uint64_t ino = 0xcbf29ce484222325ull;
            for (const char *c = name; *c; c++) ino = (ino ^ (uint8_t)*c) *
                                                     0x100000001b3ull;
            pfs_printf(b, "%08llx-%08llx %s %08llx 08:01 %llu %s\n",
                       (unsigned long long)base, (unsigned long long)next,
                       perms, (unsigned long long)off,
                       (unsigned long long)(ino & 0xffffff), name);
        } else {
            // the kernel prints a space after the inode even with no
            // pathname; parsers count on that sixth column existing
            pfs_printf(b, "%08llx-%08llx %s %08llx 00:00 0 %s\n",
                       (unsigned long long)base, (unsigned long long)next,
                       perms, (unsigned long long)off, name ? name : "");
        }
    }
}

// mountinfo: the same facts as /proc/mounts in the richer per-process
// format (std's available_parallelism and friends walk it hunting cgroup
// mounts; other readers look drive roots up in it).
static void gen_mountinfo(struct pfs_buf *b) {
    pfs_printf(b, "1 1 0:1 / / rw - rootfs rootfs rw\n");
    pfs_printf(b, "2 1 0:5 / /proc rw - proc proc rw\n");
    pfs_printf(b, "3 1 0:6 / /sys rw - sysfs sysfs rw\n");
    uint32_t drives = GetLogicalDrives();
    int id = 4;
    for (int i = 0; i < 26; i++) {
        if (!(drives & 1u << i)) continue;
        pfs_printf(b, "%d 1 8:%d / /%c rw - ntfs %c: rw\n", id++, i * 16,
                   'A' + i, 'A' + i);
    }
}

bool pfs_gen_pid_file(struct pfs_buf *b, uint32_t pid, const char *name) {
    if (IsXnuSilicon()) return pfs_xnu_gen_pid_file(b, pid, name);
    const struct pfs_proc *p = pfs_proc_find(pid);
    if (!p) return false;

    if (!strcmp(name, "cmdline")) return gen_cmdline(b, pid), true;
    if (!strcmp(name, "comm")) return pfs_printf(b, "%s\n", p->comm), true;
    if (!strcmp(name, "environ")) return gen_environ(b, pid), true;
    if (!strcmp(name, "limits")) return gen_limits(b), true;
    if (!strcmp(name, "maps")) return gen_maps(b, pid), true;
    if (!strcmp(name, "cgroup")) return pfs_printf(b, "0::/\n"), true;
    if (!strcmp(name, "mountinfo")) return gen_mountinfo(b), true;
    // per-process mounts: /proc/mounts is really self/mounts on Linux, and
    // plenty of callers spell out the real thing
    if (!strcmp(name, "mounts")) return pfs_gen_top_file(b, "mounts");
    if (!strcmp(name, "oom_score_adj") || !strcmp(name, "oom_score"))
        return pfs_printf(b, "0\n"), true;

    struct pidfacts f;
    gather(pid, &f);
    if (!strcmp(name, "stat")) return gen_stat(b, pid, p, &f), true;
    if (!strcmp(name, "status")) return gen_status(b, pid, p, &f), true;
    if (!strcmp(name, "statm")) return gen_statm(b, &f), true;
    if (!strcmp(name, "io")) return gen_io(b, &f), true;
    return false;
}

bool pfs_gen_pid_volatile(uint32_t pid, struct pfs_buf out[4],
                          uint64_t *starttime) {
    if (IsXnuSilicon()) return pfs_xnu_gen_pid_volatile(pid, out, starttime);
    const struct pfs_proc *p = pfs_proc_find(pid);
    if (!p) return false;
    struct pidfacts f;
    gather(pid, &f);
    if (starttime) *starttime = f.starttime;
    gen_stat(&out[0], pid, p, &f);
    gen_status(&out[1], pid, p, &f);
    gen_statm(&out[2], &f);
    gen_io(&out[3], &f);
    return true;
}

// ---------------------------------------------------------------------------
// Links

typedef bool32 (__msabi *QueryImageF)(int64_t, uint32_t, char16_t *,
                                      uint32_t *);

long pfs_pid_link(uint32_t pid, const char *name, char *buf, size_t n) {
    if (IsXnuSilicon()) return pfs_xnu_pid_link(pid, name, buf, n);
    if (!strcmp(name, "root")) { // no chroot to see through here
        if (n < 1) return -1;
        buf[0] = '/';
        return 1;
    }
    if (!strcmp(name, "cwd")) {
        if (pid != pfs_self_pid()) return other_cwd(pid, buf, n);
        return getcwd(buf, n) ? (long)strlen(buf) : -1;
    }
    if (strcmp(name, "exe")) return -1;

    int64_t h = pfs_open_process(pid);
    if (!h) return -1;
    static QueryImageF query;
    if (!query)
        query = (QueryImageF)pfs_sym(u"kernel32.dll",
                                     "QueryFullProcessImageNameW");
    char16_t w[600];
    uint32_t len = 600;
    long r = -1;
    if (query && query(h, 0, w, &len))
        r = (long)put_ntpath(buf, n, w, len);
    CloseHandle(h);
    return r;
}
