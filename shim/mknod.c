// cosmo has no mknodat. implement the raw syscall
// on Linux and ENOSYS elsewhere

#include <errno.h>
#include <stddef.h>
#include <sys/types.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

int mknodat(int dirfd, const char *path, mode_t mode, dev_t dev) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return (int)__ape_syscall_ret(__ape_raw_syscall(__NR_mknodat, dirfd,
                                                    (long)path, mode, (long)dev, 0));
}
