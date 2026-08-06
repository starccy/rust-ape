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
