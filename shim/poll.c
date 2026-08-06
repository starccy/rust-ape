// poll() for the Linux-personality shim.
//
// The POLL* bits are runtime constants under cosmo (WSAPoll's on Windows),
// while the Rust world bakes in musl's. Translate events on the way in and
// both fields on the way out. The events field is translated in place and
// restored afterwards, because callers reuse their pollfd arrays.
//
// Values come from tables.h (`cargo xtask gen-shim`).

#include <poll.h>
#include <stddef.h>

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

int __ape_shim_poll(struct pollfd *fds, unsigned long n, int timeout) {
    for (unsigned long i = 0; i < n; i++) fds[i].events = ev_to_host(fds[i].events);
    int r = poll(fds, n, timeout);
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
    for (unsigned long i = 0; i < n; i++) {
        fds[i].events = ev_to_linux(fds[i].events);
        fds[i].revents = ev_to_linux(fds[i].revents);
    }
    return r;
}
