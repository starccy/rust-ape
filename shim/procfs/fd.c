// /proc/self/fd, read out of cosmo's own descriptor table. For any other
// process only the socket tables say what it holds, but for ourselves g_fds
// knows every descriptor, its kind, and its NT handle -- so the entries here
// carry the real fd numbers and cover files, pipes and devices, not just
// sockets. A socket's inode is computed from the same identity the table
// rows hash, so following a socket:[N] link from here lands on the right
// row of /proc/net/tcp.

#define _COSMO_SOURCE // for g_fds

#include <netinet/in.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
#include <libc/calls/internal.h>
#include <libc/dce.h>
#include <libc/intrin/fds.h>
#include <libc/nt/files.h>

#include "procfs.h"

// The identity the socket tables hash, taken from the descriptor itself.
// An unconnected or unbound end reads as zeros, which is also how the
// tables report it.
static void socket_text(int fd, const struct Fd *f, char *out, size_t n) {
    uint8_t laddr[16] = {0}, raddr[16] = {0};
    uint16_t lport = 0, rport = 0;
    uint8_t family = f->family == AF_INET6 ? 6 : 4;
    uint8_t proto = f->type == SOCK_DGRAM ? 1 : 0;

    struct sockaddr_storage ss;
    socklen_t sl = sizeof ss;
    if (!getsockname(fd, (struct sockaddr *)&ss, &sl)) {
        if (ss.ss_family == AF_INET) {
            struct sockaddr_in *a = (struct sockaddr_in *)&ss;
            memcpy(laddr, &a->sin_addr, 4);
            lport = ntohs(a->sin_port);
        } else if (ss.ss_family == AF_INET6) {
            struct sockaddr_in6 *a = (struct sockaddr_in6 *)&ss;
            memcpy(laddr, &a->sin6_addr, 16);
            lport = ntohs(a->sin6_port);
        }
    }
    sl = sizeof ss;
    if (!getpeername(fd, (struct sockaddr *)&ss, &sl)) {
        if (ss.ss_family == AF_INET) {
            struct sockaddr_in *a = (struct sockaddr_in *)&ss;
            memcpy(raddr, &a->sin_addr, 4);
            rport = ntohs(a->sin_port);
        } else if (ss.ss_family == AF_INET6) {
            struct sockaddr_in6 *a = (struct sockaddr_in6 *)&ss;
            memcpy(raddr, &a->sin6_addr, 16);
            rport = ntohs(a->sin6_port);
        }
    }
    uint64_t inode = pfs_net_inode(proto, family, pfs_self_pid(), lport,
                                   rport, laddr, raddr);
    snprintf(out, n, "socket:[%llu]", (unsigned long long)inode);
}

// A file descriptor's path, when NT will name it. Anonymous pipes and other
// unnameable handles read as pipe:[hash] -- the number only needs to be
// stable and distinct, the way Linux's anonymous inode numbers are.
static void file_text(const struct Fd *f, char *out, size_t n) {
    char16_t w[512];
    uint32_t len = GetFinalPathNameByHandle(f->handle, w, 512, 0);
    if (len && len < 512) {
        // \\?\C:\x -> C:/x, \\?\UNC\srv\share -> //srv/share
        uint32_t i = 0;
        if (len > 4 && w[0] == '\\' && w[1] == '\\' && w[2] == '?') {
            i = 4;
            if (len > 8 && w[4] == 'U' && w[5] == 'N' && w[6] == 'C') i = 7;
        }
        size_t k = 0;
        for (; i < len && k < n - 1; i++)
            out[k++] = w[i] == '\\' ? '/' : (w[i] < 128 ? (char)w[i] : '_');
        out[k] = 0;
        // "C:/x" -> "/C/x", cosmo's spelling of an absolute path
        if (k >= 2 && out[1] == ':') {
            out[1] = out[0];
            out[0] = '/';
        }
        return;
    }
    uint64_t h = 0xcbf29ce484222325ull;
    for (int i = 0; i < 8; i++) {
        h ^= (uint8_t)(f->handle >> (i * 8));
        h *= 0x100000001b3ull;
    }
    snprintf(out, n, "pipe:[%llu]", (unsigned long long)(h & 0xffffffffffull));
}

int pfs_self_fds(struct pfs_fdent *out, int cap) {
    if (!IsWindows()) return 0;
    int n = 0;
    for (int fd = 0; fd < (int)g_fds.n && n < cap; fd++) {
        const struct Fd *f = &g_fds.p[fd];
        struct pfs_fdent *e = &out[n];
        switch (f->kind) {
            case kFdFile: file_text(f, e->text, sizeof e->text); break;
            case kFdSocket: socket_text(fd, f, e->text, sizeof e->text); break;
            case kFdConsole:
            case kFdSerial: snprintf(e->text, sizeof e->text, "/dev/tty"); break;
            case kFdDevNull: snprintf(e->text, sizeof e->text, "/dev/null"); break;
            case kFdDevRandom:
                snprintf(e->text, sizeof e->text, "/dev/urandom");
                break;
            case kFdZip: snprintf(e->text, sizeof e->text, "/zip"); break;
            case kFdEmpty: continue;
            default:
                snprintf(e->text, sizeof e->text, "anon_inode:[kind%d]",
                         f->kind);
                break;
        }
        e->fd = fd;
        n++;
    }
    return n;
}
