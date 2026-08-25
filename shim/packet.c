// AF_PACKET emulation on Windows. NT has no equivalent of it.
//
// Linux programs sniff an interface through AF_PACKET, while NT sniffs
// with a raw IP socket in SIO_RCVALL mode, so this file translates between
// the two shapes. An interface with an IPv6 address needs a secondary
// socket, since one raw socket carries one address family; shim/poll.c
// consults this file so polling the visible fd also wakes on that one.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows() and g_fds

#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
#include <libc/calls/internal.h>
#include <libc/dce.h>
#include <libc/nt/iphlpapi.h>
#include <libc/nt/struct/ipadapteraddresses.h>
#include <libc/nt/winsock.h>

// musl's AF_PACKET. Deliberately not in tables.h, since cosmo's constant
// resolves to -1 on NT and translating to it would defeat this file.
#define SHIM_LIN_AF_PACKET 17

// SOL_PACKET and its option namespace, likewise musl's.
#define SHIM_LIN_SOL_PACKET 263

#define SIO_RCVALL 0x98000001u
#define RCVALL_ON 1u

#define ETH_HLEN 14
#define ETH_P_IP 0x0800
#define ETH_P_IPV6 0x86dd

// A raw IP socket only ever delivers IP packets, so the drop path below is
// for a case that should not happen. The bound keeps it from spinning if a
// host ever misbehaves.
#define PKT_DRAIN_MAX 64

#define PKT_MAX 32

struct pkt_sock {
    bool live;
    int fd;        // AF_INET raw socket; the fd the caller holds
    long handle;   // its NT handle, recorded to detect a closed/reused fd
    int fd6;       // AF_INET6 raw socket, or -1
    int ifindex;
    int turn;      // which of the two to drain first, alternated per call
};

static struct pkt_sock g_pkt[PKT_MAX];
static pthread_mutex_t g_pkt_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_pkt_live; // fast path: nonzero only once a packet socket exists

// ---------------------------------------------------------------------------
// Table

// An entry's fd still belongs to it only while cosmo's fd table shows it
// open with the handle it was registered with. Anything else means the fd
// was closed, or reused by an unrelated open.
static bool fd_is(int fd, long handle) {
    if (fd < 0 || (size_t)fd >= g_fds.n) return false;
    if (g_fds.p[fd].kind == 0 /* kFdEmpty */) return false;
    return g_fds.p[fd].handle == handle;
}

static void reap_locked(void) {
    for (int i = 0; i < PKT_MAX; i++) {
        struct pkt_sock *p = &g_pkt[i];
        if (!p->live || fd_is(p->fd, p->handle)) continue;
        if (p->fd6 >= 0) close(p->fd6);
        memset(p, 0, sizeof *p);
        g_pkt_live--;
    }
}

static struct pkt_sock *find_locked(int fd) {
    for (int i = 0; i < PKT_MAX; i++)
        if (g_pkt[i].live && g_pkt[i].fd == fd) return &g_pkt[i];
    return NULL;
}

bool __ape_shim_packet_any(void) {
    return g_pkt_live != 0;
}

bool __ape_shim_packet_is(int fd) {
    if (!g_pkt_live) return false;
    pthread_mutex_lock(&g_pkt_lock);
    reap_locked();
    bool yes = find_locked(fd) != NULL;
    pthread_mutex_unlock(&g_pkt_lock);
    return yes;
}

// The socket shim/poll.c must watch alongside `fd`, or -1. Reported without
// reaping, so a poll on the hot path stays a plain lookup.
int __ape_shim_packet_secondary(int fd) {
    if (!g_pkt_live) return -1;
    pthread_mutex_lock(&g_pkt_lock);
    struct pkt_sock *p = find_locked(fd);
    int fd6 = p ? p->fd6 : -1;
    pthread_mutex_unlock(&g_pkt_lock);
    return fd6;
}

// ---------------------------------------------------------------------------
// Adapter lookup

struct ifaddrs4_6 {
    bool have4, have6;
    struct sockaddr_in a4;
    struct sockaddr_in6 a6;
};

// Both addresses of the adapter whose IfIndex is `ifindex`. IfIndex is the
// number shim/ifname.c reports for if_nametoindex, which is where the caller's
// sll_ifindex came from. A link-local IPv6 address is usable but only with
// its scope, so a global one is preferred and the scope filled in either way.
static bool adapter_addrs(unsigned ifindex, struct ifaddrs4_6 *out) {
    memset(out, 0, sizeof *out);
    uint32_t flags = kNtGaaFlagSkipAnycast | kNtGaaFlagSkipMulticast |
                     kNtGaaFlagSkipDnsServer;
    uint32_t size = 0;
    GetAdaptersAddresses(0 /* AF_UNSPEC */, flags, 0, 0, &size);
    if (!size) return false;
    struct NtIpAdapterAddresses *aa = malloc(size);
    if (!aa) return false;
    bool found = false;
    if (GetAdaptersAddresses(0, flags, 0, aa, &size) == 0) {
        for (struct NtIpAdapterAddresses *p = aa; p; p = p->Next) {
            if (p->IfIndex != ifindex) continue;
            found = true;
            bool v6_global = false;
            for (struct NtIpAdapterUnicastAddress *u = p->FirstUnicastAddress;
                 u; u = u->Next) {
                struct sockaddr *sa = u->Address.lpSockaddr;
                if (!sa) continue;
                if (sa->sa_family == AF_INET && !out->have4) {
                    memcpy(&out->a4, sa, sizeof out->a4);
                    out->a4.sin_port = 0;
                    out->have4 = true;
                } else if (sa->sa_family == AF_INET6 && !v6_global) {
                    struct sockaddr_in6 s6;
                    memcpy(&s6, sa, sizeof s6);
                    s6.sin6_port = 0;
                    bool link_local = s6.sin6_addr.s6_addr[0] == 0xfe &&
                                      (s6.sin6_addr.s6_addr[1] & 0xc0) == 0x80;
                    if (out->have6 && link_local) continue;
                    if (!s6.sin6_scope_id)
                        s6.sin6_scope_id = p->Ipv6IfIndex ? p->Ipv6IfIndex : ifindex;
                    out->a6 = s6;
                    out->have6 = true;
                    v6_global = !link_local;
                }
            }
            break;
        }
    }
    free(aa);
    return found;
}

// bind + SIO_RCVALL. Returns 0, or -1 with errno set by whichever step failed.
static int sniff_on(int fd, const struct sockaddr *sa, unsigned len) {
    if (bind(fd, sa, len) == -1) return -1;
    uint32_t on = RCVALL_ON, got = 0;
    if (WSAIoctl(g_fds.p[fd].handle, SIO_RCVALL, &on, sizeof on, 0, 0, &got, 0,
                 0) != 0)
        return -1;
    return 0;
}

// ---------------------------------------------------------------------------
// The calls shim/socket.c routes here

int __ape_shim_packet_socket(int lin_type, int protocol) {
    (void)protocol; // ETH_P_ALL and friends have no NT counterpart
    // SOCK_DGRAM on AF_PACKET asks for cooked packets, i.e. the same IP
    // packets minus the header this file synthesizes. Both shapes are served
    // the same way; only the framing differs, and framing is what recv does.
    (void)lin_type;

    int fd = socket(AF_INET, SOCK_RAW, IPPROTO_IP);
    if (fd == -1) return -1; // EACCES here means "not an Administrator"

    pthread_mutex_lock(&g_pkt_lock);
    reap_locked();
    struct pkt_sock *p = NULL;
    for (int i = 0; i < PKT_MAX; i++)
        if (!g_pkt[i].live) { p = &g_pkt[i]; break; }
    if (!p) {
        pthread_mutex_unlock(&g_pkt_lock);
        close(fd);
        errno = EMFILE;
        return -1;
    }
    memset(p, 0, sizeof *p);
    p->live = true;
    p->fd = fd;
    p->handle = g_fds.p[fd].handle;
    p->fd6 = -1;
    g_pkt_live++;
    pthread_mutex_unlock(&g_pkt_lock);
    return fd;
}

int __ape_shim_packet_bind(int fd, const void *addr, unsigned len) {
    // musl's sockaddr_ll: u16 family, u16 protocol, i32 ifindex, ...
    if (!addr || len < 8) return errno = EINVAL, -1;
    int ifindex;
    memcpy(&ifindex, (const char *)addr + 4, sizeof ifindex);
    if (ifindex <= 0) return errno = EINVAL, -1; // "every interface" has no NT form

    struct ifaddrs4_6 ia;
    if (!adapter_addrs((unsigned)ifindex, &ia)) return errno = ENODEV, -1;

    pthread_mutex_lock(&g_pkt_lock);
    struct pkt_sock *p = find_locked(fd);
    if (!p) {
        pthread_mutex_unlock(&g_pkt_lock);
        return errno = ENOTSOCK, -1;
    }
    p->ifindex = ifindex;
    int fd4 = p->fd;
    pthread_mutex_unlock(&g_pkt_lock);

    int err4 = 0;
    bool ok4 = false;
    if (ia.have4) {
        if (sniff_on(fd4, (struct sockaddr *)&ia.a4, sizeof ia.a4) == 0)
            ok4 = true;
        else
            err4 = errno;
    } else {
        err4 = EADDRNOTAVAIL;
    }

    // The IPv6 half is best effort: an adapter without an IPv6 address, or a
    // host that refuses SIO_RCVALL on one, simply reports no IPv6 traffic.
    int fd6 = -1;
    if (ia.have6) {
        fd6 = socket(AF_INET6, SOCK_RAW, IPPROTO_IPV6);
        if (fd6 != -1 &&
            sniff_on(fd6, (struct sockaddr *)&ia.a6, sizeof ia.a6) != 0) {
            close(fd6);
            fd6 = -1;
        }
        // Always nonblocking, since a blocking recv on the socket the caller
        // does not know about could park it forever. Blocking callers are
        // served in packet_recv, which waits on both sockets at once.
        if (fd6 != -1) fcntl(fd6, F_SETFL, fcntl(fd6, F_GETFL) | O_NONBLOCK);
    }

    if (!ok4 && fd6 == -1) {
        errno = err4 ? err4 : EADDRNOTAVAIL;
        return -1;
    }

    pthread_mutex_lock(&g_pkt_lock);
    p = find_locked(fd);
    if (p) {
        p->fd6 = fd6;
    } else if (fd6 != -1) {
        close(fd6); // the fd was closed under us mid-bind
    }
    pthread_mutex_unlock(&g_pkt_lock);
    return 0;
}

// Every SOL_PACKET option is either already true of an SIO_RCVALL socket
// (PACKET_ADD_MEMBERSHIP/PACKET_MR_PROMISC) or a tuning knob with no NT
// counterpart (fanout, ring buffers). Failing them would abort a capture that
// is about to work, so they succeed and do nothing.
bool __ape_shim_packet_sockopt(int fd, int level) {
    return level == SHIM_LIN_SOL_PACKET && __ape_shim_packet_is(fd);
}

// ---------------------------------------------------------------------------
// Receive

// One IP packet from `fd`, framed in place at the front of `buf`. Returns the
// framed length; -1/EAGAIN when the socket is dry. `*consumed` counts
// datagrams taken off the socket, framed or not, which is what tells a caller
// that reported-readable-but-nothing-to-report apart from a lying poll.
// `*unframeable` is set when the socket delivered something that is not an IP
// header at all.
static long recv_framed(int fd, unsigned char *buf, unsigned long n,
                        int *consumed, int *unframeable) {
    if (fd < 0) return errno = EAGAIN, -1;
    if (n <= ETH_HLEN) return errno = EINVAL, -1;

    for (int i = 0; i < PKT_DRAIN_MAX; i++) {
        long r = recv(fd, buf + ETH_HLEN, n - ETH_HLEN, 0);
        if (r < 0) return -1; // EAGAIN and real errors alike
        (*consumed)++;
        if (r == 0) continue;

        unsigned ver = buf[ETH_HLEN] >> 4;
        unsigned et;
        if (ver == 4 && r >= 20)
            et = ETH_P_IP;
        else if (ver == 6 && r >= 40)
            et = ETH_P_IPV6;
        else {
            *unframeable = 1;
            continue; // not a header we can frame; drop it and look again
        }

        memset(buf, 0, 12); // no MACs: the capture is above the link layer
        buf[12] = (unsigned char)(et >> 8);
        buf[13] = (unsigned char)(et & 0xff);
        return r + ETH_HLEN;
    }
    return errno = EAGAIN, -1;
}

// The address a packet socket reports is the sockaddr_ll it was bound to.
// Callers use it to tell interfaces apart when one socket covers several,
// which this never does, so the interface index is the whole of it.
static void fill_sll(void *addr, unsigned *alen, int ifindex, unsigned et) {
    if (!addr || !alen) return;
    unsigned char sll[20];
    memset(sll, 0, sizeof sll);
    sll[0] = SHIM_LIN_AF_PACKET & 0xff;
    sll[1] = (SHIM_LIN_AF_PACKET >> 8) & 0xff;
    sll[2] = (unsigned char)(et >> 8); // sll_protocol, network byte order
    sll[3] = (unsigned char)(et & 0xff);
    memcpy(sll + 4, &ifindex, sizeof ifindex);
    unsigned n = *alen < sizeof sll ? *alen : (unsigned)sizeof sll;
    memcpy(addr, sll, n);
    *alen = sizeof sll;
}

// Retire a secondary that delivered something this file cannot frame. NT is
// documented to hand an AF_INET6 raw socket the payload without the IPv6
// header on some configurations, and there is no way to ask in advance; the
// first packet answers it. Keeping the socket would mean draining and
// dropping its traffic forever, which costs the caller real time and reports
// nothing, so it goes away and the capture continues over IPv4 alone.
// https://learn.microsoft.com/en-us/windows/win32/winsock/tcp-ip-raw-sockets-2#send-and-receive-operations
static void drop_secondary(int fd) {
    pthread_mutex_lock(&g_pkt_lock);
    struct pkt_sock *p = find_locked(fd);
    if (p && p->fd6 >= 0) {
        close(p->fd6);
        p->fd6 = -1;
        p->turn = 0;
    }
    pthread_mutex_unlock(&g_pkt_lock);
}

long __ape_shim_packet_recv(int fd, void *buf, unsigned long n, void *addr,
                            unsigned *alen) {
    for (;;) {
        pthread_mutex_lock(&g_pkt_lock);
        struct pkt_sock *p = find_locked(fd);
        if (!p) {
            pthread_mutex_unlock(&g_pkt_lock);
            return errno = ENOTSOCK, -1;
        }
        int first = p->turn;
        int fds[2] = {p->fd, p->fd6};
        int ifindex = p->ifindex;
        p->turn = p->fd6 >= 0 ? !p->turn : 0; // neither family starves the other
        pthread_mutex_unlock(&g_pkt_lock);

        int consumed = 0, unframeable = 0, bad6 = 0;
        for (int i = 0; i < 2; i++) {
            int which = first ^ i;
            int before = unframeable;
            long r = recv_framed(fds[which], (unsigned char *)buf, n, &consumed,
                                 &unframeable);
            if (r >= 0) {
                unsigned char *b = buf;
                fill_sll(addr, alen, ifindex, ((unsigned)b[12] << 8) | b[13]);
                return r;
            }
            if (which == 1 && unframeable > before) bad6 = 1;
            if (errno != EAGAIN && errno != EWOULDBLOCK) return -1;
        }

        if (bad6) drop_secondary(fd);

        // Datagrams came off a socket but none could be framed. A caller
        // that polled the fd readable treats EAGAIN as the channel breaking,
        // so answer with an empty frame instead, which parses to no packet
        // and lets its loop continue.
        if (consumed) return 0;

        // Genuinely dry. A blocking caller never reaches this point, since a
        // blocking recv on the fd it holds does not return EAGAIN. Only a
        // nonblocking caller gets here, and EAGAIN is exactly its answer, so
        // this loop always terminates.
        return errno = EAGAIN, -1;
    }
}
