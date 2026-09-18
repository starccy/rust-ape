// pthread bits whose numbers differ between musl and cosmo.
//
// pthread_attr_setstacksize: musl's PTHREAD_STACK_MIN is 2048 and cosmo's is
// 32768, so a size Rust considers valid gets EINVAL. Round it up instead.

#include <errno.h>
#include <pthread.h>
#include <limits.h>

#include "tables.h"

int __ape_shim_pthread_attr_setstacksize(pthread_attr_t *attr, size_t size) {
    if (size < PTHREAD_STACK_MIN) size = PTHREAD_STACK_MIN;
    return pthread_attr_setstacksize(attr, size);
}

int __ape_shim_pthread_mutexattr_settype(pthread_mutexattr_t *attr, int lin) {
    int host;
    switch (lin) {
        case SHIM_LIN_PTHREAD_MUTEX_NORMAL: host = PTHREAD_MUTEX_NORMAL; break;
        case SHIM_LIN_PTHREAD_MUTEX_RECURSIVE: host = PTHREAD_MUTEX_RECURSIVE; break;
        case SHIM_LIN_PTHREAD_MUTEX_ERRORCHECK: host = PTHREAD_MUTEX_ERRORCHECK; break;
        default: return EINVAL;
    }
    return pthread_mutexattr_settype(attr, host);
}

// PTHREAD_PROCESS_SHARED is 1 for musl and 4 for cosmo.
static int pshared_to_host(int lin, int *out) {
    if (lin == SHIM_LIN_PTHREAD_PROCESS_PRIVATE) return *out = PTHREAD_PROCESS_PRIVATE, 0;
    if (lin == SHIM_LIN_PTHREAD_PROCESS_SHARED) return *out = PTHREAD_PROCESS_SHARED, 0;
    return EINVAL;
}

static int pshared_to_linux(int host) {
    return host == PTHREAD_PROCESS_SHARED ? SHIM_LIN_PTHREAD_PROCESS_SHARED
                                          : SHIM_LIN_PTHREAD_PROCESS_PRIVATE;
}

int __ape_shim_pthread_mutexattr_setpshared(pthread_mutexattr_t *attr, int lin) {
    int host, rc = pshared_to_host(lin, &host);
    return rc ? rc : pthread_mutexattr_setpshared(attr, host);
}

int __ape_shim_pthread_mutexattr_getpshared(const pthread_mutexattr_t *attr, int *out) {
    int host, rc = pthread_mutexattr_getpshared(attr, &host);
    if (!rc) *out = pshared_to_linux(host);
    return rc;
}

int __ape_shim_pthread_condattr_setpshared(pthread_condattr_t *attr, int lin) {
    int host, rc = pshared_to_host(lin, &host);
    return rc ? rc : pthread_condattr_setpshared(attr, host);
}

int __ape_shim_pthread_condattr_getpshared(const pthread_condattr_t *attr, int *out) {
    int host, rc = pthread_condattr_getpshared(attr, &host);
    if (!rc) *out = pshared_to_linux(host);
    return rc;
}

int __ape_shim_pthread_rwlockattr_setpshared(pthread_rwlockattr_t *attr, int lin) {
    int host, rc = pshared_to_host(lin, &host);
    return rc ? rc : pthread_rwlockattr_setpshared(attr, host);
}

int __ape_shim_pthread_rwlockattr_getpshared(const pthread_rwlockattr_t *attr, int *out) {
    int host, rc = pthread_rwlockattr_getpshared(attr, &host);
    if (!rc) *out = pshared_to_linux(host);
    return rc;
}

int __ape_shim_pthread_barrierattr_setpshared(pthread_barrierattr_t *attr, int lin) {
    int host, rc = pshared_to_host(lin, &host);
    return rc ? rc : pthread_barrierattr_setpshared(attr, host);
}
