// POSIX interval timers, which cosmo has no wrapper for. Linux gets the raw
// syscalls; every other host runs them on ITIMER_REAL, which is one timer per
// process and only ever raises SIGALRM -- so a second live timer is refused
// with EAGAIN and any other notification with ENOTSUP. sigevent and
// itimerspec pass through as void *: the libc crate's Linux layouts already
// are the kernel's, and repacking them would only invite a mismatch.

#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/time.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

// Linux-coded, read out of the caller's sigevent by offset.
#define LIN_SIGEV_SIGNAL 0
#define SEV_SIGNO_OFF    8
#define SEV_NOTIFY_OFF   12

#define FALLBACK_TIMER ((void *)(intptr_t)0x7a1e)

static int fallback_live;

// Both are four longs (interval sec/nsec, value sec/nsec). Nanoseconds round
// up: a truncating divide turns a sub-microsecond delay into 0, which
// ITIMER_REAL reads as "disarm".

static void its_to_val(const long *its, struct itimerval *out) {
    out->it_interval.tv_sec = its[0];
    out->it_interval.tv_usec = (its[1] + 999) / 1000;
    out->it_value.tv_sec = its[2];
    out->it_value.tv_usec = (its[3] + 999) / 1000;
}

static void val_to_its(const struct itimerval *val, long *out) {
    out[0] = val->it_interval.tv_sec;
    out[1] = val->it_interval.tv_usec * 1000;
    out[2] = val->it_value.tv_sec;
    out[3] = val->it_value.tv_usec * 1000;
}

int timer_create(int clockid, void *sevp, void **timerid) {
    if (IsLinux()) {
        int kid;
        long r = __ape_raw_syscall(__NR_timer_create, clockid, (long)sevp,
                                   (long)&kid, 0, 0);
        if (__ape_syscall_ret(r) == -1) return -1;
        *timerid = (void *)(intptr_t)kid;
        return 0;
    }
    // A null sigevent means SIGEV_SIGNAL/SIGALRM; anything else has to say so.
    if (sevp) {
        int signo = *(const int *)((const char *)sevp + SEV_SIGNO_OFF);
        int notify = *(const int *)((const char *)sevp + SEV_NOTIFY_OFF);
        if (notify != LIN_SIGEV_SIGNAL || signo != SIGALRM)
            return errno = ENOTSUP, -1;
    }
    if (fallback_live) return errno = EAGAIN, -1;
    fallback_live = 1;
    *timerid = FALLBACK_TIMER;
    return 0;
}

int timer_settime(void *timerid, int flags, const void *new_value,
                  void *old_value) {
    if (IsLinux())
        return (int)__ape_syscall_ret(__ape_raw_syscall(
            __NR_timer_settime, (int)(intptr_t)timerid, flags, (long)new_value,
            (long)old_value, 0));
    if (flags) return errno = ENOTSUP, -1; // TIMER_ABSTIME has no itimerval form
    struct itimerval want, had;
    its_to_val((const long *)new_value, &want);
    if (setitimer(ITIMER_REAL, &want, old_value ? &had : NULL) < 0) return -1;
    if (old_value) val_to_its(&had, (long *)old_value);
    return 0;
}

int timer_gettime(void *timerid, void *curr_value) {
    if (IsLinux())
        return (int)__ape_syscall_ret(__ape_raw_syscall(
            __NR_timer_gettime, (int)(intptr_t)timerid, (long)curr_value, 0, 0, 0));
    struct itimerval now;
    if (getitimer(ITIMER_REAL, &now) < 0) return -1;
    val_to_its(&now, (long *)curr_value);
    return 0;
}

int timer_delete(void *timerid) {
    if (IsLinux())
        return (int)__ape_syscall_ret(
            __ape_raw_syscall(__NR_timer_delete, (int)(intptr_t)timerid, 0, 0, 0, 0));
    struct itimerval off = {{0, 0}, {0, 0}};
    setitimer(ITIMER_REAL, &off, NULL);
    fallback_live = 0;
    return 0;
}
