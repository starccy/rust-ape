// poll() for the Linux-personality shim.
//
// The POLL* bits are runtime constants under cosmo (WSAPoll's on Windows),
// while the Rust world bakes in musl's. Translate events on the way in and
// both fields on the way out. The events field is translated in place and
// restored afterwards, because callers reuse their pollfd arrays.
//
// Values come from tables.h (`cargo xtask gen-shim`).

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <time.h>

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
        struct timespec ts = {nap / 1000, (nap % 1000) * 1000000};
        if (ppoll(NULL, 0, &ts, mask) == -1) return -1;
    }
}

int __ape_shim_poll(struct pollfd *fds, unsigned long n, int timeout) {
    for (unsigned long i = 0; i < n; i++) fds[i].events = ev_to_host(fds[i].events);
    int r = poll(fds, n, timeout);
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
    int r = ppoll(fds, n, timeout, sigmask);
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
