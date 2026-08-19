// poll() for the Linux-personality shim.
//
// The POLL* bits are runtime constants under cosmo (WSAPoll's on Windows),
// while the Rust world bakes in musl's. Translate events on the way in and
// both fields on the way out. The events field is translated in place and
// restored afterwards, because callers reuse their pollfd arrays.
//
// Values come from tables.h (`cargo xtask gen-shim`).
//
// This file also owns the wait itself, for both entry points here and for
// shim/epoll.c, because on Windows cosmo's poll() takes about 11ms to notice
// that a pipe became readable. See __ape_shim_host_ppoll below.

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <time.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>

#include "tables.h"

struct pbit {
    short linux_bit;
    const int16_t *host;
};

#define X(name, lin) { lin, &name },
static const struct pbit kPolls[] = { SHIM_POLL_TABLE(X) };
#undef X
#define NPOLLS (sizeof(kPolls) / sizeof(kPolls[0]))

static short ev_to_host(short lin) {
    short host = 0;
    for (size_t i = 0; i < NPOLLS; i++)
        if (lin & kPolls[i].linux_bit) host |= *kPolls[i].host;
    return host; // unknown bits are dropped; every real POLL* is in the table
}

static short ev_to_linux(short host) {
    short lin = 0;
    for (size_t i = 0; i < NPOLLS; i++) {
        int16_t h = *kPolls[i].host;
        if (h && (host & h)) lin |= kPolls[i].linux_bit;
    }
    return lin;
}

// On Windows cosmo refuses arrays past a size it does not document, and its
// own splitting of oversized ones misses cases; shim/epoll.c's header has the
// measurements. Retry a refused call in chunks small enough to land, which
// means giving up the blocking wait: a chunk has to be polled with no timeout
// for the next one to get a turn, so the array gets scanned on a 10ms tick
// instead. Only calls cosmo has already refused come here.
#define SHIM_POLL_CHUNK 32
#define SHIM_POLL_NAP_MS 10

static int64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

// Sleeps between polled-phase checks. Uses nanosleep instead of an
// empty-set ppoll: on NT, cosmo's poll rounds the timeout up to the system
// tick (~15.6ms), while nanosleep uses a high-resolution timer and can
// sleep for less than a millisecond. The caller's signal mask is not
// applied during the sleep; a signal it would unblock is noticed by the
// next zero-timeout ppoll, at most one sleep later. A handler interrupting
// the sleep returns EINTR, same as ppoll.
static int polled_nap(long us) {
    struct timespec nap = {us / 1000000, us % 1000000 * 1000};
    return nanosleep(&nap, NULL);
}

static int chunked(struct pollfd *fds, unsigned long n, int64_t deadline,
                   const sigset_t *mask) {
    struct timespec zero = {0, 0};
    for (;;) {
        int got = 0;
        for (unsigned long base = 0; base < n; base += SHIM_POLL_CHUNK) {
            unsigned long k = n - base;
            if (k > SHIM_POLL_CHUNK) k = SHIM_POLL_CHUNK;
            int r = ppoll(fds + base, k, &zero, mask);
            if (r == -1) return -1;
            got += r;
        }
        if (got) return got;

        int64_t nap = SHIM_POLL_NAP_MS;
        if (deadline >= 0) {
            int64_t left = deadline - now_ms();
            if (left <= 0) return 0;
            if (left < nap) nap = left;
        }
        if (polled_nap(nap * 1000) == -1) return -1;
    }
}

// A blocking poll, with a polled first phase on Windows.
//
// cosmo's NT poll() sorts descriptors into a WaitForMultipleObjects group and
// a WSAPoll group. Sockets are fine: the wait happens inside WSAPoll and an
// event lands in microseconds. Everything else -- pipes above all -- is
// waited on by handle, and a pipe handle is not signalled when the other end
// writes to it. What actually ends the wait is one of the timed rounds the
// loop makes anyway, so the readability shows up on the next lap rather than
// when it happened. Measured on 10.0.26200: a byte written to a pipe by
// another thread takes 11ms to come back out of poll(), and a poll asking for
// a 1ms timeout returns after 15ms.
//
// Win32 has no primitive for this. An anonymous pipe cannot be waited on for
// data; PeekNamedPipe in a loop is what everyone ends up doing, Cygwin's
// select() included. So that is what happens here, for a while.
//
// The latency is invisible in a benchmark of poll() itself and very visible
// in a program built out of small pipe round-trips. fish runs every command
// substitution through a pipe drained by a helper thread, so each one costs a
// wakeup: `(string escape -- $PWD)` took 2.4ms, drawing the default prompt
// took 60ms, and the interactive prompt visibly stalled after every command.
//
// Two phases, both Windows-only:
//
//   1. 500us naps for the first 10ms. That is cosmo's floor there --
//      nanosleep() runs on a high-resolution waitable timer and honors
//      ~500us, while anything shorter still costs 500us -- and it covers the
//      round-trip a thread hands to another thread and waits on.
//
//   2. 1ms naps out to 250ms. Anything not satisfied by then is not part of
//      an interactive round-trip, so past that the call hands off to cosmo
//      and parks properly: an idle wait costs no wakeups, exactly as before,
//      and only pays cosmo's 11ms if it is woken after a quarter second of
//      silence.
//
// The polling itself does not show up: 25 waits cost under a tenth of a
// millisecond of CPU, and a full second of phase 2 is about 3ms.
//
// Elsewhere none of this applies -- Linux wakes a poll() in ~40us, where the
// naps would only add latency -- so the whole thing is skipped.
//
// RUST_APE_POLL_MS moves the 250ms boundary, and 0 turns the polling off and
// leaves the wait to cosmo.
#define SHIM_NT_NAP1_US 500
#define SHIM_NT_NAP1_MS 10
#define SHIM_NT_NAP2_US 1000
#define SHIM_NT_POLLED_MS 250

static int polled_ms(void) {
    static int cached = -1;
    int ms = cached;
    if (ms < 0) {
        const char *v = getenv("RUST_APE_POLL_MS");
        char *end;
        long n;
        ms = SHIM_NT_POLLED_MS;
        if (v && *v && (n = strtol(v, &end, 10)) >= 0 && !*end && n <= 60000)
            ms = (int)n;
        cached = ms;
    }
    return ms;
}

int __ape_shim_host_ppoll(struct pollfd *fds, unsigned long n,
                          const struct timespec *timeout,
                          const sigset_t *mask) {
    int zero_timeout = timeout && !timeout->tv_sec && !timeout->tv_nsec;
    int budget = IsWindows() ? polled_ms() : 0;
    if (!budget || !n || zero_timeout)
        return ppoll(fds, n, timeout, mask);

    struct timespec zero = {0, 0};
    int r = ppoll(fds, n, &zero, mask);
    // Ready already, or an error the caller has to see -- an oversized array
    // comes back EINVAL here and is retried in chunks by our callers.
    if (r) return r;

    int64_t start = now_ms();
    int64_t deadline = -1;
    if (timeout)
        deadline = start + (int64_t)timeout->tv_sec * 1000 +
                   (timeout->tv_nsec + 999999) / 1000000;

    for (;;) {
        int64_t polled = now_ms() - start;
        if (polled >= budget) break;
        long us = polled < SHIM_NT_NAP1_MS ? SHIM_NT_NAP1_US : SHIM_NT_NAP2_US;
        if (polled_nap(us) == -1) return -1;
        if ((r = ppoll(fds, n, &zero, mask))) return r;
        if (deadline >= 0 && now_ms() >= deadline) return 0;
    }

    if (deadline < 0) return ppoll(fds, n, NULL, mask);
    int64_t left = deadline - now_ms();
    if (left <= 0) return 0;
    struct timespec rest = {left / 1000, (left % 1000) * 1000000};
    return ppoll(fds, n, &rest, mask);
}

int __ape_shim_poll(struct pollfd *fds, unsigned long n, int timeout) {
    for (unsigned long i = 0; i < n; i++) fds[i].events = ev_to_host(fds[i].events);
    struct timespec ts = {timeout / 1000, (timeout % 1000) * 1000000};
    int r = __ape_shim_host_ppoll(fds, n, timeout < 0 ? NULL : &ts, NULL);
    if (r == -1 && errno == EINVAL && n > SHIM_POLL_CHUNK)
        r = chunked(fds, n, timeout < 0 ? -1 : now_ms() + timeout, NULL);
    for (unsigned long i = 0; i < n; i++) {
        fds[i].events = ev_to_linux(fds[i].events);
        fds[i].revents = ev_to_linux(fds[i].revents);
    }
    return r;
}

int __ape_shim_ppoll(struct pollfd *fds, unsigned long n,
                     const struct timespec *timeout, const void *sigmask) {
    for (unsigned long i = 0; i < n; i++) fds[i].events = ev_to_host(fds[i].events);
    int r = __ape_shim_host_ppoll(fds, n, timeout, sigmask);
    if (r == -1 && errno == EINVAL && n > SHIM_POLL_CHUNK) {
        int64_t deadline = -1;
        if (timeout)
            deadline = now_ms() + (int64_t)timeout->tv_sec * 1000 + timeout->tv_nsec / 1000000;
        r = chunked(fds, n, deadline, sigmask);
    }
    for (unsigned long i = 0; i < n; i++) {
        fds[i].events = ev_to_linux(fds[i].events);
        fds[i].revents = ev_to_linux(fds[i].revents);
    }
    return r;
}
