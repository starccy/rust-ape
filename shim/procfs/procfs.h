// Internal interface of the /proc emulation (Windows only; on every other
// host the real /proc is untouched). The split is deliberate. Everything in
// pid.c/net.c/sysinfo.c/sysctl.c is a generator, reading NT and emitting
// Linux-formatted text into a pfs_buf while knowing nothing about files or
// descriptors. core/ is the carrier, deciding when a generator runs and
// where its output goes (a materialized file under the skeleton tree, or a
// delete-on-close descriptor handed straight to the caller). Swapping the
// carrier for a fully virtual one later must not touch the generators.
#ifndef RUST_APE_SHIM_PROCFS_H_
#define RUST_APE_SHIM_PROCFS_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <libc/nt/struct/iocounters.h>

// ---------------------------------------------------------------------------
// Growable text buffer the generators emit into. On allocation failure `oom`
// latches and further writes are dropped; the carrier turns that into ENOMEM.

struct pfs_buf {
    char *p;
    size_t n, cap;
    int oom;
};

void pfs_printf(struct pfs_buf *b, const char *fmt, ...)
    __attribute__((__format__(__printf__, 2, 3)));
void pfs_put(struct pfs_buf *b, const void *data, size_t n);
void pfs_buf_free(struct pfs_buf *b);

// ---------------------------------------------------------------------------
// ntapi.c. NT plumbing shared by the generators.

// GetProcAddress with the DLL cached.
void *pfs_sym(const char16_t *dll, const char *name);

// OpenProcess with query rights (limited first, full as fallback); 0 on
// failure. Callers CloseHandle what they get.
int64_t pfs_open_process(uint32_t pid);

// The pid this process has in the emulated tree, the Win32 one. cosmo's
// getpid() is not it after fork+exec on NT, where execve spawns the
// program as a new process, hands it the forker's pid through _COSMO_PID
// and terminates the forker, so getpid() then names a dead process. The
// self entry and identity tests use this; the parser maps a bare
// /proc/<getpid()> onto it as well.
uint32_t pfs_self_pid(void);

// The kernel identity, aligned with what cosmo's uname() says on this host
// (sysname "Windows", version "Cosmopolitan 4.0.2 MODE=..."). The release is
// uname's "10.0" plus the real build number, as in "10.0.20348", so
// major.minor.patch parsers get three components.
const char *pfs_kernel_sysname(void);
const char *pfs_kernel_release(void);
const char *pfs_kernel_version(void);

// One toolhelp snapshot, refreshed at most every PFS_SNAP_MS, answering
// every question about which processes exist.
struct pfs_proc {
    uint32_t pid, ppid;
    uint32_t threads;
    char comm[64]; // printable, no space/parens; never empty
    // The per-process counters when the snapshot came from
    // NtQuerySystemInformation, which reports them for every process in
    // one call and without a handle. Times are 100ns units.
    bool facts;
    uint64_t utime, stime, create;
    uint64_t vsize, rss, peak_vsize, peak_rss;
    uint32_t minflt;
    struct NtIoCounters io;
};
int pfs_procs(const struct pfs_proc **out); // returns count
const struct pfs_proc *pfs_proc_find(uint32_t pid);

// Thread ids of one process, via Thread32First on NT and the libproc
// listing on XNU, whose ids need the full 64 bits; returns 0 when the host
// cannot say (caller then pretends one thread, the pid).
int pfs_threads_of(uint32_t pid, uint64_t *out, int cap);

// Time. Jiffies run at sysconf(_SC_CLK_TCK), see pfs_filetime_jiffies.
uint64_t pfs_filetime_jiffies(uint64_t ft100ns);
uint64_t pfs_boot_filetime(void); // boot instant as a filetime
uint64_t pfs_now_filetime(void);
int64_t pfs_now_ms(void); // CLOCK_MONOTONIC

// ---------------------------------------------------------------------------
// pid.c. /proc/<pid>/*.

// Emit one content file ("stat", "status", ...). False means no such file.
bool pfs_gen_pid_file(struct pfs_buf *b, uint32_t pid, const char *name);

// The materialization split. The volatile four move while a process runs and
// come out of one OpenProcess pass; the stable rest never changes for a
// process's lifetime. starttime identifies the incarnation, so a recycled
// pid is recognized as a new process.
extern const char *const pfs_pid_volatile[];
extern const char *const pfs_pid_stable[];
bool pfs_gen_pid_volatile(uint32_t pid, struct pfs_buf out[4],
                          uint64_t *starttime);

// Link text of /proc/<pid>/{exe,cwd}; -1 when absent for this pid.
long pfs_pid_link(uint32_t pid, const char *name, char *buf, size_t n);

// ---------------------------------------------------------------------------
// net.c. /proc/net/* plus socket ownership, shared with the fd machinery.

extern const char *const pfs_net_files[];
bool pfs_gen_net_file(struct pfs_buf *b, const char *name);

// Socket inodes held by one pid, for /proc/<pid>/fd. Refreshes the tables
// if stale.
int pfs_net_fds_of(uint32_t pid, uint64_t *inodes, int cap);

// One interface's counters and state, joined from the adapter list (which
// filters out NDIS shadow rows and carries the name) and the interface
// table (which carries 64-bit counters). Shared by /proc/net/dev and the
// /sys/class/net emulation.
struct pfs_ifstat {
    char name[16]; // cosmo's spelling of the FriendlyName
    uint64_t rx_bytes, tx_bytes, rx_pkts, tx_pkts;
    uint64_t rx_errs, tx_errs, rx_drop, tx_drop;
    uint32_t mtu;
    bool up;
    uint8_t mac[8];
    uint32_t maclen;     // 0 on interfaces with no hardware address
    uint64_t speed_mbps; // negotiated link speed; 0 when NT will not say
};
int pfs_net_ifstats(struct pfs_ifstat *out, int cap);

// A socket's inode from its identity, shared between the table rows and the
// descriptors in our own fd table so the two views join up.
uint64_t pfs_net_inode(uint8_t proto, uint8_t family, uint32_t pid,
                       uint16_t lport, uint16_t rport,
                       const uint8_t laddr[16], const uint8_t raddr[16]);

// ---------------------------------------------------------------------------
// fd.c. Our own process's full descriptor table, read out of cosmo's g_fds.
// Entry names are the real fd numbers, unlike other processes' directories,
// which only know sockets and number them densely.

struct pfs_fdent {
    int fd;
    char text[512]; // the link target
};
int pfs_self_fds(struct pfs_fdent *out, int cap);

// Another process's descriptors, real fd numbers and link texts, on hosts
// whose kernel enumerates them (XNU); -1 where none will (NT), and the
// caller falls back to the socket tables' dense numbering.
int pfs_other_fds(uint32_t pid, struct pfs_fdent *out, int cap);

// ---------------------------------------------------------------------------
// sysinfo.c. The top-level files (/proc/meminfo, uptime, stat, ...).

extern const char *const pfs_top_files[];
bool pfs_gen_top_file(struct pfs_buf *b, const char *name);

// ---------------------------------------------------------------------------
// sysctl.c. /proc/sys/**. `rest` is the path after the /proc/sys/ prefix.

bool pfs_gen_sys_file(struct pfs_buf *b, const char *rest);
// Directories the subtree needs, relative to /proc/sys, NULL-ended.
extern const char *const pfs_sys_dirs[];
// Files in the subtree, relative to /proc/sys, NULL-ended.
extern const char *const pfs_sys_files[];

// ---------------------------------------------------------------------------
// power.c. CallNtPowerInformation, for /sys/class/power_supply and the
// per-core frequencies in /sys/devices/system/cpu and /proc/cpuinfo.

struct pfs_batt {
    bool present, ac, charging, discharging;
    uint32_t max_mwh, rem_mwh; // capacities in milliwatt-hours
    int32_t rate_mw;           // drain/charge rate, absolute value
    uint32_t design_mwh;       // 0 when the battery device will not say
    uint32_t cycles;
    char chem[5];              // chemistry tag ("LION"); empty when unknown
};
bool pfs_battery(struct pfs_batt *out);
int pfs_cpu_mhz(uint32_t *cur, uint32_t *max, int cap); // per-cpu, MHz

// Core and package of each logical cpu, from the processor relationship
// records; 0 when the host cannot answer (callers then assume every cpu
// is its own core on package 0, which is also what /proc says on machines
// Linux cannot read).
int pfs_cpu_topology(uint8_t *coreid, uint8_t *pkgid, int cap);

// ---------------------------------------------------------------------------
// dmi.c. The SMBIOS identity behind /sys/class/dmi/id. Each have_* says
// whether that structure exists in the table; a missing one leaves its
// files out, the way the kernel does.

struct pfs_dmi {
    bool have_bios, have_sys, have_board, have_chassis;
    char bios_vendor[64], bios_version[64], bios_date[32];
    char sys_vendor[64], product_name[64], product_version[64];
    char product_sku[64], product_family[64];
    char board_vendor[64], board_name[64], board_version[64];
    char board_asset_tag[64];
    char chassis_vendor[64], chassis_version[64];
    int chassis_type;
};
bool pfs_dmi(struct pfs_dmi *out);

// core/virtdir.c. A listing of the tree served from memory, for the
// vendored shim/dirstream.c: the entries of `path`, count >= 0 when the path
// is a directory listed here, -2 when it is one but absent (a dead pid),
// -1 when it is not ours. The caller frees the entries.
struct pfs_virtent {
    char name[32];
    unsigned char type; // DT_*
};
int __ape_shim_procfs_virtual_dir(const char *path, struct pfs_virtent **out);
// core/open.c. The descriptor behind such a listing, -1 if none could be had.
int __ape_shim_procfs_memfd_dir(const char *vpath);
// core/open.c. The virtual path a tracked descriptor of the tree stands
// for; false when fd is not one of ours.
bool __ape_shim_procfs_fd_vpath(int fd, char *out, unsigned long n);

#endif // RUST_APE_SHIM_PROCFS_H_
