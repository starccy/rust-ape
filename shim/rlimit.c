// Resource limits for the Linux-personality shim.
//
// The RLIMIT_* resource numbers differ per host (RLIMIT_NOFILE is 7 on
// Linux, 8 on the BSDs) and are runtime constants under cosmo. struct
// rlimit is two uint64s everywhere; RLIM_INFINITY is all-ones on Linux and
// under cosmo alike, so the values pass through.
//
// Values come from tables.h (`cargo xtask gen-shim`).

#include <errno.h>
#include <stddef.h>
#include <sys/resource.h>
#include <sys/types.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"
// newer cosmocc moved the RLIMIT_* constants next to the struct
#if __has_include(<libc/sysv/consts/rlimit.h>)
#include <libc/sysv/consts/rlimit.h>
#else
#include <libc/calls/struct/rlimit.h>
#endif

#include "tables.h"

struct rmap {
    int lin;
    const unsigned *host;
};

#define X(name, lin) { lin, &name },
static const struct rmap kRlimits[] = { SHIM_RLIMIT_TABLE(X) };
#undef X
#define NRLIMITS (sizeof(kRlimits) / sizeof(kRlimits[0]))

static int res_to_host(int lin, int *out) {
    for (size_t i = 0; i < NRLIMITS; i++) {
        if (kRlimits[i].lin == lin) {
            *out = (int)*kRlimits[i].host;
            return 0;
        }
    }
    return errno = EINVAL, -1;
}

int __ape_shim_getrlimit(int lin, struct rlimit *rl) {
    int host;
    if (res_to_host(lin, &host) < 0) return -1;
    return getrlimit(host, rl);
}

int __ape_shim_setrlimit(int lin, const struct rlimit *rl) {
    int host;
    if (res_to_host(lin, &host) < 0) return -1;
    return setrlimit(host, rl);
}

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
// the new one is installed, and a rejected install still fails the call. The
// resource number goes to the kernel Linux-coded (not translated through
// res_to_host) because that is who reads it -- and under IsLinux() the two
// codings are the same value anyway. struct rlimit is two uint64s on both
// sides, so it passes through unrepacked, exactly as get/setrlimit do above.
int __ape_shim_prlimit(pid_t pid, int lin, const struct rlimit *neu,
                       struct rlimit *old) {
    int host;
    if (res_to_host(lin, &host) < 0) return -1;
    if (pid == 0 || pid == getpid()) {
        if (old && getrlimit(host, old) < 0) return -1;
        if (neu && setrlimit(host, neu) < 0) return -1;
        return 0;
    }
    if (!IsLinux()) return errno = ENOSYS, -1;
    return (int)__ape_syscall_ret(
        __ape_raw_syscall(__NR_prlimit, pid, lin, (long)neu, (long)old, 0));
}
