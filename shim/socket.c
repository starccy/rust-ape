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
#include <limits.h>
#include <netdb.h>
#include <poll.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <uchar.h>
#include <unistd.h>
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

#define SUN_PATH_OFF 2
#define SUN_PATH_MAX 108

// cosmo's own path translation, the one open() and friends go through.
int __mkntpath(const char *, char16_t *);

// sockaddr_un is the one address that carries a filename, and cosmo hands
// sun_path to Winsock exactly as given. So an AF_UNIX socket bound to
// anything but a relative path fails on Windows, reported as WSAENETDOWN,
// which surfaces in Rust as "Network is down" for what is really a filename
// in the wrong syntax. Running it through __mkntpath first turns /C/Users/...
// and /tmp/... into what NT expects; Winsock takes the result and echoes it
// back verbatim from getsockname, so a bind/connect pair stays consistent.
//
// Bailing out leaves the path as it was, which is no worse than not
// translating: non-ASCII can't be expressed in sun_path's bytes, and a
// translation that outgrows 108 bytes has nowhere to go.
static void unix_path_to_nt(struct fam_copy *tmp, unsigned *len) {
    char *path = tmp->buf + SUN_PATH_OFF;
    unsigned avail = *len > SUN_PATH_OFF ? *len - SUN_PATH_OFF : 0;
    if (!avail || !*path) return; // unnamed, or Linux's abstract namespace
    if (!memchr(path, '\0', avail)) return;

    char16_t wide[PATH_MAX];
    int n = __mkntpath(path, wide);
    if (n < 0 || n >= SUN_PATH_MAX) return;
    for (int i = 0; i < n; i++)
        if (wide[i] > 0x7f) return;
    for (int i = 0; i < n; i++)
        path[i] = (char)wide[i];
    path[n] = '\0';
    *len = SUN_PATH_OFF + (unsigned)n + 1;
}

static const void *addr_to_host(const void *addr, unsigned *len, struct fam_copy *tmp) {
    if (!addr || *len < 2 || *len > sizeof(tmp->buf)) return addr;
    memcpy(tmp->buf, addr, *len);
    unsigned short *fam = (unsigned short *)tmp->buf;
    *fam = (unsigned short)af_to_host(*fam);
    if (IsWindows() && *fam == AF_UNIX) unix_path_to_nt(tmp, len);
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

// Defined in winsock.c; keeps Windows from tearing Winsock down at exit while
// other threads are still using it. Hooked here because these two are the only
// ways a socket enters the process, and cosmo starts Winsock lazily too.
void __ape_shim_pin_winsock(void);

// shim/epoll.c's edge-triggered arming: EAGAIN from a recv-flavored call
// re-arms the read side, EAGAIN from a send-flavored one the write side, and
// fd creation/duplication retires stale pipe-pair records for that number.
void __ape_shim_epoll_rearm_in(int fd);
void __ape_shim_epoll_rearm_out(int fd);
void __ape_shim_epoll_note_pair(int a, int b);
void __ape_shim_epoll_note_new_fd(int fd);
void __ape_shim_epoll_hint_pipe_write(int fd);

// Data as well as EAGAIN re-arms the read side: a real kernel reports a
// fresh edge when new bytes arrive later, so a consumer that stops at a
// short read instead of draining to EAGAIN must keep getting events. EOF
// and hard errors don't re-arm, keeping HUP/ERR single-shot.
static long recv_result(int fd, long r) {
    if (r > 0 || (r == -1 && errno == EAGAIN)) __ape_shim_epoll_rearm_in(fd);
    return r;
}

// A short send re-arms the write side like EAGAIN does: callers treat a
// partial send as a full buffer and wait for the next writability event,
// and the NT clamp produces partial sends on sockets whose buffers still
// have room. `want` is the original request length, before clamping.
static long send_result(int fd, long r, unsigned long want) {
    if (r > 0) {
        __ape_shim_epoll_hint_pipe_write(fd);
        if ((unsigned long)r < want) __ape_shim_epoll_rearm_out(fd);
    } else if (r == -1 && errno == EAGAIN) {
        __ape_shim_epoll_rearm_out(fd);
    }
    return r;
}

int __ape_shim_socket(int domain, int lin_type, int protocol) {
    int type;
    type_to_host(lin_type, &type);
    __ape_shim_pin_winsock();
    int fd = socket(af_to_host(domain), type, protocol);
    if (fd >= 0) __ape_shim_epoll_note_new_fd(fd);
    return fd;
}

// A pair of AF_UNIX sockets built the ordinary way: listen, connect, accept.
//
// cosmo has its own socketpair on NT, but poll() never reports the fds it
// hands back as writable -- not when fresh, not after a send has blocked, not
// after the peer drains. Readability works, which is why the signal self-pipe
// gets by. Anything that waits for writability, which is every async write,
// waits forever. A pair assembled from a real listener on the same host has
// none of that; the difference is in how the pair is made, not in AF_UNIX.
//
// The listener sits at a random name under the temp directory for the length
// of one connect. A nonce the client sends and the server checks keeps a local
// process that raced us to the path from being accepted in our client's place;
// a connection that fails the check is dropped and the next one considered.
static int io_all(int fd, void *buf, unsigned long n, int writing) {
    unsigned char *p = buf;
    while (n) {
        long r = writing ? send(fd, p, n, 0) : recv(fd, p, n, 0);
        if (r <= 0) return -1;
        p += r;
        n -= (unsigned long)r;
    }
    return 0;
}

static int nt_unix_pair(int type, int sv[2]) {
    unsigned char nonce[16];
    if (getrandom(nonce, sizeof nonce, 0) != (long)sizeof nonce) return -1;

    // /tmp is what cosmo maps to the host's temp directory.
    char path[64];
    int plen = snprintf(path, sizeof path, "/tmp/ape-sp-%d-%02x%02x%02x%02x.sock",
                        (int)getpid(), nonce[0], nonce[1], nonce[2], nonce[3]);
    if (plen < 0 || plen >= (int)sizeof path) return -1;

    struct fam_copy sa;
    memset(&sa, 0, sizeof sa);
    *(unsigned short *)sa.buf = (unsigned short)AF_UNIX;
    unsigned salen = SUN_PATH_OFF + (unsigned)plen + 1;
    memcpy(sa.buf + SUN_PATH_OFF, path, (unsigned long)plen + 1);
    unix_path_to_nt(&sa, &salen);

    int srv = -1, cli = -1, acc = -1, err;
    if ((srv = socket(AF_UNIX, type, 0)) < 0) goto fail;
    if (bind(srv, (const struct sockaddr *)sa.buf, salen) < 0) goto fail;
    if (listen(srv, 4) < 0) goto fail;
    if ((cli = socket(AF_UNIX, type, 0)) < 0) goto fail;
    if (connect(cli, (const struct sockaddr *)sa.buf, salen) < 0) goto fail;
    if (io_all(cli, nonce, sizeof nonce, 1) < 0) goto fail;

    for (int tries = 0; tries < 8 && acc < 0; tries++) {
        int fd = accept4(srv, NULL, NULL, type & SOCK_CLOEXEC);
        if (fd < 0) goto fail;
        unsigned char got[sizeof nonce];
        if (io_all(fd, got, sizeof got, 0) == 0 && !memcmp(got, nonce, sizeof got))
            acc = fd;
        else
            close(fd);
    }
    if (acc < 0) { errno = ECONNABORTED; goto fail; }

    close(srv);
    unlink(path);
    sv[0] = cli;
    sv[1] = acc;
    return 0;

fail:
    err = errno;
    if (srv >= 0) close(srv);
    if (cli >= 0) close(cli);
    if (acc >= 0) close(acc);
    unlink(path);
    errno = err;
    return -1;
}

// cosmo's socketpair rejects SOCK_NONBLOCK on Windows with WSAEOPNOTSUPP,
// though socket() and accept4() both take it there and fcntl() sets O_NONBLOCK
// on the resulting fds without complaint. So the flag is peeled off the type
// and applied afterwards. Done on every host rather than under IsWindows(),
// because the two are equivalent here: nothing else can see these fds between
// the two calls, and SOCK_CLOEXEC, the flag that would care, still goes in
// atomically.
//
// Without this, mio's UnixStream::pair() fails, which takes down every tokio
// program built with the signal feature -- enable_all() stands up the signal
// driver whether or not the program ever waits on a signal.
int __ape_shim_socketpair(int domain, int lin_type, int protocol, int sv[2]) {
    int type;
    type_to_host(lin_type, &type);
    int nonblock = type & SOCK_NONBLOCK;
    type &= ~SOCK_NONBLOCK;

    __ape_shim_pin_winsock();
    int host_domain = af_to_host(domain);
    int made = -1;
    if (IsWindows() && host_domain == AF_UNIX && (type & ~SOCK_CLOEXEC) == SOCK_STREAM)
        made = nt_unix_pair(type, sv);
    // Falling back rather than failing: cosmo's own pair is what shipped
    // before, and it is still better than no pair at all.
    if (made < 0 && socketpair(host_domain, type, protocol, sv) < 0) {
        // XNU's AF_UNIX has no SOCK_SEQPACKET and says so with
        // EPROTONOSUPPORT. std asks for one on every linux target, as the
        // channel a failed exec reports its errno back on, so without this
        // every Command::spawn that skips posix_spawn (a pre_exec closure, a
        // uid, a chroot) dies on macOS before it even forks. A stream pair
        // carries that exchange whole, one write and then EOF from the
        // CLOEXEC close. What it gives up is message boundaries, so a caller
        // that sends two records back to back may read them as one.
        if (!(host_domain == AF_UNIX && (type & ~SOCK_CLOEXEC) == SOCK_SEQPACKET &&
              (errno == EPROTONOSUPPORT || errno == ESOCKTNOSUPPORT ||
               errno == EPROTOTYPE)))
            return -1;
        int stream = (type & SOCK_CLOEXEC) | SOCK_STREAM;
        if (socketpair(host_domain, stream, protocol, sv) < 0) return -1;
    }
    if (nonblock) {
        for (int i = 0; i < 2; i++) {
            int fl = fcntl(sv[i], F_GETFL);
            if (fl < 0 || fcntl(sv[i], F_SETFL, fl | O_NONBLOCK) < 0) {
                int err = errno;
                close(sv[0]);
                close(sv[1]);
                errno = err;
                return -1;
            }
        }
    }
    // Either end may serve as the waker's read side; record both directions.
    __ape_shim_epoll_note_pair(sv[0], sv[1]);
    return 0;
}

int __ape_shim_accept4(int fd, void *addr, unsigned *len, int lin_flags) {
    int flags = 0;
    if (lin_flags & SHIM_LIN_SOCK_CLOEXEC) flags |= SOCK_CLOEXEC;
    if (lin_flags & SHIM_LIN_SOCK_NONBLOCK) flags |= SOCK_NONBLOCK;
    int r = accept4(fd, addr, len, flags);
    if (r >= 0) {
        addr_to_linux(addr, len);
        __ape_shim_epoll_note_new_fd(r);
        __ape_shim_epoll_rearm_in(fd); // more connections may be pending
    } else if (errno == EAGAIN) {
        __ape_shim_epoll_rearm_in(fd);
    }
    return r;
}

int __ape_shim_accept(int fd, void *addr, unsigned *len) {
    int r = accept(fd, addr, len);
    if (r >= 0) {
        addr_to_linux(addr, len);
        __ape_shim_epoll_note_new_fd(r);
        __ape_shim_epoll_rearm_in(fd); // more connections may be pending
    } else if (errno == EAGAIN) {
        __ape_shim_epoll_rearm_in(fd);
    }
    return r;
}

int __ape_shim_bind(int fd, const void *addr, unsigned len) {
    struct fam_copy tmp;
    const void *host = addr_to_host(addr, &len, &tmp);
    return bind(fd, host, len);
}

int __ape_shim_connect(int fd, const void *addr, unsigned len) {
    struct fam_copy tmp;
    const void *host = addr_to_host(addr, &len, &tmp);
    return connect(fd, host, len);
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

// MSG_NOSIGNAL only ever means "don't raise SIGPIPE", and NT has no SIGPIPE
// for cosmo to raise; it hands the flag to WSASend rather than acting on it.
// Winsock tolerates the bit on AF_INET and rejects it on AF_UNIX with
// ERROR_INVALID_PARAMETER, which is how writes to a socketpair failed -- and
// a socketpair is how tokio's signal handler reaches its driver, so every
// signal, and every async child waiting on SIGCHLD, silently never arrived.
//
// Retrying without the flag, rather than clearing it up front, is deliberate.
// Clearing it for every send on NT also worked for the socketpair, but it
// wedged large transfers: 3 runs in 20 of the reqwest example froze mid-body
// with both ends parked, against 0 in 20 once the flag was left alone. cosmo
// evidently does read the bit somewhere in its NT send path, so the only
// sends that get their flags touched here are the ones that already failed.
static int nosignal_retry(int host_flags, long r) {
    return r < 0 && IsWindows() && (host_flags & MSG_NOSIGNAL) && errno == EINVAL;
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
    if (__ape_shim_nt_wants_eagain(fd)) return send_result(fd, -1, n);
    int host = msg_to_host(flags);
    unsigned long len = __ape_shim_nt_clamp(fd, n);
    long r = send(fd, buf, len, host);
    if (nosignal_retry(host, r)) r = send(fd, buf, len, host & ~MSG_NOSIGNAL);
    return send_result(fd, r, n);
}

long __ape_shim_sendto(int fd, const void *buf, unsigned long n, int flags,
                       const void *addr, unsigned alen) {
    struct fam_copy tmp;
    if (__ape_shim_nt_wants_eagain(fd)) return send_result(fd, -1, n);
    int host = msg_to_host(flags);
    unsigned long len = __ape_shim_nt_clamp(fd, n);
    const void *to = addr_to_host(addr, &alen, &tmp);
    long r = sendto(fd, buf, len, host, to, alen);
    if (nosignal_retry(host, r)) r = sendto(fd, buf, len, host & ~MSG_NOSIGNAL, to, alen);
    return send_result(fd, r, n);
}

long __ape_shim_recv(int fd, void *buf, unsigned long n, int flags) {
    return recv_result(fd, recv(fd, buf, n, msg_to_host(flags)));
}

long __ape_shim_recvfrom(int fd, void *buf, unsigned long n, int flags,
                         void *addr, unsigned *alen) {
    long r = recvfrom(fd, buf, n, msg_to_host(flags), addr, alen);
    if (r >= 0) addr_to_linux(addr, alen);
    return recv_result(fd, r);
}

// ---------------------------------------------------------------------------
// sendmsg/recvmsg. struct msghdr is byte-compatible with musl's on LE
// (cosmo declares it "Linux+NT ABI"; musl's int+pad pairs line up with
// cosmo's u64/u32+pad), so the values inside it are what get rewritten: the
// MSG_* flags argument, the msg_name family, recvmsg's msg_flags output,
// and the control payload's cmsg_level (SOL_SOCKET is a runtime constant,
// same trap as SO_ERROR's payload). cmsg_type: SCM_RIGHTS is 1 on both
// sides; IPPROTO_IP/IPV6 types go through the sockopt tables; anything else
// passes raw. The cmsghdr LAYOUT, however, is only linux-shaped on hosts
// whose kernel takes it (Linux, cosmo's NT layer): cosmo forwards
// msg_control untouched, so on the BSDs — whose cmsghdr is 12 bytes with
// its own alignment — the shim additionally converts the layout both ways.

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
// u32 len, pad, i32 level, i32 type) rewriting level/type in place. Only
// correct for hosts whose kernel takes the linux layout (Linux, and cosmo's
// NT emulation); the BSDs get the layout-converting pair below instead.
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

// The BSDs don't share the linux cmsghdr layout — theirs is {u32 cmsg_len;
// i32 level; i32 type} (12-byte header; XNU aligns entries to 4, the others
// to 8, vs linux's 16-byte header and 8) — and cosmo's sendmsg/recvmsg pass
// msg_control through UNtranslated on every host. So on the BSDs the shim
// converts the layout itself, translating level/type on the way through.

static unsigned long cmsg_align_host(unsigned long n) {
    unsigned long a = IsXnu() ? 4 : 8;
    return (n + a - 1) & ~(a - 1);
}

// Linux control buffer -> BSD layout. Returns the BSD controllen, or -1 if
// the converted stream wouldn't fit in dstcap.
static long cmsgs_to_bsd(const unsigned char *src, unsigned long srclen,
                         unsigned char *dst, unsigned long dstcap) {
    unsigned long si = 0, di = 0;
    while (si + 16 <= srclen) {
        unsigned long len = *(const unsigned long *)(src + si); // incl. 16B hdr
        if (len < 16 || len > srclen - si) break;
        unsigned long payload = len - 16;
        unsigned long blen = 12 + payload;
        if (cmsg_align_host(blen) > dstcap - di) return -1;
        int lvl = *(const int *)(src + si + 8);
        int typ = *(const int *)(src + si + 12);
        if (lvl == SHIM_LIN_SOL_SOCKET) lvl = SOL_SOCKET;
        else if (lvl == SHIM_LIN_IPPROTO_IP) typ = LOOKUP(kIp, typ);
        else if (lvl == SHIM_LIN_IPPROTO_IPV6) typ = LOOKUP(kIpv6, typ);
        *(unsigned *)(dst + di) = blen;
        *(int *)(dst + di + 4) = lvl;
        *(int *)(dst + di + 8) = typ;
        memcpy(dst + di + 12, src + si + 16, payload);
        di += cmsg_align_host(blen);
        si += (len + 7) & ~7ul;
    }
    return (long)di;
}

// BSD control buffer -> linux layout in the caller's buffer. The linux form
// is larger per entry, so an overfull result is truncated at an entry
// boundary, which is the same shape MSG_CTRUNC leaves behind.
static unsigned long cmsgs_to_linux_layout(const unsigned char *src, unsigned long srclen,
                                           unsigned char *dst, unsigned long dstcap) {
    unsigned long si = 0, di = 0;
    while (si + 12 <= srclen) {
        unsigned blen = *(const unsigned *)(src + si);
        if (blen < 12 || blen > srclen - si) break;
        unsigned long payload = blen - 12;
        unsigned long llen = 16 + payload;
        if (((llen + 7) & ~7ul) > dstcap - di) break;
        int lvl = *(const int *)(src + si + 4);
        int typ = *(const int *)(src + si + 8);
        if (lvl == SOL_SOCKET) lvl = SHIM_LIN_SOL_SOCKET;
        else if (lvl == SHIM_LIN_IPPROTO_IP) typ = LOOKUP_REV(kIp, typ);
        else if (lvl == SHIM_LIN_IPPROTO_IPV6) typ = LOOKUP_REV(kIpv6, typ);
        *(unsigned long *)(dst + di) = llen;
        *(int *)(dst + di + 8) = lvl;
        *(int *)(dst + di + 12) = typ;
        memmove(dst + di + 16, src + si + 12, payload);
        di += (llen + 7) & ~7ul;
        si += cmsg_align_host(blen);
    }
    return di;
}

static unsigned long msg_total(const struct msghdr *msg) {
    unsigned long total = 0;
    if (msg && msg->msg_iov)
        for (size_t i = 0; i < (size_t)msg->msg_iovlen; i++)
            total += msg->msg_iov[i].iov_len;
    return total;
}

long __ape_shim_sendmsg(int fd, const struct msghdr *msg, int flags) {
    unsigned long want = msg_total(msg);
    if (__ape_shim_nt_wants_eagain(fd)) return send_result(fd, -1, want);
    if (!msg) {
        int host = msg_to_host(flags);
        long r = sendmsg(fd, msg, host);
        if (nosignal_retry(host, r)) r = sendmsg(fd, msg, host & ~MSG_NOSIGNAL);
        return send_result(fd, r, want);
    }
    struct msghdr m = *msg;
    struct fam_copy name_tmp;
    if (m.msg_name)
        m.msg_name = (void *)addr_to_host(m.msg_name, &m.msg_namelen, &name_tmp);
    // control payload: rewrite on a copy — the caller's buffer is logically
    // const. Oversized control data passes through untranslated rather than
    // failing (SOL_SOCKET is 1 == Linux on every cosmo host except the BSDs'
    // 0xffff, and cmsgs beyond this size are exotic).
    unsigned char ctl_tmp[1024];
    if (m.msg_control && m.msg_controllen <= sizeof(ctl_tmp)) {
        if (IsBsd()) {
            long n = cmsgs_to_bsd(m.msg_control, m.msg_controllen,
                                  ctl_tmp, sizeof(ctl_tmp));
            if (n >= 0) {
                m.msg_control = n ? ctl_tmp : NULL;
                m.msg_controllen = n;
            }
        } else {
            memcpy(ctl_tmp, m.msg_control, m.msg_controllen);
            cmsgs_rewrite(ctl_tmp, m.msg_controllen, 1);
            m.msg_control = ctl_tmp;
        }
    }
    int host = msg_to_host(flags);
    long r = sendmsg(fd, &m, host);
    if (nosignal_retry(host, r)) r = sendmsg(fd, &m, host & ~MSG_NOSIGNAL);
    return send_result(fd, r, want);
}

long __ape_shim_recvmsg(int fd, struct msghdr *msg, int flags) {
    // On the BSDs cosmo's recvmsg runs the caller's msg_name OUTPUT buffer
    // through sockaddr2bsd as if it were input, and a family it doesn't
    // recognize (a zeroed buffer, or musl's AF_INET6 left by a previous
    // call) is answered EPFNOSUPPORT before any syscall (recvfrom converts
    // through a local copy instead and doesn't have this). Linux semantics
    // never read the buffer, so seed it with the socket's own host-coded
    // family, which is what any received address will be anyway.
    if (IsBsd() && msg && msg->msg_name && msg->msg_namelen >= 2) {
        struct sockaddr_storage ss;
        unsigned sl = sizeof(ss);
        if (getsockname(fd, (struct sockaddr *)&ss, &sl) == 0 && sl >= 2)
            *(unsigned short *)msg->msg_name = ss.ss_family;
    }
    // msg_controllen is capacity going in, actual length coming out; the
    // BSD->linux expansion below writes back into the caller's buffer and
    // needs the original capacity.
    unsigned long ctl_cap = msg ? msg->msg_controllen : 0;
    long r = recv_result(fd, recvmsg(fd, msg, msg_to_host(flags)));
    if (r >= 0 && msg) {
        if (msg->msg_name && msg->msg_namelen >= 2) {
            unsigned short *fam = msg->msg_name;
            *fam = (unsigned short)af_to_linux(*fam);
        }
        if (msg->msg_control && msg->msg_controllen) {
            if (IsBsd()) {
                unsigned char tmp[1024];
                unsigned long n = msg->msg_controllen;
                if (n <= sizeof(tmp)) {
                    memcpy(tmp, msg->msg_control, n);
                    msg->msg_controllen =
                        cmsgs_to_linux_layout(tmp, n, msg->msg_control, ctl_cap);
                }
            } else {
                cmsgs_rewrite(msg->msg_control, msg->msg_controllen, 0);
            }
        }
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
    return getnameinfo(addr_to_host(addr, &alen, &tmp), alen, host_out, hostlen,
                       serv, servlen, flags);
}
