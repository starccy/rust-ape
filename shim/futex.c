// Futex rerouting for the Linux-personality shim.
//
// std's futex.rs goes through raw `syscall(SYS_futex, ...)`, and cosmo's
// syscall() is a stub that only knows three syscall numbers — everything
// else is ENOSYS. std treated that as a spurious wakeup, which quietly
// turned every Mutex/Condvar into a spin loop on all platforms, Linux
// included. std itself stays unpatched; its syscall() calls resolve
// through the patched libc crate to __ape_shim_syscall below, which sits
// on cosmo's real cross-platform futex (Linux futex, Windows
// WaitOnAddress, ulock on mac).
//
// cosmo_futex_wait returns 0 or a negative *host-coded* errno (verified
// empirically; it does not touch the errno variable). That coding never
// leaves this file; the Rust side only sees the 1/0 result.

#include <errno.h>
#include <stdarg.h>
#include <stddef.h>
#include <sys/random.h>
#include <time.h>
#include <libc/cosmo.h>
#include <libc/sysv/consts/clock.h>

#include "tables.h"

// libc::syscall() itself. std's futex parker, parking_lot's, and the
// getrandom crate all issue raw `syscall(SYS_...)`, and cosmo's own
// syscall() is a stub that answers ENOSYS to nearly everything, which
// std's error handling read as a spurious wakeup, silently degrading every
// Mutex/Condvar into a spin loop on all platforms, Linux included.
// Rerouting the function covers std and every third-party crate at once.
// Only the futex operations in actual use and getrandom are translated;
// every other syscall number keeps the ENOSYS an unshimmed build had,
// which callers with fallbacks already handle.
//
// Linux futex semantics reproduced faithfully: 0 = woken; -1 with EAGAIN =
// value mismatch, EINTR = signal, ETIMEDOUT = timeout expired. Plain
// FUTEX_WAIT takes a RELATIVE timeout; FUTEX_WAIT_BITSET (std's choice, to
// get absolute timeouts) an ABSOLUTE one — cosmo_futex_wait wants absolute,
// so the relative form is anchored to the clock the flags select. The errno
// values are set host-coded here; the errno shim hands the Rust side the
// musl coding its comparisons expect (std and parking_lot both compare
// against EINTR/EAGAIN/ETIMEDOUT).
long __ape_shim_syscall(long n, ...) {
    if (n == SHIM_LIN_SYS_futex) {
        va_list ap;
        va_start(ap, n);
        void *addr = va_arg(ap, void *);
        int op = va_arg(ap, int);
        int val = va_arg(ap, int);
        const struct timespec *ts = va_arg(ap, const struct timespec *);
        void *uaddr2 = va_arg(ap, void *);
        unsigned bitset = va_arg(ap, unsigned);
        va_end(ap);
        (void)uaddr2;
        int cmd = op & ~(SHIM_LIN_FUTEX_PRIVATE_FLAG | SHIM_LIN_FUTEX_CLOCK_REALTIME);
        int pshare = !(op & SHIM_LIN_FUTEX_PRIVATE_FLAG);
        int clock = (op & SHIM_LIN_FUTEX_CLOCK_REALTIME) ? CLOCK_REALTIME
                                                         : CLOCK_MONOTONIC;
        if (cmd == SHIM_LIN_FUTEX_WAIT) {
            struct timespec abs, *deadline = 0;
            if (ts) {
                clock_gettime(clock, &abs);
                abs.tv_sec += ts->tv_sec;
                abs.tv_nsec += ts->tv_nsec;
                if (abs.tv_nsec >= 1000000000) {
                    abs.tv_nsec -= 1000000000;
                    abs.tv_sec++;
                }
                deadline = &abs;
            }
            int r = cosmo_futex_wait((_Atomic(int) *)addr, val, pshare, clock,
                                     deadline);
            if (r < 0) return errno = -r, -1;
            return 0;
        }
        if (cmd == SHIM_LIN_FUTEX_WAIT_BITSET) {
            // A partial bitset would need real kernel support; the only users
            // pass MATCH_ANY, which makes this a plain absolute-deadline wait.
            if (bitset != (unsigned)SHIM_LIN_FUTEX_BITSET_MATCH_ANY)
                return errno = ENOSYS, -1;
            int r = cosmo_futex_wait((_Atomic(int) *)addr, val, pshare, clock, ts);
            if (r < 0) return errno = -r, -1;
            return 0;
        }
        if (cmd == SHIM_LIN_FUTEX_WAKE ||
            (cmd == SHIM_LIN_FUTEX_WAKE_BITSET &&
             bitset == (unsigned)SHIM_LIN_FUTEX_BITSET_MATCH_ANY)) {
            int r = cosmo_futex_wake((_Atomic(int) *)addr, val, pshare);
            if (r < 0) return errno = -r, -1;
            return r;
        }
        // requeue/PI/partial-bitset operations: nobody in the crate set
        // issues them
        return errno = ENOSYS, -1;
    }
    if (n == SHIM_LIN_SYS_getrandom) {
        va_list ap;
        va_start(ap, n);
        void *buf = va_arg(ap, void *);
        size_t len = va_arg(ap, size_t);
        unsigned flags = va_arg(ap, unsigned);
        va_end(ap);
        // GRND_NONBLOCK/GRND_RANDOM are 1/2 on both sides; unknown bits are
        // cosmo's to reject. This is cosmo's own cross-platform getrandom
        // (host RNG on NT), so the crate's /dev/urandom fallback never runs.
        return getrandom(buf, len, flags);
    }
    return errno = ENOSYS, -1;
}
