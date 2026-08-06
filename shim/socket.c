// The socket half of the Linux-personality shim.
//
// Four things cross this boundary: SOCK_CLOEXEC/SOCK_NONBLOCK mixed into
// socket types, the sockopt (level, name) space, the MSG_* send/recv flags,
// and the AF_* address family — as the socket() domain argument AND as the
// family field inside every sockaddr. All are runtime constants under cosmo
// (WSA's on Windows); the Rust world bakes in musl's values.
//
// The sockaddr structs themselves need no repacking: cosmo's sockaddr_in6 is
// declared "Linux+NT ABI" and matches musl field for field — only the family
// VALUE diverges (AF_INET6 is 10 on Linux, 23 on Windows, 30 on XNU). So
// address-taking calls rewrite the family forward on a local copy, and
// address-returning calls (accept, getsockname, recvfrom, getaddrinfo's
// result chain) rewrite it back in place.
//
// getaddrinfo/getnameinfo are cosmo's third_party/musl build: struct
// addrinfo layout and the AI_*/NI_*/EAI_* values are musl's own, so only the
// family fields flowing through them need help.
//
// Values come from tables.h (`cargo xtask gen-shim`).

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <poll.h>
#include <stddef.h>
#include <string.h>
#include <sys/socket.h>
#include <libc/dce.h>
#include <libc/sysv/consts/af.h>
#include <libc/sysv/consts/sock.h>
#include <libc/sysv/consts/so.h>
#include <libc/sysv/consts/sol.h>
#include <libc/sysv/consts/tcp.h>
#include <libc/sysv/consts/ip.h>
#include <libc/sysv/consts/ipv6.h>
#include <libc/sysv/consts/msg.h>

#include "tables.h"

// ---------------------------------------------------------------------------
// Address families. Unknown values pass through raw in both directions —
// pre-shim behavior for families the table doesn't know.

static int af_to_host(int lin) {
#define X(name, linval) if (lin == (linval)) return (int)(name);
    SHIM_AF_TABLE(X)
#undef X
    return lin;
}

static int af_to_linux(int host) {
#define X(name, linval) if (host == (int)(name)) return (linval);
    SHIM_AF_TABLE(X)
#undef X
    return host;
}

// Forward: rewrite the family on a stack copy (the caller's buffer is const).
// `sa_family` is a uint16 at offset 0 in every sockaddr variant.
struct fam_copy {
    char buf[128]; // covers sockaddr_storage
};

static const void *addr_to_host(const void *addr, unsigned len, struct fam_copy *tmp) {
    if (!addr || len < 2 || len > sizeof(tmp->buf)) return addr;
    memcpy(tmp->buf, addr, len);
    unsigned short *fam = (unsigned short *)tmp->buf;
    *fam = (unsigned short)af_to_host(*fam);
    return tmp->buf;
}

// Reverse: patch the family in place after the host filled the buffer in.
static void addr_to_linux(void *addr, const unsigned *len) {
    if (!addr || !len || *len < 2) return;
    unsigned short *fam = (unsigned short *)addr;
    *fam = (unsigned short)af_to_linux(*fam);
}

// SOCK_STREAM/DGRAM/... are the same numbers everywhere; only the two
// high-bit modifiers musl borrows from O_* need translating.
static int type_to_host(int lin, int *out) {
    int host = lin & ~(SHIM_LIN_SOCK_CLOEXEC | SHIM_LIN_SOCK_NONBLOCK);
    if (lin & SHIM_LIN_SOCK_CLOEXEC) host |= SOCK_CLOEXEC;
    if (lin & SHIM_LIN_SOCK_NONBLOCK) host |= SOCK_NONBLOCK;
    *out = host;
    return 0;
}

int __ape_shim_socket(int domain, int lin_type, int protocol) {
    int type;
    type_to_host(lin_type, &type);
    return socket(af_to_host(domain), type, protocol);
}

int __ape_shim_socketpair(int domain, int lin_type, int protocol, int sv[2]) {
    int type;
    type_to_host(lin_type, &type);
    return socketpair(af_to_host(domain), type, protocol, sv);
}

int __ape_shim_accept4(int fd, void *addr, unsigned *len, int lin_flags) {
    int flags = 0;
    if (lin_flags & SHIM_LIN_SOCK_CLOEXEC) flags |= SOCK_CLOEXEC;
    if (lin_flags & SHIM_LIN_SOCK_NONBLOCK) flags |= SOCK_NONBLOCK;
    int r = accept4(fd, addr, len, flags);
    if (r >= 0) addr_to_linux(addr, len);
    return r;
}

int __ape_shim_accept(int fd, void *addr, unsigned *len) {
    int r = accept(fd, addr, len);
    if (r >= 0) addr_to_linux(addr, len);
    return r;
}

int __ape_shim_bind(int fd, const void *addr, unsigned len) {
    struct fam_copy tmp;
    return bind(fd, addr_to_host(addr, len, &tmp), len);
}

int __ape_shim_connect(int fd, const void *addr, unsigned len) {
    struct fam_copy tmp;
    return connect(fd, addr_to_host(addr, len, &tmp), len);
}

int __ape_shim_getsockname(int fd, void *addr, unsigned *len) {
    int r = getsockname(fd, addr, len);
    if (r == 0) addr_to_linux(addr, len);
    return r;
}

int __ape_shim_getpeername(int fd, void *addr, unsigned *len) {
    int r = getpeername(fd, addr, len);
    if (r == 0) addr_to_linux(addr, len);
    return r;
}

// The sockopt space: translate (level, name) from musl's numbering to the
// host's. SOL_SOCKET is its own level value; the IPPROTO_* levels are real
// protocol numbers, identical everywhere. Unknown names pass through raw,
// matching pre-shim behavior rather than inventing failures for exotic
// options.
struct opt {
    short lin;
    const int *host;
};

#define X(name, lin) { lin, &name },
static const struct opt kSo[] = { SHIM_SO_TABLE(X) };
static const struct opt kTcp[] = { SHIM_TCP_TABLE(X) };
static const struct opt kIp[] = { SHIM_IP_TABLE(X) };
static const struct opt kIpv6[] = { SHIM_IPV6_TABLE(X) };
#undef X

static int lookup(const struct opt *t, size_t n, int lin) {
    for (size_t i = 0; i < n; i++)
        if (t[i].lin == lin) return *t[i].host;
    return lin;
}
#define LOOKUP(t, lin) lookup(t, sizeof(t) / sizeof(t[0]), lin)

static void sockopt_to_host(int *level, int *name) {
    switch (*level) {
        case SHIM_LIN_SOL_SOCKET:
            *level = SOL_SOCKET;
            *name = LOOKUP(kSo, *name);
            break;
        case SHIM_LIN_IPPROTO_TCP:
            *name = LOOKUP(kTcp, *name);
            break;
        case SHIM_LIN_IPPROTO_IP:
            *name = LOOKUP(kIp, *name);
            break;
        case SHIM_LIN_IPPROTO_IPV6:
            *name = LOOKUP(kIpv6, *name);
            break;
        default:
            break; // other levels are protocol numbers; pass through
    }
}

int __ape_shim_setsockopt(int fd, int level, int name, const void *val, unsigned len) {
    sockopt_to_host(&level, &name);
    return setsockopt(fd, level, name, val, len);
}

int __ape_shim_getsockopt(int fd, int level, int name, void *val, unsigned *len) {
    int lin_level = level, lin_name = name;
    sockopt_to_host(&level, &name);
    int r = getsockopt(fd, level, name, val, len);
    // SO_ERROR's payload IS an errno, in host coding; hand back the Linux one
    // so raw_os_error comparisons in take_error()/connect_timeout() stay true.
    extern int __ape_shim_errno_host_to_linux(int);
    if (r == 0 && lin_level == SHIM_LIN_SOL_SOCKET && lin_name == SHIM_LIN_SO_ERROR &&
        val && len && *len >= sizeof(int)) {
        *(int *)val = __ape_shim_errno_host_to_linux(*(int *)val);
    }
    return r;
}

// MSG_* flags: OOB/PEEK/DONTROUTE are 1/2/4 everywhere and pass through; the
// rest are runtime. Untranslatable or unknown bits are dropped — for send
// flags they are hints, and failing hard here would break retry loops.
#define LIN_MSG_FIXED 0x7

static int msg_to_host(int lin) {
    int host = lin & LIN_MSG_FIXED;
    lin &= ~LIN_MSG_FIXED;
#define X(name, linval) \
    if (lin & (linval)) { host |= name; lin &= ~(linval); }
    SHIM_MSG_TABLE(X)
#undef X
    return host;
}

// ---------------------------------------------------------------------------
// NT nonblocking-send emulation. cosmo's sys_send_nt hardcodes the
// __winsock_block nonblock argument to false (verified in 4.0.2 and current
// master) — it ignores both the fd's O_NONBLOCK and MSG_DONTWAIT, so a send
// into a full buffer blocks forever; recv honors the flag fine. Every async
// runtime is built on "nonblocking write returns EAGAIN", so this gets
// emulated here. For an O_NONBLOCK fd, ask poll(POLLOUT, 0) first and answer
// EAGAIN when the buffer is full (poll's writability reporting on NT is
// accurate; probed). Residual gap: poll can say writable when fewer than n
// bytes fit, and the blocking send would then wait for the receiver; the
// send length is clamped to 64 KiB in this mode (a legal short write for
// stream sockets, and no UDP datagram is bigger) to keep that window at
// "receiver is reading slower than 64K" rather than unbounded.
//
// Non-Windows hosts skip all of this — one IsWindows() branch.
// Is this an fd the emulation applies to? (NT host, O_NONBLOCK set.)
int __ape_shim_nt_gated(int fd) {
    if (!IsWindows()) return 0;
    int fl = fcntl(fd, F_GETFL);
    return fl != -1 && (fl & O_NONBLOCK);
}

int __ape_shim_nt_wants_eagain(int fd) {
    if (!__ape_shim_nt_gated(fd)) return 0;
    struct pollfd p = { fd, POLLOUT, 0 };
    if (poll(&p, 1, 0) == 0) {
        errno = EAGAIN;
        return 1;
    }
    return 0; // writable (or poll failed: let the real call report)
}

unsigned long __ape_shim_nt_clamp(int fd, unsigned long n) {
    return (n > 65536 && __ape_shim_nt_gated(fd)) ? 65536 : n;
}

long __ape_shim_send(int fd, const void *buf, unsigned long n, int flags) {
    if (__ape_shim_nt_wants_eagain(fd)) return -1;
    return send(fd, buf, __ape_shim_nt_clamp(fd, n), msg_to_host(flags));
}

long __ape_shim_sendto(int fd, const void *buf, unsigned long n, int flags,
                       const void *addr, unsigned alen) {
    struct fam_copy tmp;
    if (__ape_shim_nt_wants_eagain(fd)) return -1;
    return sendto(fd, buf, __ape_shim_nt_clamp(fd, n), msg_to_host(flags),
                  addr_to_host(addr, alen, &tmp), alen);
}

long __ape_shim_recv(int fd, void *buf, unsigned long n, int flags) {
    return recv(fd, buf, n, msg_to_host(flags));
}

long __ape_shim_recvfrom(int fd, void *buf, unsigned long n, int flags,
                         void *addr, unsigned *alen) {
    long r = recvfrom(fd, buf, n, msg_to_host(flags), addr, alen);
    if (r >= 0) addr_to_linux(addr, alen);
    return r;
}

// ---------------------------------------------------------------------------
// sendmsg/recvmsg. struct msghdr and cmsghdr are byte-compatible with musl's
// on LE (cosmo declares them "Linux+NT ABI"; musl's int+pad pairs line up
// with cosmo's u64/u32+pad) — so, like sockaddr, this is value rewriting:
// the MSG_* flags argument, the msg_name family, recvmsg's msg_flags output,
// and the cmsg_level field inside the control payload (SOL_SOCKET is a
// runtime constant, same trap as SO_ERROR's payload). cmsg_type: SCM_RIGHTS
// is 1 on both sides; IPPROTO_IP/IPV6 types go through the sockopt tables;
// anything else passes raw.

static int msg_to_linux(int host) {
    int lin = host & LIN_MSG_FIXED;
    host &= ~LIN_MSG_FIXED;
#define X(name, linval) \
    if (host & name) { lin |= (linval); host &= ~(int)name; }
    SHIM_MSG_TABLE(X)
#undef X
    return lin;
}

static int lookup_to_linux(const struct opt *t, size_t n, int host) {
    for (size_t i = 0; i < n; i++)
        if (*t[i].host == host) return t[i].lin;
    return host;
}
#define LOOKUP_REV(t, host) lookup_to_linux(t, sizeof(t) / sizeof(t[0]), host)

// Walk a control buffer (musl and cosmo agree on the linux cmsghdr shape:
// u32 len, pad, i32 level, i32 type) rewriting level/type in place.
static void cmsgs_rewrite(void *control, unsigned long controllen, int to_host) {
    unsigned char *p = control, *end = p + controllen;
    while (p + 16 <= end) {
        unsigned len = *(unsigned *)p;
        int *level = (int *)(p + 8);
        int *type = (int *)(p + 12);
        if (len < 16) break;
        if (to_host) {
            if (*level == SHIM_LIN_SOL_SOCKET) *level = SOL_SOCKET;
            else if (*level == SHIM_LIN_IPPROTO_IP) *type = LOOKUP(kIp, *type);
            else if (*level == SHIM_LIN_IPPROTO_IPV6) *type = LOOKUP(kIpv6, *type);
        } else {
            if (*level == SOL_SOCKET) *level = SHIM_LIN_SOL_SOCKET;
            else if (*level == SHIM_LIN_IPPROTO_IP) *type = LOOKUP_REV(kIp, *type);
            else if (*level == SHIM_LIN_IPPROTO_IPV6) *type = LOOKUP_REV(kIpv6, *type);
        }
        unsigned long step = (len + 7) & ~7ul; // CMSG_ALIGN
        if (step == 0 || p + step < p) break;
        p += step;
    }
}

long __ape_shim_sendmsg(int fd, const struct msghdr *msg, int flags) {
    if (__ape_shim_nt_wants_eagain(fd)) return -1;
    if (!msg) return sendmsg(fd, msg, msg_to_host(flags));
    struct msghdr m = *msg;
    struct fam_copy name_tmp;
    if (m.msg_name)
        m.msg_name = (void *)addr_to_host(m.msg_name, m.msg_namelen, &name_tmp);
    // control payload: rewrite on a copy — the caller's buffer is logically
    // const. Oversized control data passes through untranslated rather than
    // failing (SOL_SOCKET is 1 == Linux on every cosmo host except the BSDs'
    // 0xffff, and cmsgs beyond this size are exotic).
    unsigned char ctl_tmp[1024];
    if (m.msg_control && m.msg_controllen <= sizeof(ctl_tmp)) {
        memcpy(ctl_tmp, m.msg_control, m.msg_controllen);
        cmsgs_rewrite(ctl_tmp, m.msg_controllen, 1);
        m.msg_control = ctl_tmp;
    }
    return sendmsg(fd, &m, msg_to_host(flags));
}

long __ape_shim_recvmsg(int fd, struct msghdr *msg, int flags) {
    long r = recvmsg(fd, msg, msg_to_host(flags));
    if (r >= 0 && msg) {
        if (msg->msg_name && msg->msg_namelen >= 2) {
            unsigned short *fam = msg->msg_name;
            *fam = (unsigned short)af_to_linux(*fam);
        }
        if (msg->msg_control && msg->msg_controllen)
            cmsgs_rewrite(msg->msg_control, msg->msg_controllen, 0);
        msg->msg_flags = msg_to_linux(msg->msg_flags);
    }
    return r;
}

// ---------------------------------------------------------------------------
// Name resolution. Same addrinfo layout on both sides (cosmo ships musl's
// netdb); the family field is the only host-coded thing passing through.

int __ape_shim_getaddrinfo(const char *node, const char *service,
                           const struct addrinfo *hints, struct addrinfo **res) {
    struct addrinfo h;
    if (hints) {
        h = *hints;
        h.ai_family = af_to_host(h.ai_family);
        hints = &h;
    }
    int r = getaddrinfo(node, service, hints, res);
    if (r == 0 && res) {
        for (struct addrinfo *ai = *res; ai; ai = ai->ai_next) {
            ai->ai_family = af_to_linux(ai->ai_family);
            if (ai->ai_addr)
                ai->ai_addr->sa_family = (unsigned short)af_to_linux(ai->ai_addr->sa_family);
        }
    }
    return r;
}

int __ape_shim_getnameinfo(const void *addr, unsigned alen, char *host_out,
                           unsigned hostlen, char *serv, unsigned servlen, int flags) {
    struct fam_copy tmp;
    // NI_* flag values are musl's on both sides (cosmo ships musl's netdb).
    return getnameinfo(addr_to_host(addr, alen, &tmp), alen, host_out, hostlen,
                       serv, servlen, flags);
}
