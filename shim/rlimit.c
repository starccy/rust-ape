// prlimit for the Linux-personality shim. RLIMIT_* and struct rlimit are
// the same on both sides, so getrlimit and setrlimit need no help.

#include <errno.h>
#include <stddef.h>
#include <sys/resource.h>
#include <sys/types.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"
#include "tables.h"

// prlimit() has no cosmo counterpart, and reaching into *another* process's
// limits is a Linux-only ability to begin with. Two paths therefore:
//
//   - pid means me: get/setrlimit say the same thing and work on every host,
//     so that case needs no syscall and stays portable.
//   - pid means someone else: only Linux can do it, via the raw syscall.
//     Elsewhere it is ENOSYS, which is what a kernel lacking the call says
//     and what callers carrying a fallback already handle.
//
// The self path follows the kernel's order: the old value is reported before
// the new one is installed, and a rejected install still fails the call.
int __ape_shim_prlimit(pid_t pid, int lin, const struct rlimit *neu,
                       struct rlimit *old) {
    if (pid == 0 || pid == getpid()) {
        if (old && getrlimit(lin, old) < 0) return -1;
        if (neu && setrlimit(lin, neu) < 0) return -1;
        return 0;
    }
    if (!IsLinux()) return errno = ENOSYS, -1;
    return (int)__ape_syscall_ret(
        __ape_raw_syscall(__NR_prlimit, pid, lin, (long)neu, (long)old, 0));
}

int __ape_shim_getrusage(int who, struct rusage *ru) {
    if (who == SHIM_LIN_RUSAGE_THREAD) who = RUSAGE_THREAD;
    return getrusage(who, ru);
}
