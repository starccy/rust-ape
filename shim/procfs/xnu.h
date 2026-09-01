// The XNU side of the /proc emulation, Apple Silicon only. Data comes from
// libSystem through the ape loader's Syslib table: sysctl by name, and the
// libproc and mach entry points resolved once with its dlsym. The public
// pfs_* generator entries dispatch here on IsXnuSilicon(); the carrier and
// the output shapes are shared with NT, the data plumbing is not.
#ifndef RUST_APE_SHIM_PROCFS_XNU_H_
#define RUST_APE_SHIM_PROCFS_XNU_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "procfs.h"

// ---------------------------------------------------------------------------
// Structures copied from the macOS 14 SDK headers, sizes verified against a
// live macOS 26 dump (procfs-macos-todo.md). Apple marks them unstable but
// only appends fields, and every call negotiates by size.

struct xnu_proc_bsdinfo { // <sys/proc_info.h>, 136 bytes
    uint32_t pbi_flags;
    uint32_t pbi_status;
    uint32_t pbi_xstatus;
    uint32_t pbi_pid;
    uint32_t pbi_ppid;
    uint32_t pbi_uid;
    uint32_t pbi_gid;
    uint32_t pbi_ruid;
    uint32_t pbi_rgid;
    uint32_t pbi_svuid;
    uint32_t pbi_svgid;
    uint32_t rfu_1;
    char pbi_comm[16];
    char pbi_name[32];
    uint32_t pbi_nfiles;
    uint32_t pbi_pgid;
    uint32_t pbi_pjobc;
    uint32_t e_tdev;
    uint32_t e_tpgid;
    int32_t pbi_nice;
    uint64_t pbi_start_tvsec;
    uint64_t pbi_start_tvusec;
};

struct xnu_proc_taskinfo { // <sys/proc_info.h>, 96 bytes
    uint64_t pti_virtual_size;
    uint64_t pti_resident_size;
    uint64_t pti_total_user; // mach absolute time units
    uint64_t pti_total_system;
    uint64_t pti_threads_user;
    uint64_t pti_threads_system;
    int32_t pti_policy;
    int32_t pti_faults;
    int32_t pti_pageins;
    int32_t pti_cow_faults;
    int32_t pti_messages_sent;
    int32_t pti_messages_received;
    int32_t pti_syscalls_mach;
    int32_t pti_syscalls_unix;
    int32_t pti_csw;
    int32_t pti_threadnum;
    int32_t pti_numrunning;
    int32_t pti_priority;
};

struct xnu_rusage_info_v2 { // <sys/resource.h>, 160 bytes
    uint8_t ri_uuid[16];
    uint64_t ri_user_time;
    uint64_t ri_system_time;
    uint64_t ri_pkg_idle_wkups;
    uint64_t ri_interrupt_wkups;
    uint64_t ri_pageins;
    uint64_t ri_wired_size;
    uint64_t ri_resident_size;
    uint64_t ri_phys_footprint;
    uint64_t ri_proc_start_abstime;
    uint64_t ri_proc_exit_abstime;
    uint64_t ri_child_user_time;
    uint64_t ri_child_system_time;
    uint64_t ri_child_pkg_idle_wkups;
    uint64_t ri_child_interrupt_wkups;
    uint64_t ri_child_pageins;
    uint64_t ri_child_elapsed_abstime;
    uint64_t ri_diskio_bytesread;
    uint64_t ri_diskio_byteswritten;
};

struct xnu_vm_statistics64 { // <mach/vm_statistics.h>, 152 bytes
    uint32_t free_count;
    uint32_t active_count;
    uint32_t inactive_count;
    uint32_t wire_count;
    uint64_t zero_fill_count;
    uint64_t reactivations;
    uint64_t pageins;
    uint64_t pageouts;
    uint64_t faults;
    uint64_t cow_faults;
    uint64_t lookups;
    uint64_t hits;
    uint64_t purges;
    uint32_t purgeable_count;
    uint32_t speculative_count;
    uint64_t decompressions;
    uint64_t compressions;
    uint64_t swapins;
    uint64_t swapouts;
    uint32_t compressor_page_count;
    uint32_t throttled_count;
    uint32_t external_page_count;
    uint32_t internal_page_count;
    uint64_t total_uncompressed_pages_in_compressor;
} __attribute__((__aligned__(8)));

struct xnu_xsw_usage { // <sys/sysctl.h>, vm.swapusage, 32 bytes
    uint64_t xsu_total, xsu_avail, xsu_used;
    uint32_t xsu_pagesize;
    uint32_t xsu_encrypted;
};

struct xnu_proc_fdinfo { // <sys/proc_info.h>, PROC_PIDLISTFDS entries
    int32_t proc_fd;
    uint32_t proc_fdtype; // PROX_FDTYPE_*
};

struct xnu_vinfo_stat { // <sys/proc_info.h>
    uint32_t vst_dev;
    uint16_t vst_mode, vst_nlink;
    uint64_t vst_ino;
    uint32_t vst_uid, vst_gid;
    int64_t vst_atime, vst_atimensec;
    int64_t vst_mtime, vst_mtimensec;
    int64_t vst_ctime, vst_ctimensec;
    int64_t vst_birthtime, vst_birthtimensec;
    int64_t vst_size, vst_blocks;
    int32_t vst_blksize;
    uint32_t vst_flags, vst_gen, vst_rdev;
    int64_t vst_qspare[2];
};

struct xnu_vnode_fdinfowithpath { // <sys/proc_info.h>, 1200 bytes
    struct {
        uint32_t fi_openflags, fi_status;
        int64_t fi_offset;
        int32_t fi_type;
        uint32_t fi_guardflags;
    } pfi;
    struct {
        struct xnu_vinfo_stat vi_stat;
        int32_t vi_type, vi_pad;
        uint64_t vi_fsid;
    } vip_vi;
    char vip_path[1024];
};

struct xnu_proc_vnodepathinfo { // <sys/proc_info.h>, 2352 bytes
    struct {
        struct {
            struct xnu_vinfo_stat vi_stat;
            int32_t vi_type, vi_pad;
            uint64_t vi_fsid;
        } vip_vi;
        char vip_path[1024];
    } pvi_cdir, pvi_rdir;
};

// The leading fields of PROC_PIDFDSOCKETINFO's answer. The kernel writes a
// full socket_fdinfo whose trailing union covers protocols we never read,
// so callers pass a buffer with headroom and only these offsets matter.
struct xnu_sockbuf_info { // <sys/proc_info.h>, 24 bytes
    uint32_t sbi_cc, sbi_hiwat, sbi_mbcnt, sbi_mbmax, sbi_lowat;
    int16_t sbi_flags, sbi_timeo;
};

struct xnu_in_sockinfo { // <sys/proc_info.h>, 80 bytes
    int32_t insi_fport, insi_lport; // network byte order in the low half
    uint64_t insi_gencnt;
    uint32_t insi_flags;
    uint32_t insi_flow;
    uint8_t insi_vflag; // INI_IPV4 1, INI_IPV6 2
    uint8_t insi_ip_ttl;
    uint32_t rfu_1;
    union {
        struct { uint32_t pad[3]; uint32_t addr4; } a4;
        uint8_t a6[16];
    } insi_faddr, insi_laddr;
    struct { uint8_t in4_tos; } insi_v4;
    struct {
        uint8_t in6_hlim;
        int32_t in6_cksum;
        uint16_t in6_ifindex;
        int16_t in6_hops;
    } insi_v6;
};

struct xnu_socket_fdinfo { // socket_fdinfo up to the protocol union
    struct {
        uint32_t fi_openflags, fi_status;
        int64_t fi_offset;
        int32_t fi_type;
        uint32_t fi_guardflags;
    } pfi;
    struct {
        struct xnu_vinfo_stat soi_stat;
        uint64_t soi_so, soi_pcb;
        int32_t soi_type, soi_protocol, soi_family;
        int16_t soi_options, soi_linger, soi_state;
        int16_t soi_qlen, soi_incqlen, soi_qlimit, soi_timeo;
        uint16_t soi_error;
        uint32_t soi_oobmark;
        struct xnu_sockbuf_info soi_rcv, soi_snd;
        int32_t soi_kind; // SOCKINFO_IN 1, SOCKINFO_TCP 2
        uint32_t rfu_1;
        union {
            struct xnu_in_sockinfo pri_in; // SOCKINFO_TCP leads with it too
        } soi_proto;
    } psi;
};

// The interface list (sysctl NET_RT_IFLIST2). These two are outside the
// SDK's pack(4) regions and keep natural alignment.
struct xnu_if_data64 { // <net/if_var.h>
    uint8_t ifi_type, ifi_typelen, ifi_physical, ifi_addrlen, ifi_hdrlen;
    uint8_t ifi_recvquota, ifi_xmitquota, ifi_unused1;
    uint32_t ifi_mtu, ifi_metric;
    uint64_t ifi_baudrate;
    uint64_t ifi_ipackets, ifi_ierrors, ifi_opackets, ifi_oerrors;
    uint64_t ifi_collisions, ifi_ibytes, ifi_obytes;
    uint64_t ifi_imcasts, ifi_omcasts, ifi_iqdrops, ifi_noproto;
    uint32_t ifi_recvtiming, ifi_xmittiming;
    struct { int32_t tv_sec, tv_usec; } ifi_lastchange;
};

struct xnu_if_msghdr2 { // <net/if.h>
    uint16_t ifm_msglen;
    uint8_t ifm_version, ifm_type;
    int32_t ifm_addrs, ifm_flags;
    uint16_t ifm_index;
    int32_t ifm_snd_len, ifm_snd_maxlen, ifm_snd_drops, ifm_timer;
    struct xnu_if_data64 ifm_data;
};

struct xnu_sockaddr_dl { // <net/if_dl.h>
    uint8_t sdl_len, sdl_family;
    uint16_t sdl_index;
    uint8_t sdl_type, sdl_nlen, sdl_alen, sdl_slen;
    char sdl_data[12]; // can be longer; sdl_len says
};

// The socket tables (pcblist64). The SDK wraps these in pack(4).
#pragma pack(4)
struct xnu_xinpgen { // <netinet/in_pcb.h>
    uint32_t xig_len, xig_count;
    uint64_t xig_gen, xig_sogen;
};

struct xnu_xsockbuf { // <sys/socketvar.h>
    uint32_t sb_cc, sb_hiwat, sb_mbcnt, sb_mbmax;
    int32_t sb_lowat;
    int16_t sb_flags, sb_timeo;
};

struct xnu_xsocket64 { // <sys/socketvar.h>
    uint32_t xso_len;
    uint64_t xso_so;
    int16_t so_type, so_options, so_linger, so_state;
    uint64_t so_pcb;
    int32_t xso_protocol, xso_family;
    int16_t so_qlen, so_incqlen, so_qlimit, so_timeo;
    uint16_t so_error;
    int32_t so_pgid;
    uint32_t so_oobmark;
    struct xnu_xsockbuf so_rcv, so_snd;
    uint32_t so_uid;
};

struct xnu_inpcb64_le {
    uint64_t le_next, le_prev;
};

struct xnu_xinpcb64 { // <netinet/in_pcb.h>
    uint64_t xi_len;
    uint64_t xi_inpp;
    uint16_t inp_fport, inp_lport; // network byte order
    struct xnu_inpcb64_le inp_list;
    uint64_t inp_ppcb, inp_pcbinfo;
    struct xnu_inpcb64_le inp_portlist;
    uint64_t inp_phd;
    uint64_t inp_gencnt;
    int32_t inp_flags;
    uint32_t inp_flow;
    uint8_t inp_vflag; // INP_IPV4 1, INP_IPV6 2
    uint8_t inp_ip_ttl, inp_ip_p;
    union {
        struct { uint32_t pad[3]; uint32_t addr4; } a4;
        uint8_t a6[16];
    } inp_dependfaddr, inp_dependladdr;
    struct { uint8_t inp4_ip_tos; } inp_depend4;
    struct {
        uint8_t inp6_hlim;
        int32_t inp6_cksum;
        uint16_t inp6_ifindex;
        int16_t inp6_hops;
    } inp_depend6;
    struct xnu_xsocket64 xi_socket;
    uint64_t xi_alignment_hack;
};

struct xnu_xtcpcb64 { // <netinet/tcp_var.h>; leading fields only, records
                      // advance by xt_len
    uint32_t xt_len;
    struct xnu_xinpcb64 xt_inpcb;
    uint64_t t_segq;
    int32_t t_dupacks;
    int32_t t_timer[4]; // TCPT_NTIMERS_EXT
    int32_t t_state;    // TCPS_*
};
#pragma pack()

#pragma pack(4)
struct xnu_vm_region_submap_info_64 { // <mach/vm_region.h>, pack(4)
    int32_t protection, max_protection, inheritance;
    uint64_t offset;
    uint32_t user_tag, pages_resident, pages_shared_now_private;
    uint32_t pages_swapped_out, pages_dirtied, ref_count;
    uint16_t shadow_depth;
    uint8_t external_pager, share_mode; // SM_*
    int32_t is_submap;
    int32_t behavior;
    uint32_t object_id;
    uint16_t user_wired_count, flags;
    uint32_t pages_reusable;
    uint64_t object_id_full;
};
#pragma pack()

// ---------------------------------------------------------------------------
// xnuapi.c, the data layer.

bool pfs_xnu_ready(void); // the Syslib is present and recent enough
long pfs_xnu_sysctl(const char *name, void *buf, size_t *len); // 0 or -errno
uint64_t pfs_xnu_mach_ns(uint64_t mach); // mach absolute units to ns
uint64_t pfs_xnu_boottime(void);         // unix seconds; 0 unknown
long pfs_xnu_pagesize(void);
int pfs_xnu_hz(void); // the value jiffies must agree with

bool pfs_xnu_bsdinfo(uint32_t pid, struct xnu_proc_bsdinfo *out);
bool pfs_xnu_taskinfo(uint32_t pid, struct xnu_proc_taskinfo *out);
bool pfs_xnu_rusage(uint32_t pid, struct xnu_rusage_info_v2 *out);
long pfs_xnu_pidpath(uint32_t pid, char *buf, size_t n);
bool pfs_xnu_vmstat(struct xnu_vm_statistics64 *out);

struct xnu_cpu_ticks {
    uint64_t user, nice, system, idle; // CLK_TCK units
};
int pfs_xnu_cpu_ticks(struct xnu_cpu_ticks *out, int cap);

// The raw KERN_PROCARGS2 block, malloc'd; the caller frees it.
bool pfs_xnu_procargs(uint32_t pid, char **out, size_t *len);

int pfs_xnu_listfds(uint32_t pid, struct xnu_proc_fdinfo *out, int cap);
long pfs_xnu_fdpath(uint32_t pid, int fd, char *buf, size_t n);
// One socket descriptor's identity, normalized for the shared inode hash.
struct xnu_sock_id {
    uint8_t proto, family; // 0 tcp / 1 udp; 4 / 6
    uint16_t lport, rport;
    uint8_t laddr[16], raddr[16];
};
bool pfs_xnu_fdsock(uint32_t pid, int fd, struct xnu_sock_id *out);
// Thread ids (64-bit here) of any process the caller may see.
int pfs_xnu_threads(uint32_t pid, uint64_t *out, int cap);
// Another process's working directory; ours comes from getcwd.
long pfs_xnu_cwd(uint32_t pid, char *buf, size_t n);
// Whether a path names the ape loader.
bool pfs_xnu_is_loader(const char *path);
// The program a loader-run process actually executes, spelled as it was on
// its command line; -1 when pid does not run under the loader.
long pfs_xnu_ape_program(uint32_t pid, char *buf, size_t n);
// Raw sysctl dumps, malloc'd; the caller frees them.
bool pfs_xnu_iflist2(char **out, size_t *len);
// One address-space region at or after *addr; the depth walks submaps.
bool pfs_xnu_region(uint64_t *addr, uint64_t *size, uint32_t *depth,
                    struct xnu_vm_region_submap_info_64 *info);
long pfs_xnu_regionfile(uint64_t addr, char *buf, size_t n);
bool pfs_xnu_pcblist(const char *name, char **out, size_t *len);

int pfs_xnu_procs(const struct pfs_proc **out);
const struct pfs_proc *pfs_xnu_proc_find(uint32_t pid);

const char *pfs_xnu_kernel_sysname(void);
const char *pfs_xnu_kernel_release(void);
const char *pfs_xnu_kernel_version(void);

// ---------------------------------------------------------------------------
// xnu.c, the generators.

bool pfs_xnu_gen_top_file(struct pfs_buf *b, const char *name);
bool pfs_xnu_gen_net_file(struct pfs_buf *b, const char *name);
bool pfs_xnu_gen_pid_file(struct pfs_buf *b, uint32_t pid, const char *name);
bool pfs_xnu_gen_pid_volatile(uint32_t pid, struct pfs_buf out[4],
                              uint64_t *starttime);
long pfs_xnu_pid_link(uint32_t pid, const char *name, char *buf, size_t n);
int pfs_xnu_fds_of(uint32_t pid, struct pfs_fdent *out, int cap);
int pfs_xnu_ifstats(struct pfs_ifstat *out, int cap);

// The /sys slices, fully virtual here (the NT ones live on its disk
// skeleton). kind: -1 absent, 0 a file, 1 a directory.
bool pfs_xnu_gen_sysfs(struct pfs_buf *b, const char *path);
int pfs_xnu_sysfs_kind(const char *path);
int pfs_xnu_sysfs_list(const char *path, struct pfs_virtent **out);

#endif // RUST_APE_SHIM_PROCFS_XNU_H_
