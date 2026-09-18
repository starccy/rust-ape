// Scheduling policies are runtime constants under cosmo (SCHED_FIFO is 1 on
// Linux, 4 on XNU), and 127 where the host has no such policy.

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <spawn.h>
#include <stddef.h>
#include <libc/sysv/consts/sched.h>

#include "tables.h"

#define NOPOLICY 127

static int policy_to_host(int lin, int *out) {
    int reset = lin & SHIM_LIN_SCHED_RESET_ON_FORK;
    lin &= ~SHIM_LIN_SCHED_RESET_ON_FORK;
#define X(name, linval)                                  \
    if (lin == (linval)) {                               \
        if ((name) == NOPOLICY) return EINVAL;           \
        *out = (name) | (reset ? SCHED_RESET_ON_FORK : 0); \
        return 0;                                        \
    }
    SHIM_SCHED_TABLE(X)
#undef X
    return EINVAL;
}

static int policy_to_linux(int host) {
    int reset = SCHED_RESET_ON_FORK && (host & SCHED_RESET_ON_FORK);
    if (reset) host &= ~SCHED_RESET_ON_FORK;
#define X(name, linval) \
    if ((name) != NOPOLICY && host == (name)) return (linval) | (reset ? SHIM_LIN_SCHED_RESET_ON_FORK : 0);
    SHIM_SCHED_TABLE(X)
#undef X
    return host;
}

int __ape_shim_sched_setscheduler(int pid, int lin, const struct sched_param *param) {
    int host, rc = policy_to_host(lin, &host);
    if (rc) return errno = rc, -1;
    return sched_setscheduler(pid, host, param);
}

int __ape_shim_sched_getscheduler(int pid) {
    int host = sched_getscheduler(pid);
    return host < 0 ? host : policy_to_linux(host);
}

int __ape_shim_sched_get_priority_max(int lin) {
    int host, rc = policy_to_host(lin, &host);
    if (rc) return errno = rc, -1;
    return sched_get_priority_max(host);
}

int __ape_shim_sched_get_priority_min(int lin) {
    int host, rc = policy_to_host(lin, &host);
    if (rc) return errno = rc, -1;
    return sched_get_priority_min(host);
}

int __ape_shim_posix_spawnattr_setschedpolicy(posix_spawnattr_t *attr, int lin) {
    int host, rc = policy_to_host(lin, &host);
    return rc ? rc : posix_spawnattr_setschedpolicy(attr, host);
}

int __ape_shim_posix_spawnattr_getschedpolicy(const posix_spawnattr_t *attr, int *out) {
    int host, rc = posix_spawnattr_getschedpolicy(attr, &host);
    if (!rc) *out = policy_to_linux(host);
    return rc;
}

int __ape_shim_pthread_attr_setschedpolicy(pthread_attr_t *attr, int lin) {
    int host, rc = policy_to_host(lin, &host);
    return rc ? rc : pthread_attr_setschedpolicy(attr, host);
}

int __ape_shim_pthread_attr_getschedpolicy(const pthread_attr_t *attr, int *out) {
    int host, rc = pthread_attr_getschedpolicy(attr, &host);
    if (!rc) *out = policy_to_linux(host);
    return rc;
}

int __ape_shim_pthread_setschedparam(pthread_t thread, int lin, const struct sched_param *param) {
    int host, rc = policy_to_host(lin, &host);
    return rc ? rc : pthread_setschedparam(thread, host, param);
}

int __ape_shim_pthread_getschedparam(pthread_t thread, int *out, struct sched_param *param) {
    int host, rc = pthread_getschedparam(thread, &host, param);
    if (!rc) *out = policy_to_linux(host);
    return rc;
}
