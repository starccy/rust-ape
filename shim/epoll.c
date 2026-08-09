// epoll() for the Linux-personality shim.
//
// Cosmopolitan deleted its epoll on 2024-09-01 (libc/sock/epoll.c, 1655
// lines). That implementation was wepoll-shaped: it drove `\Device\Afd`
// directly, so it only ever covered Windows socket handles, and the commit
// message says generalizing it to arbitrary file descriptors, plus finding
// an XNU and BSD story, was more than a C library should promise. The same
// commit spent its effort making poll() consistent across platforms
// instead.
//
// So epoll is rebuilt here, on top of that poll() rather than beside it. The
// bar is lower than a libc's: the only callers are mio and polling, and they
// use three entry points between them. Two implementations sit behind those:
//
//   Linux  -- the raw syscalls. Nothing to emulate, and it doubles as the
//             reference the emulation is checked against.
//   else   -- an interest list kept here, flattened into a pollfd array and
//             handed to cosmo's poll() on every wait.
//
// Two things the emulation cannot promise, both worth knowing before you
// read further:
//
//   * EPOLLET is accepted and ignored, so every registration behaves as
//     level-triggered. Edge semantics need the kernel to notice the moment
//     data arrives; polling for state cannot tell "still readable" from
//     "drained and refilled" without a window where events go missing.
//     Reporting too often is survivable, reporting too rarely is not.
//   * On Windows, cosmo's poll() refuses arrays past a certain size, and a
//     wait that hits that stops blocking and scans on a 10ms tick instead.
//     See poll_chunked below. It only happens to the calls cosmo would
//     otherwise fail outright.
//
// Unlike every other table in the shim there is no host side to translate
// to: cosmo has no EPOLL* constants left at all. The values here are Linux's
// from top to bottom, exactly as the Rust side spells them, and they only
// turn into host POLL* bits deep inside the emulation.

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>
#include <libc/sysv/consts/nr.h>

#include "syscall.h"

// musl's struct epoll_event: packed on x86_64, naturally aligned on aarch64.
// The kernel wants the same thing, so on Linux the pointer goes straight
// through without a repack.
#ifdef __x86_64__
#define SHIM_EPOLL_PACKED __attribute__((packed))
#else
#define SHIM_EPOLL_PACKED
#endif

struct shim_epoll_event {
    uint32_t events;
    uint64_t data;
} SHIM_EPOLL_PACKED;

// aarch64 Linux has no epoll_wait, only epoll_pwait, so both arches go
// through pwait with an empty mask. The 8 is sizeof(sigset_t) as the kernel
// counts it, not as musl declares it.
#define SHIM_KERNEL_SIGSETSIZE 8

// ---------------------------------------------------------------------------
// The emulation, for hosts without epoll.
//
// One pipe per epoll set does double duty: the read end is what we hand back
// as the epoll fd, so close() and O_CLOEXEC behave without any special
// casing, and it is also entry 0 of every poll() we issue, which is how a
// thread sitting in epoll_wait finds out that another thread changed the
// interest list. epoll_ctl writes a byte; the waiter drains it, rebuilds its
// pollfd array from the current list, and polls again with whatever timeout
// is left. Rebuilding from the list rather than patching the array in place
// is what keeps a concurrent change from being lost -- the failure mode that
// mio's own poll backend has (tokio-rs/mio#1874).

// Linux's EPOLL* values, hardcoded: there is no cosmo side to read them from.
#define SHIM_EPOLLIN 0x001u
#define SHIM_EPOLLPRI 0x002u
#define SHIM_EPOLLOUT 0x004u
#define SHIM_EPOLLERR 0x008u
#define SHIM_EPOLLHUP 0x010u
#define SHIM_EPOLLRDHUP 0x2000u

#define SHIM_EPOLL_CTL_ADD 1
#define SHIM_EPOLL_CTL_DEL 2
#define SHIM_EPOLL_CTL_MOD 3

// Linux's EPOLL_CLOEXEC, which is O_CLOEXEC's value there.
#define SHIM_EPOLL_CLOEXEC 02000000

struct shim_epoll_entry {
    int fd;
    uint32_t events;
    uint64_t data;
};

// An epoll fd can be duplicated, and mio does exactly that: tokio's runtime
// clones the selector, which is an OwnedFd::try_clone, which is
// fcntl(F_DUPFD_CLOEXEC). Both numbers then name the same set, so a set owns
// a list of handles rather than one. shim/open.c calls
// __ape_shim_epoll_note_dup on the way out of the two fcntl duplication
// commands. Plain dup()/dup2() are not intercepted; nothing in the supported
// crate set reaches an epoll fd that way.
#define SHIM_EPOLL_MAX_HANDLES 8

struct shim_epoll_set {
    int used;
    int handles[SHIM_EPOLL_MAX_HANDLES]; // [0] is the pipe's read end
    int nhandles;
    int waker; // write end
    struct shim_epoll_entry *entries;
    int count;
    int cap;
    pthread_mutex_t lock;
};

// A process has one or two of these in practice, never a crowd.
#define SHIM_EPOLL_MAX_SETS 32
static struct shim_epoll_set g_sets[SHIM_EPOLL_MAX_SETS];
static pthread_mutex_t g_sets_lock = PTHREAD_MUTEX_INITIALIZER;

// Caller must hold g_sets_lock.
static struct shim_epoll_set *set_by_handle_locked(int fd) {
    for (int i = 0; i < SHIM_EPOLL_MAX_SETS; i++) {
        if (!g_sets[i].used) continue;
        for (int h = 0; h < g_sets[i].nhandles; h++)
            if (g_sets[i].handles[h] == fd) return &g_sets[i];
    }
    return NULL;
}

static struct shim_epoll_set *set_by_handle(int fd) {
    pthread_mutex_lock(&g_sets_lock);
    struct shim_epoll_set *found = set_by_handle_locked(fd);
    pthread_mutex_unlock(&g_sets_lock);
    return found;
}

static int use_syscalls(void);

// Called from shim/open.c after a successful fcntl duplication. Every fcntl
// that duplicates an fd lands here, so leave early when there is nothing to
// track.
void __ape_shim_epoll_note_dup(int oldfd, int newfd) {
    if (oldfd == newfd || use_syscalls()) return;
    pthread_mutex_lock(&g_sets_lock);
    struct shim_epoll_set *s = set_by_handle_locked(oldfd);
    if (s && !set_by_handle_locked(newfd) && s->nhandles < SHIM_EPOLL_MAX_HANDLES)
        s->handles[s->nhandles++] = newfd;
    pthread_mutex_unlock(&g_sets_lock);
}

static short epoll_to_poll(uint32_t e) {
    short p = 0;
    if (e & SHIM_EPOLLIN) p |= POLLIN;
    if (e & SHIM_EPOLLOUT) p |= POLLOUT;
    if (e & SHIM_EPOLLPRI) p |= POLLPRI;
#ifdef POLLRDHUP
    if (e & SHIM_EPOLLRDHUP) p |= POLLRDHUP;
#endif
    return p; // EPOLLET and friends carry no poll() meaning; see the header
}

static uint32_t poll_to_epoll(short r) {
    uint32_t e = 0;
    if (r & POLLIN) e |= SHIM_EPOLLIN;
    if (r & POLLOUT) e |= SHIM_EPOLLOUT;
    if (r & POLLPRI) e |= SHIM_EPOLLPRI;
    if (r & POLLERR) e |= SHIM_EPOLLERR;
    if (r & POLLHUP) e |= SHIM_EPOLLHUP;
#ifdef POLLRDHUP
    if (r & POLLRDHUP) e |= SHIM_EPOLLRDHUP;
#endif
    // A closed or bogus fd has no epoll spelling of its own. epoll reports
    // such a registration as an error, and so do we.
    if (r & POLLNVAL) e |= SHIM_EPOLLERR;
    return e;
}

static int64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void drain(int fd) {
    char buf[64];
    while (read(fd, buf, sizeof(buf)) > 0) continue;
}

// Caller must hold g_sets_lock. Drops `fd` from whatever set still claims it,
// and retires the set once its last handle is gone.
//
// This is how a closed epoll fd actually gets noticed. close() is not
// intercepted, so a set outlives the fd the caller dropped; the kernel will
// not hand a still-open number to a new pipe, so seeing our fresh pipe land
// on a recorded handle proves that set is dead. Without this, a program that
// builds and tears down runtimes in a loop (tokio does, on shutdown) ends up
// with two sets claiming one number and the newer registrations landing in
// the older set's list.
static void forget_fd(int fd) {
    for (int i = 0; i < SHIM_EPOLL_MAX_SETS; i++) {
        if (!g_sets[i].used) continue;
        int hit = 0;
        for (int h = 0; h < g_sets[i].nhandles; h++)
            if (g_sets[i].handles[h] == fd) {
                g_sets[i].handles[h] = g_sets[i].handles[--g_sets[i].nhandles];
                h--;
                hit = 1;
            }
        if (hit && g_sets[i].nhandles == 0) {
            close(g_sets[i].waker);
            free(g_sets[i].entries);
            pthread_mutex_destroy(&g_sets[i].lock);
            memset(&g_sets[i], 0, sizeof(g_sets[i]));
        }
    }
}

static int emu_create1(int flags) {
    int p[2];
    if (pipe(p) == -1) return -1;
    // The waker must never block a thread calling epoll_ctl, and the drain
    // side must stop rather than wait once it is empty.
    for (int i = 0; i < 2; i++) {
        int fl = fcntl(p[i], F_GETFL);
        if (fl != -1) fcntl(p[i], F_SETFL, fl | O_NONBLOCK);
        if (flags & SHIM_EPOLL_CLOEXEC) fcntl(p[i], F_SETFD, FD_CLOEXEC);
    }

    pthread_mutex_lock(&g_sets_lock);
    // The numbers we just got cannot belong to a live set. See forget_fd.
    forget_fd(p[0]);
    forget_fd(p[1]);

    // Sets whose handles are all closed without their numbers having been
    // reused yet. Only reclaims the waker fd yet to be leaked; correctness
    // comes from forget_fd above.
    for (int i = 0; i < SHIM_EPOLL_MAX_SETS; i++) {
        if (!g_sets[i].used) continue;
        int alive = 0;
        for (int h = 0; h < g_sets[i].nhandles; h++)
            if (fcntl(g_sets[i].handles[h], F_GETFD) != -1) alive = 1;
        if (alive) continue;
        close(g_sets[i].waker);
        free(g_sets[i].entries);
        pthread_mutex_destroy(&g_sets[i].lock);
        memset(&g_sets[i], 0, sizeof(g_sets[i]));
    }

    struct shim_epoll_set *s = NULL;
    for (int i = 0; i < SHIM_EPOLL_MAX_SETS; i++)
        if (!g_sets[i].used) {
            s = &g_sets[i];
            break;
        }
    if (!s) {
        pthread_mutex_unlock(&g_sets_lock);
        close(p[0]);
        close(p[1]);
        errno = EMFILE;
        return -1;
    }
    s->used = 1;
    s->handles[0] = p[0];
    s->nhandles = 1;
    s->waker = p[1];
    s->entries = NULL;
    s->count = 0;
    s->cap = 0;
    pthread_mutex_init(&s->lock, NULL);
    pthread_mutex_unlock(&g_sets_lock);
    return p[0];
}

static struct shim_epoll_entry *find_entry(struct shim_epoll_set *s, int fd) {
    for (int i = 0; i < s->count; i++)
        if (s->entries[i].fd == fd) return &s->entries[i];
    return NULL;
}

static int emu_ctl(int epfd, int op, int fd, struct shim_epoll_event *ev) {
    struct shim_epoll_set *s = set_by_handle(epfd);
    if (!s) {
        errno = EBADF;
        return -1;
    }
    if (fd == epfd) {
        errno = EINVAL;
        return -1;
    }
    if ((op == SHIM_EPOLL_CTL_ADD || op == SHIM_EPOLL_CTL_MOD) && !ev) {
        errno = EFAULT;
        return -1;
    }

    int rc = 0;
    pthread_mutex_lock(&s->lock);
    struct shim_epoll_entry *e = find_entry(s, fd);
    switch (op) {
        case SHIM_EPOLL_CTL_ADD:
            if (e) {
                errno = EEXIST;
                rc = -1;
                break;
            }
            if (s->count == s->cap) {
                int cap = s->cap ? s->cap * 2 : 8;
                void *grown = realloc(s->entries, (size_t)cap * sizeof(*s->entries));
                if (!grown) {
                    errno = ENOMEM;
                    rc = -1;
                    break;
                }
                s->entries = grown;
                s->cap = cap;
            }
            s->entries[s->count].fd = fd;
            s->entries[s->count].events = ev->events;
            s->entries[s->count].data = ev->data;
            s->count++;
            break;
        case SHIM_EPOLL_CTL_MOD:
            if (!e) {
                errno = ENOENT;
                rc = -1;
                break;
            }
            e->events = ev->events;
            e->data = ev->data;
            break;
        case SHIM_EPOLL_CTL_DEL:
            if (!e) {
                errno = ENOENT;
                rc = -1;
                break;
            }
            *e = s->entries[s->count - 1];
            s->count--;
            break;
        default:
            errno = EINVAL;
            rc = -1;
    }
    pthread_mutex_unlock(&s->lock);

    if (rc == 0) {
        char b = 1;
        // Best effort: a full pipe already means a wakeup is pending.
        (void)!write(s->waker, &b, 1);
    }
    return rc;
}

// cosmo's poll() on NT sorts descriptors into a WaitForMultipleObjects group
// and a WSAPoll group, each holding 64, and answers EINVAL rather than
// truncating when one of them overflows. Its own oversized-array path splits
// the call up but gets the arithmetic wrong somewhere: measured on 4.0.2, an
// array of 64 pipes fails, and so does one of 128, while 65 through 127 come
// back fine. Rather than chase that, hand cosmo chunks small enough that
// neither group can fill, and give up blocking in exchange: a chunk has to be
// polled with no timeout for the next one to get a turn, so a set this large
// scans and then naps on its waker.
//
// Only the calls cosmo has just refused come here, so a set that shrinks back
// under the limit goes straight back to blocking, and a host without the
// limit never arrives at all.
#define SHIM_POLL_CHUNK 32
#define SHIM_POLL_NAP_MS 10

static int poll_chunked(struct pollfd *pfds, int total, int ms) {
    int64_t deadline = ms < 0 ? -1 : now_ms() + ms;
    for (;;) {
        int got = 0;
        for (int base = 0; base < total; base += SHIM_POLL_CHUNK) {
            int k = total - base;
            if (k > SHIM_POLL_CHUNK) k = SHIM_POLL_CHUNK;
            int r = poll(pfds + base, (nfds_t)k, 0);
            if (r == -1) return -1;
            got += r;
        }
        if (got) return got;

        int nap = SHIM_POLL_NAP_MS;
        if (deadline >= 0) {
            int64_t left = deadline - now_ms();
            if (left <= 0) return 0;
            if (left < nap) nap = (int)left;
        }
        // Entry 0 is the waker, so a change to the interest list cuts the nap
        // short. Its byte stays in the pipe for the scan above to find.
        struct pollfd waker = {pfds[0].fd, POLLIN, 0};
        if (poll(&waker, 1, nap) == -1) return -1;
    }
}

static int emu_wait(int epfd, struct shim_epoll_event *out, int maxevents,
                    int timeout) {
    struct shim_epoll_set *s = set_by_handle(epfd);
    if (!s) {
        errno = EBADF;
        return -1;
    }
    if (maxevents <= 0) {
        errno = EINVAL;
        return -1;
    }

    int64_t deadline = timeout < 0 ? -1 : now_ms() + timeout;
    struct pollfd *pfds = NULL;
    int pcap = 0;
    int bad_polls = 0;

    for (;;) {
        // Snapshot the interest list. Entry 0 is the waker, so a change made
        // while we are inside poll() below cuts the wait short.
        pthread_mutex_lock(&s->lock);
        int n = s->count;
        if (n + 1 > pcap) {
            int cap = n + 1;
            void *grown = realloc(pfds, (size_t)cap * sizeof(*pfds));
            if (!grown) {
                pthread_mutex_unlock(&s->lock);
                free(pfds);
                errno = ENOMEM;
                return -1;
            }
            pfds = grown;
            pcap = cap;
        }
        pfds[0].fd = s->handles[0];
        pfds[0].events = POLLIN;
        pfds[0].revents = 0;
        for (int i = 0; i < n; i++) {
            pfds[i + 1].fd = s->entries[i].fd;
            pfds[i + 1].events = epoll_to_poll(s->entries[i].events);
            pfds[i + 1].revents = 0;
        }
        pthread_mutex_unlock(&s->lock);

        int ms;
        if (deadline < 0) {
            ms = -1;
        } else {
            int64_t left = deadline - now_ms();
            ms = left <= 0 ? 0 : (left > INT_MAX ? INT_MAX : (int)left);
        }

        int rc = poll(pfds, (nfds_t)(n + 1), ms);
        if (rc == -1 && errno == EINVAL && n + 1 > SHIM_POLL_CHUNK)
            rc = poll_chunked(pfds, n + 1, ms);
        if (rc == -1) {
            if (errno == EINTR) {
                if (deadline < 0 || now_ms() < deadline) continue;
                rc = 0;
            } else if (errno == EBADF || errno == ENOTSOCK) {
                // One bad descriptor takes the whole call down here, while
                // Linux would have marked just that entry POLLNVAL. It
                // happens when a registered fd is closed without being
                // deregistered first, which epoll explicitly allows: the
                // closed fd simply leaves the set. Find the offenders one at
                // a time, drop them, and go again.
                //
                // Coming up empty is not a contradiction. The close can land
                // while poll() is already in flight, so by the time we probe,
                // the fd is gone from our list too and there is nothing to
                // drop; the retry budget covers that without spinning
                // forever on a genuinely wedged set.
                int dropped = 0;
                for (int i = 1; i <= n; i++) {
                    struct pollfd one = {pfds[i].fd, pfds[i].events, 0};
                    int r1 = poll(&one, 1, 0);
                    if (r1 != -1 && !(one.revents & POLLNVAL)) continue;
                    pthread_mutex_lock(&s->lock);
                    struct shim_epoll_entry *e = find_entry(s, pfds[i].fd);
                    if (e) {
                        *e = s->entries[s->count - 1];
                        s->count--;
                        dropped++;
                    }
                    pthread_mutex_unlock(&s->lock);
                }
                if (dropped || ++bad_polls <= 8) continue;
                free(pfds);
                errno = EBADF;
                return -1;
            } else {
                free(pfds);
                return -1;
            }
        }

        if (rc > 0 && (pfds[0].revents & POLLIN)) {
            // Somebody changed the list. Drain and rebuild rather than trust
            // the array we just polled.
            drain(s->handles[0]);
            rc--;
            if (rc == 0 && (deadline < 0 || now_ms() < deadline)) continue;
        }

        // Match revents back to tokens under the lock, since an fd may have
        // been dropped from the list while poll() was running.
        int got = 0;
        if (rc > 0) {
            pthread_mutex_lock(&s->lock);
            for (int i = 1; i <= n && got < maxevents; i++) {
                if (!pfds[i].revents) continue;
                struct shim_epoll_entry *e = find_entry(s, pfds[i].fd);
                if (!e) continue; // deregistered mid-poll
                out[got].events = poll_to_epoll(pfds[i].revents);
                out[got].data = e->data;
                got++;
            }
            pthread_mutex_unlock(&s->lock);
        }

        if (got > 0 || (deadline >= 0 && now_ms() >= deadline)) {
            free(pfds);
            return got;
        }
        // Woken with nothing to report and time still on the clock.
    }
}

// Which implementation answers. Linux takes the syscalls unless
// RUST_APE_EPOLL_EMULATE is set, which exists so the emulation can be run
// against the real thing on a machine that has both. The race on `cached` is
// harmless: every racing thread computes the same answer.
static int use_syscalls(void) {
    static int cached = -1;
    if (cached < 0) {
        const char *v = getenv("RUST_APE_EPOLL_EMULATE");
        cached = IsLinux() && !(v && *v);
    }
    return cached;
}

int __ape_shim_epoll_create1(int flags) {
    if (use_syscalls())
        return (int)__ape_syscall_ret(
            __ape_raw_syscall(__NR_epoll_create1, flags, 0, 0, 0, 0));
    return emu_create1(flags);
}

int __ape_shim_epoll_ctl(int epfd, int op, int fd, struct shim_epoll_event *ev) {
    if (use_syscalls())
        return (int)__ape_syscall_ret(
            __ape_raw_syscall(__NR_epoll_ctl, epfd, op, fd, (long)ev, 0));
    return emu_ctl(epfd, op, fd, ev);
}

int __ape_shim_epoll_wait(int epfd, struct shim_epoll_event *events,
                          int maxevents, int timeout) {
    if (use_syscalls())
        return (int)__ape_syscall_ret(
            __ape_raw_syscall6(__NR_epoll_pwait, epfd, (long)events, maxevents,
                               timeout, 0, SHIM_KERNEL_SIGSETSIZE));
    return emu_wait(epfd, events, maxevents, timeout);
}
