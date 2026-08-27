// /proc/net/{tcp,tcp6,udp,udp6}, and the socket-ownership facts behind
// /proc/<pid>/fd. NT keeps both in GetExtendedTcpTable/GetExtendedUdpTable,
// which pair every socket with its owning pid; this file reads them into one
// row snapshot and emits the kernel's text format from it.
//
// Inodes are a hash of what identifies the socket, not a counter, so the
// number a caller read from /proc/<pid>/fd/<n> still joins up with the one
// in /proc/net/tcp even when a refresh lands between the two reads.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libc/dce.h>
#include <libc/nt/thunk/msabi.h> // __msabi

#include "procfs.h"

#define NET_REFRESH_MS 250
#define MAX_ROWS 4096

// TCP_TABLE_OWNER_PID_ALL / UDP_TABLE_OWNER_PID, and the two address
// families as Winsock numbers them.
#define TCP_TABLE_OWNER_PID_ALL 5
#define UDP_TABLE_OWNER_PID 1
#define NT_AF_INET 2
#define NT_AF_INET6 23

typedef uint32_t (__msabi *GetExtTableF)(void *, uint32_t *, int32_t,
                                         uint32_t, uint32_t, uint32_t);

const char *const pfs_net_files[] = {"tcp",  "tcp6", "udp",
                                     "udp6", "dev",  "unix", 0};

struct row {
    uint8_t proto;  // 0 tcp, 1 udp
    uint8_t family; // 4 or 6
    uint8_t state;  // already Linux-coded
    uint32_t pid;
    uint16_t lport, rport;        // host order
    uint8_t laddr[16], raddr[16]; // network order
    uint64_t inode;
};

static pthread_mutex_t g_net_lock = PTHREAD_MUTEX_INITIALIZER;
static struct row g_rows[MAX_ROWS];
static int g_nrows;
static int64_t g_net_ms;

static uint64_t fnv(const void *p, size_t n, uint64_t h) {
    const uint8_t *b = p;
    for (size_t i = 0; i < n; i++) {
        h ^= b[i];
        h *= 0x100000001b3ull;
    }
    return h;
}

// MIB_TCP_STATE is its own numbering; these are the kernel's.
static uint8_t tcp_state_to_linux(uint32_t nt) {
    switch (nt) {
        case 2: return 0x0a;  // LISTEN
        case 3: return 0x02;  // SYN_SENT
        case 4: return 0x03;  // SYN_RCVD
        case 5: return 0x01;  // ESTAB
        case 6: return 0x04;  // FIN_WAIT1
        case 7: return 0x05;  // FIN_WAIT2
        case 8: return 0x08;  // CLOSE_WAIT
        case 9: return 0x0b;  // CLOSING
        case 10: return 0x09; // LAST_ACK
        case 11: return 0x06; // TIME_WAIT
        default: return 0x07; // CLOSED, DELETE_TCB, anything new
    }
}

static uint16_t nt_port(uint32_t p) {
    return (uint16_t)(((p & 0xff) << 8) | ((p >> 8) & 0xff));
}

static void add_row(uint8_t proto, uint8_t family, uint8_t state,
                    uint32_t pid, const void *laddr, uint16_t lport,
                    const void *raddr, uint16_t rport) {
    if (g_nrows >= MAX_ROWS) return;
    struct row *r = &g_rows[g_nrows];
    memset(r, 0, sizeof *r);
    r->proto = proto;
    r->family = family;
    r->state = state;
    r->pid = pid;
    r->lport = lport;
    r->rport = rport;
    memcpy(r->laddr, laddr, family == 4 ? 4 : 16);
    if (raddr) memcpy(r->raddr, raddr, family == 4 ? 4 : 16);
    r->inode = pfs_net_inode(proto, family, pid, lport, rport, r->laddr,
                             r->raddr);
    g_nrows++;
}

// A socket's identity as its inode number. The remote end is in it too: one
// listening process can hold many connections from the same local port, and
// they are different sockets. Shared with the fd machinery so a descriptor
// in our own fd table joins up with the row the tables report for it.
uint64_t pfs_net_inode(uint8_t proto, uint8_t family, uint32_t pid,
                       uint16_t lport, uint16_t rport,
                       const uint8_t laddr[16], const uint8_t raddr[16]) {
    uint64_t h = fnv(&proto, 1, 0xcbf29ce484222325ull);
    h = fnv(&family, 1, h);
    h = fnv(&pid, 4, h);
    h = fnv(&lport, 2, h);
    h = fnv(&rport, 2, h);
    h = fnv(laddr, 16, h);
    h = fnv(raddr, 16, h);
    h &= 0x7fffffffffffull; // stays inside what a u64 parse takes
    return h ? h : 1;
}

struct nt_tcp4 { uint32_t state, laddr, lport, raddr, rport, pid; };
struct nt_tcp6 {
    uint8_t laddr[16]; uint32_t lscope, lport;
    uint8_t raddr[16]; uint32_t rscope, rport, state, pid;
};
struct nt_udp4 { uint32_t laddr, lport, pid; };
struct nt_udp6 { uint8_t laddr[16]; uint32_t lscope, lport, pid; };

static void *fetch(GetExtTableF f, uint32_t af, uint32_t cls) {
    if (!f) return 0;
    uint32_t size = 0;
    f(0, &size, 0, af, cls, 0);
    if (!size) return 0;
    void *buf = malloc(size);
    if (!buf) return 0;
    if (f(buf, &size, 0, af, cls, 0) != 0) {
        free(buf);
        return 0;
    }
    return buf;
}

static void net_refresh_locked(void) {
    int64_t t = pfs_now_ms();
    if (g_net_ms && t - g_net_ms < NET_REFRESH_MS) return;

    GetExtTableF tcp =
        (GetExtTableF)pfs_sym(u"iphlpapi.dll", "GetExtendedTcpTable");
    GetExtTableF udp =
        (GetExtTableF)pfs_sym(u"iphlpapi.dll", "GetExtendedUdpTable");
    void *b;
    static const uint8_t zero[16] = {0};

    g_nrows = 0;
    if ((b = fetch(tcp, NT_AF_INET, TCP_TABLE_OWNER_PID_ALL))) {
        uint32_t n = *(uint32_t *)b;
        struct nt_tcp4 *r = (struct nt_tcp4 *)((char *)b + 4);
        for (uint32_t i = 0; i < n; i++)
            add_row(0, 4, tcp_state_to_linux(r[i].state), r[i].pid,
                    &r[i].laddr, nt_port(r[i].lport), &r[i].raddr,
                    nt_port(r[i].rport));
        free(b);
    }
    if ((b = fetch(tcp, NT_AF_INET6, TCP_TABLE_OWNER_PID_ALL))) {
        uint32_t n = *(uint32_t *)b;
        struct nt_tcp6 *r = (struct nt_tcp6 *)((char *)b + 4);
        for (uint32_t i = 0; i < n; i++)
            add_row(0, 6, tcp_state_to_linux(r[i].state), r[i].pid,
                    r[i].laddr, nt_port(r[i].lport), r[i].raddr,
                    nt_port(r[i].rport));
        free(b);
    }
    if ((b = fetch(udp, NT_AF_INET, UDP_TABLE_OWNER_PID))) {
        uint32_t n = *(uint32_t *)b;
        struct nt_udp4 *r = (struct nt_udp4 *)((char *)b + 4);
        for (uint32_t i = 0; i < n; i++)
            add_row(1, 4, 0x07, r[i].pid, &r[i].laddr, nt_port(r[i].lport),
                    zero, 0);
        free(b);
    }
    if ((b = fetch(udp, NT_AF_INET6, UDP_TABLE_OWNER_PID))) {
        uint32_t n = *(uint32_t *)b;
        struct nt_udp6 *r = (struct nt_udp6 *)((char *)b + 4);
        for (uint32_t i = 0; i < n; i++)
            add_row(1, 6, 0x07, r[i].pid, r[i].laddr, nt_port(r[i].lport),
                    zero, 0);
        free(b);
    }
    g_net_ms = pfs_now_ms();
}

// Linux prints an address as the %08X of each 4-byte group read in host
// order, which on a little-endian machine reverses each group.
static void emit_addr(struct pfs_buf *b, const uint8_t *a, int family) {
    int groups = family == 4 ? 1 : 4;
    for (int g = 0; g < groups; g++)
        for (int i = 3; i >= 0; i--)
            pfs_printf(b, "%02X", a[g * 4 + i]);
}

static bool gen_net_dev(struct pfs_buf *b);

bool pfs_gen_net_file(struct pfs_buf *b, const char *name) {
    uint8_t proto, family;
    if (!strcmp(name, "dev")) return gen_net_dev(b);
    if (!strcmp(name, "unix")) // header only: NT cannot enumerate these
        return pfs_printf(b, "Num       RefCount Protocol Flags    Type St "
                             "Inode Path\n"),
               true;
    if (!strcmp(name, "tcp")) proto = 0, family = 4;
    else if (!strcmp(name, "tcp6")) proto = 0, family = 6;
    else if (!strcmp(name, "udp")) proto = 1, family = 4;
    else if (!strcmp(name, "udp6")) proto = 1, family = 6;
    else return false;

    pthread_mutex_lock(&g_net_lock);
    net_refresh_locked();
    pfs_printf(b, "  sl  local_address rem_address   st tx_queue rx_queue "
                  "tr tm->when retrnsmt   uid  timeout inode\n");
    int sl = 0;
    for (int i = 0; i < g_nrows; i++) {
        struct row *r = &g_rows[i];
        if (r->proto != proto || r->family != family) continue;
        pfs_printf(b, "%4d: ", sl++);
        emit_addr(b, r->laddr, family);
        pfs_printf(b, ":%04X ", r->lport);
        emit_addr(b, r->raddr, family);
        pfs_printf(b,
                   ":%04X %02X 00000000:00000000 00:00000000 00000000     0"
                   "        0 %llu 1 0000000000000000 0 0 0 0 0\n",
                   r->rport, r->state, (unsigned long long)r->inode);
    }
    pthread_mutex_unlock(&g_net_lock);
    return true;
}

int pfs_net_fds_of(uint32_t pid, uint64_t *inodes, int cap) {
    pthread_mutex_lock(&g_net_lock);
    net_refresh_locked();
    int n = 0;
    for (int i = 0; i < g_nrows && n < cap; i++)
        if (g_rows[i].pid == pid) inodes[n++] = g_rows[i].inode;
    pthread_mutex_unlock(&g_net_lock);
    return n;
}

// ---------------------------------------------------------------------------
// /proc/net/dev. Two sources joined on the interface index: the adapter list
// (GetAdaptersAddresses) says which interfaces are real -- the raw interface
// table is padded with one shadow row per NDIS filter driver -- and carries
// the FriendlyName cosmo's interface naming is based on; the interface table
// (GetIfTable2) carries the 64-bit counters. Row layout facts are from a
// probe, not headers: rows are 1352 bytes, and the trailing block is 18
// 64-bit counters preceded by the two link speeds, so InOctets sits at
// row+1208. Byte and packet counters are real; the merge/fifo/frame columns
// have no NT equivalent and read 0.

#include <libc/mem/mem.h>
#include <libc/nt/iphlpapi.h>
#include <libc/nt/struct/ipadapteraddresses.h>

#define IF_ROW2_SIZE 1352
#define IF_ROW2_INDEX 8      // InterfaceIndex (u32)
#define IF_ROW2_INOCTETS 1208 // then 8 more u64s of In-counters
#define IF_ROW2_OUTOCTETS 1280

typedef uint32_t (__msabi *GetIfTable2F)(void **);
typedef void (__msabi *FreeMibTableF)(void *);

static const char *if_row_of(const char *tab, uint32_t index) {
    uint32_t n = *(const uint32_t *)tab;
    for (uint32_t i = 0; i < n; i++) {
        const char *row = tab + 8 + (size_t)i * IF_ROW2_SIZE;
        if (*(const uint32_t *)(row + IF_ROW2_INDEX) == index) return row;
    }
    return 0;
}

// cosmo's spelling of an NT interface name: FriendlyName with non-name
// characters replaced, cut to IFNAMSIZ-2 (see shim/ifname.c)
static void if_name(char *out, const uint16_t *friendly) {
    int i = 0;
    for (; i < 14 && friendly && friendly[i]; i++) {
        unsigned c = friendly[i];
        out[i] = ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                  (c >= '0' && c <= '9') || c == '-' || c == '.')
                     ? (char)c
                     : '_';
    }
    out[i] = 0;
}

int pfs_net_ifstats(struct pfs_ifstat *out, int cap) {
    static GetIfTable2F table2;
    static FreeMibTableF freetab;
    if (!table2) {
        table2 = (GetIfTable2F)pfs_sym(u"iphlpapi.dll", "GetIfTable2");
        freetab = (FreeMibTableF)pfs_sym(u"iphlpapi.dll", "FreeMibTable");
    }
    if (!table2 || !freetab) return 0;

    uint32_t size = 0;
    GetAdaptersAddresses(0 /* AF_UNSPEC */, 0x8f /* skip everything */, 0, 0,
                         &size);
    if (!size) return 0;
    struct NtIpAdapterAddresses *aa = malloc(size);
    if (!aa) return 0;
    if (GetAdaptersAddresses(0, 0x8f, 0, aa, &size)) {
        free(aa);
        return 0;
    }
    void *tab = 0;
    if (table2(&tab) || !tab) {
        free(aa);
        return 0;
    }

    int n = 0;
    for (struct NtIpAdapterAddresses *p = aa; p && n < cap; p = p->Next) {
        const char *row = if_row_of(tab, p->IfIndex);
        if (!row) continue;
        const uint64_t *in = (const uint64_t *)(row + IF_ROW2_INOCTETS);
        const uint64_t *outc = (const uint64_t *)(row + IF_ROW2_OUTOCTETS);
        // [0] octets [1] ucast pkts [2] nucast pkts [3] discards [4] errors
        struct pfs_ifstat *s = &out[n++];
        if_name(s->name, p->FriendlyName);
        s->rx_bytes = in[0];
        s->rx_pkts = in[1] + in[2];
        s->rx_drop = in[3];
        s->rx_errs = in[4];
        s->tx_bytes = outc[0];
        s->tx_pkts = outc[1] + outc[2];
        s->tx_drop = outc[3];
        s->tx_errs = outc[4];
        s->mtu = p->Mtu;
        s->up = p->OperStatus == 1; // kNtIfOperStatusUp
        s->maclen = p->PhysicalAddressLength > 8 ? 8
                                                 : p->PhysicalAddressLength;
        memcpy(s->mac, p->PhysicalAddress, sizeof s->mac);
        s->speed_mbps = p->TransmitLinkSpeed / 1000000;
    }
    freetab(tab);
    free(aa);
    return n;
}

static bool gen_net_dev(struct pfs_buf *b) {
    struct pfs_ifstat ifs[32];
    int n = pfs_net_ifstats(ifs, 32);
    if (!n) return false;
    pfs_printf(b, "Inter-|   Receive                                       "
                  "         |  Transmit\n"
                  " face |bytes    packets errs drop fifo frame compressed "
                  "multicast|bytes    packets errs drop fifo colls carrier "
                  "compressed\n");
    for (int i = 0; i < n; i++) {
        struct pfs_ifstat *s = &ifs[i];
        pfs_printf(b,
                   "%6s: %7llu %7llu %4llu %4llu    0     0          0     "
                   "    0 %8llu %7llu %4llu %4llu    0     0       0        "
                   "  0\n",
                   s->name, (unsigned long long)s->rx_bytes,
                   (unsigned long long)s->rx_pkts,
                   (unsigned long long)s->rx_errs,
                   (unsigned long long)s->rx_drop,
                   (unsigned long long)s->tx_bytes,
                   (unsigned long long)s->tx_pkts,
                   (unsigned long long)s->tx_errs,
                   (unsigned long long)s->tx_drop);
    }
    return true;
}
