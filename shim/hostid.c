// cosmo has neither gethostid nor sethostname. gethostid answers from
// /etc/hostid on any host, and 0 when it is absent; sethostname is a raw
// syscall on Linux and ENOSYS elsewhere, where renaming the machine is not
// what this call promises.

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

long gethostid(void) {
    uint32_t id = 0;
    int fd = open("/etc/hostid", O_RDONLY);
    if (fd != -1) {
        unsigned char buf[4];
        if (read(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf)) {
            // little-endian on disk
            id = (uint32_t)buf[0] | (uint32_t)buf[1] << 8 |
                 (uint32_t)buf[2] << 16 | (uint32_t)buf[3] << 24;
        }
        close(fd);
    }
    // glibc sign-extends the stored value through int32_t; same shape here.
    return (long)(int32_t)id;
}

int sethostname(const char *name, size_t len) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return (int)__ape_syscall_ret(
        __ape_raw_syscall(__NR_sethostname, (long)name, (long)len, 0, 0, 0));
}
