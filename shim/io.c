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

#include <stddef.h>
#include <sys/uio.h>
#include <unistd.h>

int __ape_shim_nt_gated(int fd);                            // socket.c
int __ape_shim_nt_wants_eagain(int fd);                     // socket.c
unsigned long __ape_shim_nt_clamp(int fd, unsigned long n); // socket.c

long __ape_shim_write(int fd, const void *buf, unsigned long n) {
    if (__ape_shim_nt_wants_eagain(fd)) return -1;
    return write(fd, buf, __ape_shim_nt_clamp(fd, n));
}

long __ape_shim_writev(int fd, const struct iovec *iov, int iovcnt) {
    if (__ape_shim_nt_wants_eagain(fd)) return -1;
    if (iovcnt > 0 && __ape_shim_nt_gated(fd)) {
        // in gated mode, degrade to a short write of the first non-empty
        // slice so the total handed to the blocking NT path stays clamped.
        for (int i = 0; i < iovcnt; i++) {
            if (iov[i].iov_len)
                return write(fd, iov[i].iov_base, __ape_shim_nt_clamp(fd, iov[i].iov_len));
        }
    }
    return writev(fd, iov, iovcnt);
}
