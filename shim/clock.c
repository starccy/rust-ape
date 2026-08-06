// Clock ids for the Linux-personality shim.
//
// clockid_t is another set of values baked in at compile time. musl
// numbers the clocks 0..8, while cosmo resolves them per host (mach
// clocks on mac, emulated on NT). CLOCK_REALTIME is 0 everywhere and
// TIMER_ABSTIME is 1 on both sides, so they pass through; the rest
// translate.
//
// pthread_condattr_setclock is also handled here, since it is the one
// pthread entry point that takes a clockid.
//
// Values come from tables.h (`cargo xtask gen-shim`).

#include <errno.h>
#include <pthread.h>
#include <stddef.h>
#include <time.h>
#include <libc/sysv/consts/clock.h>

#include "tables.h"

struct cmap {
    int lin;
    const int *host;
};

#define X(name, lin) { lin, &name },
static const struct cmap kClocks[] = { SHIM_CLOCK_TABLE(X) };
#undef X
#define NCLOCKS (sizeof(kClocks) / sizeof(kClocks[0]))

static int clock_to_host(int lin, int *out) {
    for (size_t i = 0; i < NCLOCKS; i++) {
        if (kClocks[i].lin == lin) {
            *out = *kClocks[i].host;
            return 0;
        }
    }
    return errno = EINVAL, -1;
}

int __ape_shim_clock_gettime(int lin, struct timespec *ts) {
    int host;
    if (clock_to_host(lin, &host) < 0) return -1;
    return clock_gettime(host, ts);
}

int __ape_shim_clock_getres(int lin, struct timespec *ts) {
    int host;
    if (clock_to_host(lin, &host) < 0) return -1;
    return clock_getres(host, ts);
}

int __ape_shim_clock_settime(int lin, const struct timespec *ts) {
    int host;
    if (clock_to_host(lin, &host) < 0) return -1;
    return clock_settime(host, ts);
}

int __ape_shim_clock_nanosleep(int lin, int flags, const struct timespec *req,
                               struct timespec *rem) {
    int host;
    if (clock_to_host(lin, &host) < 0) return EINVAL; // @returnserrno family
    return clock_nanosleep(host, flags, req, rem);    // TIMER_ABSTIME: 1 everywhere
}

int __ape_shim_pthread_condattr_setclock(pthread_condattr_t *attr, int lin) {
    int host;
    if (clock_to_host(lin, &host) < 0) return EINVAL; // @returnserrno family
    return pthread_condattr_setclock(attr, host);
}
