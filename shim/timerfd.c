// eventfd and the timerfd family

// Linux gets the real syscalls and every other host answers ENOSYS
// Plain names, no __ape_shim_ prefix

#include <errno.h>
#include <time.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

int eventfd(unsigned int initval, int flags) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return __ape_syscall_ret(__ape_raw_syscall(__NR_eventfd2, initval, flags, 0, 0, 0));
}

int timerfd_create(int clockid, int flags) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return __ape_syscall_ret(__ape_raw_syscall(__NR_timerfd_create, clockid, flags, 0, 0, 0));
}

int timerfd_settime(int fd, int flags, const struct itimerspec *new_value,
                    struct itimerspec *old_value) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return __ape_syscall_ret(__ape_raw_syscall(__NR_timerfd_settime, fd, flags,
                                               (long)new_value, (long)old_value, 0));
}

int timerfd_gettime(int fd, struct itimerspec *curr_value) {
    if (!IsLinux()) return errno = ENOSYS, -1;
    return __ape_syscall_ret(__ape_raw_syscall(__NR_timerfd_gettime, fd, (long)curr_value, 0, 0, 0));
}
