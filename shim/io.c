// write/writev go through the shim for one reason. cosmo's NT send path
// ignores O_NONBLOCK (see the emulation note in socket.c), and std's
// TcpStream (and everything layered on it, smol included) writes to sockets
// through plain write()/writev(), not send(). So the same poll gate is
// applied here. On a non-Windows host, or an fd without O_NONBLOCK, this is
// a single predictable branch on the way to the real call.
//
// The gate runs for every O_NONBLOCK fd, socket or not. poll(POLLOUT, 0)
// reports regular files and ready pipes as writable, so non-sockets pass
// straight through, which is cheaper and simpler than asking each fd
// whether it's a socket first.
//
// writev in gated mode degrades to writing the first non-empty iov slice
// (clamped). That is a short write, which vectored-write callers must
// already handle, and it only happens on NT for nonblocking fds.

#include <errno.h>
#include <stddef.h>
#include <sys/uio.h>
#include <unistd.h>

int __ape_shim_nt_gated(int fd);                            // socket.c
int __ape_shim_nt_wants_eagain(int fd);                     // socket.c
unsigned long __ape_shim_nt_clamp(int fd, unsigned long n); // socket.c
void __ape_shim_epoll_rearm_out(int fd);                    // epoll.c
void __ape_shim_epoll_hint_pipe_write(int fd);              // epoll.c
void __ape_shim_console_wrote(int fd, const void *, unsigned long);      // console.c
void __ape_shim_console_wrotev(int fd, const struct iovec *, int);      // console.c

// Feeds shim/epoll.c's edge-triggered arming. EAGAIN (real or synthesized
// by the poll gate) re-arms the write side. So does a short write: callers
// treat a partial write as a full buffer and wait for the next writability
// event instead of writing again, and the NT clamp produces partial writes
// on sockets whose buffers still have room, so no EAGAIN follows. A
// successful write to a recorded pipe write end re-arms the paired read
// end, which may be registered for reading without ever being read. `want`
// is the caller's original request length, before clamping.
static long write_result(int fd, long r, unsigned long want) {
    if (r > 0) {
        __ape_shim_epoll_hint_pipe_write(fd);
        if ((unsigned long)r < want) __ape_shim_epoll_rearm_out(fd);
    } else if (r == -1 && errno == EAGAIN) {
        __ape_shim_epoll_rearm_out(fd);
    }
    return r;
}

long __ape_shim_write(int fd, const void *buf, unsigned long n) {
    if (__ape_shim_nt_wants_eagain(fd)) return write_result(fd, -1, n);
    long r = write(fd, buf, __ape_shim_nt_clamp(fd, n));
    if (r > 0) __ape_shim_console_wrote(fd, buf, r);
    return write_result(fd, r, n);
}

long __ape_shim_writev(int fd, const struct iovec *iov, int iovcnt) {
    if (__ape_shim_nt_wants_eagain(fd)) return write_result(fd, -1, 1);
    unsigned long total = 0;
    for (int i = 0; i < iovcnt; i++) total += iov[i].iov_len;
    if (iovcnt > 0 && __ape_shim_nt_gated(fd)) {
        // in gated mode, degrade to a short write of the first non-empty
        // slice so the total handed to the blocking NT path stays clamped.
        for (int i = 0; i < iovcnt; i++) {
            if (iov[i].iov_len) {
                long r = write(fd, iov[i].iov_base, __ape_shim_nt_clamp(fd, iov[i].iov_len));
                if (r > 0) __ape_shim_console_wrote(fd, iov[i].iov_base, r);
                return write_result(fd, r, total);
            }
        }
    }
    long r = writev(fd, iov, iovcnt);
    if (r > 0) __ape_shim_console_wrotev(fd, iov, iovcnt);
    return write_result(fd, r, total);
}
