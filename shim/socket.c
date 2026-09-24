// The socket half of the Linux-personality shim.
//
// Socket types, address families, sockopt names, MSG_* flags and POLL* bits
// are Linux's on both sides now, and cosmo converts sockaddr, msghdr and
// cmsghdr for the BSDs itself. What is left here is emulation and policy:
// the AF_PACKET socket shim/packet.c builds on NT, epoll's edge-triggered
// bookkeeping, a socketpair that NT's poll reports writable, NT's
// nonblocking send, and a parking lot for sockopts WinSock refuses while a
// connect is in flight.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <libc/dce.h>
#include <pthread.h>
#include <stdbool.h> // cosmo's own build has C23 bool
#include <libc/calls/internal.h>
#include <libc/intrin/fds.h>
#include <libc/sysv/pib.h>

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

// shim/packet.c emulates AF_PACKET on NT with an SIO_RCVALL raw socket, and
// these are the calls whose meaning differs enough that it has to answer
// them itself. Every check hides behind a cheap load asking whether any
// packet socket exists, so a program that never sniffs pays nothing.
int __ape_shim_packet_socket(int lin_type, int protocol);
_Bool __ape_shim_packet_is(int fd);
_Bool __ape_shim_packet_sockopt(int fd, int level);
int __ape_shim_packet_bind(int fd, const void *addr, unsigned len);
long __ape_shim_packet_recv(int fd, void *buf, unsigned long n, void *addr,
                            unsigned *alen);

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

int __ape_shim_socket(int domain, int type, int protocol) {
    __ape_shim_pin_winsock();
    // NT has no packet socket, so shim/packet.c builds one out of a raw
    // socket. Only there: on every other host AF_PACKET is the host's own.
    if (IsWindows() && domain == AF_PACKET) {
        int fd = __ape_shim_packet_socket(type, protocol);
        if (fd >= 0) __ape_shim_epoll_note_new_fd(fd);
        return fd;
    }
    int fd = socket(domain, type, protocol);
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

    // /tmp is what cosmo maps to the host's temp directory; bind() turns it
    // into the spelling Winsock wants.
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof sa);
    sa.sun_family = AF_UNIX;
    int plen = snprintf(sa.sun_path, sizeof sa.sun_path, "/tmp/ape-sp-%d-%02x%02x%02x%02x.sock",
                        (int)getpid(), nonce[0], nonce[1], nonce[2], nonce[3]);
    if (plen < 0 || plen >= (int)sizeof sa.sun_path) return -1;
    unsigned salen = offsetof(struct sockaddr_un, sun_path) + (unsigned)plen + 1;

    int srv = -1, cli = -1, acc = -1, err;
    if ((srv = socket(AF_UNIX, type, 0)) < 0) goto fail;
    if (bind(srv, (const struct sockaddr *)&sa, salen) < 0) goto fail;
    if (listen(srv, 4) < 0) goto fail;
    if ((cli = socket(AF_UNIX, type, 0)) < 0) goto fail;
    if (connect(cli, (const struct sockaddr *)&sa, salen) < 0) goto fail;
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
    unlink(sa.sun_path);
    sv[0] = cli;
    sv[1] = acc;
    return 0;

fail:
    err = errno;
    if (srv >= 0) close(srv);
    if (cli >= 0) close(cli);
    if (acc >= 0) close(acc);
    unlink(sa.sun_path);
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
int __ape_shim_socketpair(int domain, int type, int protocol, int sv[2]) {
    int nonblock = type & SOCK_NONBLOCK;
    type &= ~SOCK_NONBLOCK;

    __ape_shim_pin_winsock();
    int made = -1;
    if (IsWindows() && domain == AF_UNIX && (type & ~SOCK_CLOEXEC) == SOCK_STREAM)
        made = nt_unix_pair(type, sv);
    // Falling back rather than failing: cosmo's own pair is what shipped
    // before, and it is still better than no pair at all.
    if (made < 0 && socketpair(domain, type, protocol, sv) < 0) {
        // XNU's AF_UNIX has no SOCK_SEQPACKET and says so with
        // EPROTONOSUPPORT. std asks for one on every linux target, as the
        // channel a failed exec reports its errno back on, so without this
        // every Command::spawn that skips posix_spawn (a pre_exec closure, a
        // uid, a chroot) dies on macOS before it even forks. A stream pair
        // carries that exchange whole, one write and then EOF from the
        // CLOEXEC close. What it gives up is message boundaries, so a caller
        // that sends two records back to back may read them as one.
        if (!(domain == AF_UNIX && (type & ~SOCK_CLOEXEC) == SOCK_SEQPACKET &&
              (errno == EPROTONOSUPPORT || errno == ESOCKTNOSUPPORT ||
               errno == EPROTOTYPE)))
            return -1;
        int stream = (type & SOCK_CLOEXEC) | SOCK_STREAM;
        if (socketpair(domain, stream, protocol, sv) < 0) return -1;
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

int __ape_shim_accept4(int fd, void *addr, unsigned *len, int flags) {
    int r = accept4(fd, addr, len, flags);
    if (r >= 0) {
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
        __ape_shim_epoll_note_new_fd(r);
        __ape_shim_epoll_rearm_in(fd); // more connections may be pending
    } else if (errno == EAGAIN) {
        __ape_shim_epoll_rearm_in(fd);
    }
    return r;
}

int __ape_shim_bind(int fd, const void *addr, unsigned len) {
    // A packet socket binds to an interface index, not to an address, so
    // shim/packet.c resolves it and turns the capture on.
    if (__ape_shim_packet_is(fd)) return __ape_shim_packet_bind(fd, addr, len);
    return bind(fd, addr, len);
}

// ---------------------------------------------------------------------------
// Socket options set while a nonblocking connect is still in flight.
//
// Linux accepts them and they take effect once the connection is up.
// WinSock refuses several (TCP_NODELAY, SO_KEEPALIVE and SO_REUSEADDR among
// them) with EINVAL until the handshake completes, and cosmo only learns the
// handshake completed when connect() is called again, so nothing in the
// runtime can retry on the program's behalf. Such requests are parked here
// and replayed before the socket's first send, the earliest point at which
// they could matter. Only int-sized values are parked, which covers every
// option WinSock refuses in this state. Entries die with the fd.

struct deferred_opt { int fd, level, name, val; };
#define DEFER_MAX 32
static struct deferred_opt g_defer[DEFER_MAX] = { [0 ... DEFER_MAX - 1] = { .fd = -1 } };
static int g_defer_n; // fast path: sends look no further while this is zero
static pthread_mutex_t g_defer_lock = PTHREAD_MUTEX_INITIALIZER;

static int sock_is_connecting(int fd) {
    if ((unsigned)fd >= __get_pib()->fds.n) return 0;
    struct Fd *f = __get_pib()->fds.p + fd;
    return f->kind == kFdSocket && f->connecting == 1;
}

static int defer_sockopt(int fd, int level, int name, int val) {
    int ok = 0;
    pthread_mutex_lock(&g_defer_lock);
    int slot = -1;
    for (int i = 0; i < DEFER_MAX; i++) {
        struct deferred_opt *d = &g_defer[i];
        if (d->fd == fd && d->level == level && d->name == name) { slot = i; break; }
        if (slot < 0 && d->fd == -1) slot = i;
    }
    if (slot >= 0) {
        if (g_defer[slot].fd == -1) g_defer_n++;
        g_defer[slot] = (struct deferred_opt){ fd, level, name, val };
        ok = 1;
    }
    pthread_mutex_unlock(&g_defer_lock);
    return ok;
}

// Retries every parked option on fd. One that still gets EINVAL stays
// parked, since the handshake is then still in flight and the send that
// follows will not go through either.
void __ape_shim_nt_sockopt_replay(int fd) {
    if (!g_defer_n) return;
    pthread_mutex_lock(&g_defer_lock);
    for (int i = 0; i < DEFER_MAX; i++) {
        struct deferred_opt *d = &g_defer[i];
        if (d->fd != fd) continue;
        int e = errno;
        if (setsockopt(fd, d->level, d->name, &d->val, sizeof(d->val)) == 0 ||
            errno != EINVAL) {
            d->fd = -1;
            g_defer_n--;
        }
        errno = e;
    }
    pthread_mutex_unlock(&g_defer_lock);
}

void __ape_shim_nt_sockopt_forget(int fd) {
    if (!g_defer_n) return;
    pthread_mutex_lock(&g_defer_lock);
    for (int i = 0; i < DEFER_MAX; i++)
        if (g_defer[i].fd == fd) { g_defer[i].fd = -1; g_defer_n--; }
    pthread_mutex_unlock(&g_defer_lock);
}

static int deferred_lookup(int fd, int level, int name, int *val) {
    int found = 0;
    if (!g_defer_n) return 0;
    pthread_mutex_lock(&g_defer_lock);
    for (int i = 0; i < DEFER_MAX; i++) {
        struct deferred_opt *d = &g_defer[i];
        if (d->fd == fd && d->level == level && d->name == name) {
            *val = d->val;
            found = 1;
            break;
        }
    }
    pthread_mutex_unlock(&g_defer_lock);
    return found;
}

int __ape_shim_setsockopt(int fd, int level, int name, const void *val, unsigned len) {
    // SO_REUSEPORT has no Windows equivalent (cosmo answers ENOPROTOOPT
    // there), and WinSock's SO_REUSEADDR already allows rebinding a port.
    // On Linux the option always succeeds, so accept it as a no-op instead
    // of failing callers that treat the error as fatal.
    if (IsWindows() && level == SOL_SOCKET && name == SO_REUSEPORT) return 0;
    // SOL_PACKET options succeed as no-ops, since promiscuity is already on
    // and refusing would abort a capture that does work.
    if (__ape_shim_packet_sockopt(fd, level)) return 0;
    int r = setsockopt(fd, level, name, val, len);
    if (r == -1 && errno == EINVAL && IsWindows() && val && len == sizeof(int) &&
        sock_is_connecting(fd)) {
        if (defer_sockopt(fd, level, name, *(const int *)val)) return 0;
        errno = EINVAL;
    }
    return r;
}

// BSD-derived kernels answer a boolean option with their own flag bit
// (on XNU TCP_NODELAY reads back as 4 and SO_KEEPALIVE as 8); Linux
// answers 0 or 1, and callers compare against 1.
static int sockopt_is_boolean(int level, int name) {
    if (level == SOL_SOCKET)
        switch (name) {
            case SO_REUSEADDR:
            case SO_DONTROUTE:
            case SO_BROADCAST:
            case SO_KEEPALIVE:
            case SO_OOBINLINE:
            case SO_REUSEPORT:
            case SO_ACCEPTCONN:
                return 1;
        }
    if (level == IPPROTO_TCP)
        switch (name) {
            case TCP_NODELAY:
            case TCP_CORK:
                return 1;
        }
    return 0;
}

int __ape_shim_getsockopt(int fd, int level, int name, void *val, unsigned *len) {
    int parked;
    if (val && len && *len >= sizeof(int) && deferred_lookup(fd, level, name, &parked)) {
        *(int *)val = parked;
        *len = sizeof(int);
        return 0;
    }
    int r = getsockopt(fd, level, name, val, len);
    if (r == 0 && IsBsd() && val && len && *len == sizeof(int) &&
        sockopt_is_boolean(level, name)) {
        *(int *)val = !!*(int *)val;
    }
    return r;
}

// MSG_* bits cosmo has no name for are dropped rather than refused: for
// send flags they are hints, and failing hard would break retry loops.
#define MSG_KNOWN (MSG_OOB | MSG_PEEK | MSG_DONTROUTE | MSG_DONTWAIT | MSG_WAITALL | \
                   MSG_NOSIGNAL | MSG_TRUNC | MSG_CTRUNC)

static int msg_flags(int lin) {
    return lin & MSG_KNOWN;
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
static int nosignal_retry(int flags, long r) {
    return r < 0 && IsWindows() && (flags & MSG_NOSIGNAL) && errno == EINVAL;
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
    __ape_shim_nt_sockopt_replay(fd);
    if (__ape_shim_nt_wants_eagain(fd)) return send_result(fd, -1, n);
    int host = msg_flags(flags);
    unsigned long len = __ape_shim_nt_clamp(fd, n);
    long r = send(fd, buf, len, host);
    if (nosignal_retry(host, r)) r = send(fd, buf, len, host & ~MSG_NOSIGNAL);
    return send_result(fd, r, n);
}

long __ape_shim_sendto(int fd, const void *buf, unsigned long n, int flags,
                       const void *addr, unsigned alen) {
    __ape_shim_nt_sockopt_replay(fd);
    if (__ape_shim_nt_wants_eagain(fd)) return send_result(fd, -1, n);
    int host = msg_flags(flags);
    unsigned long len = __ape_shim_nt_clamp(fd, n);
    long r = sendto(fd, buf, len, host, addr, alen);
    if (nosignal_retry(host, r)) r = sendto(fd, buf, len, host & ~MSG_NOSIGNAL, addr, alen);
    return send_result(fd, r, n);
}

long __ape_shim_recv(int fd, void *buf, unsigned long n, int flags) {
    // A packet socket's datagrams need the link-layer header put back in
    // front of them, and may come from either of the two sockets behind it.
    if (__ape_shim_packet_is(fd))
        return recv_result(fd, __ape_shim_packet_recv(fd, buf, n, NULL, NULL));
    return recv_result(fd, recv(fd, buf, n, msg_flags(flags)));
}

long __ape_shim_recvfrom(int fd, void *buf, unsigned long n, int flags,
                         void *addr, unsigned *alen) {
    if (__ape_shim_packet_is(fd))
        return recv_result(fd, __ape_shim_packet_recv(fd, buf, n, addr, alen));
    return recv_result(fd, recvfrom(fd, buf, n, msg_flags(flags), addr, alen));
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
    __ape_shim_nt_sockopt_replay(fd);
    if (__ape_shim_nt_wants_eagain(fd)) return send_result(fd, -1, want);
    int host = msg_flags(flags);
    long r = sendmsg(fd, msg, host);
    if (nosignal_retry(host, r)) r = sendmsg(fd, msg, host & ~MSG_NOSIGNAL);
    return send_result(fd, r, want);
}

long __ape_shim_recvmsg(int fd, struct msghdr *msg, int flags) {
    return recv_result(fd, recvmsg(fd, msg, msg_flags(flags)));
}
