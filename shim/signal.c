// The signal half of the Linux-personality shim.
//
// Three things cross this boundary and all three need translation:
//
//   - signal numbers: the universal ones (SIGKILL 9, SIGSEGV 11, ...) are
//     fixed to the Linux values by cosmo itself and pass through; the
//     divergent ones (SIGCHLD is 17 on Linux, 20 on BSDs; SIGUSR1 10 vs 30)
//     are runtime constants.
//   - struct sigaction: the layouts genuinely differ. musl is {handler,
//     128-byte sa_mask, int sa_flags, restorer}; cosmo is {handler, uint64
//     sa_flags, restorer, uint64 sa_mask}. Passing one where the other is
//     expected silently drops sa_flags — SA_SIGINFO and SA_ONSTACK included,
//     on every host — so both directions get repacked field by field here.
//   - sa_flags themselves: runtime constants under cosmo (SA_SIGINFO is 4 on
//     Linux, 0x40 on the BSDs...).
//
// Handlers are wrapped in a trampoline so the signum argument (and si_signo/
// si_errno for SA_SIGINFO handlers) arrive Linux-coded; cosmo delivers its
// host-coded numbers. The third handler argument (ucontext_t) is passed
// through untranslated — nothing in the supported crate set reads it.
//
// The sigset_t convention, established by the sigaddset/sigdelset/sigismember
// accessors, is that the bits inside a set are HOST-numbered (the accessors
// translate the signum at the boundary), and only the low 64 bits are
// meaningful. musl's sigset_t is 128 bytes but every consumer of a set in
// the final binary is a cosmo function reading a uint64, so repacking a mask
// means copying the low word, and writing one back means zeroing the tail.
//
// Not handled: the real-time range (SIGRTMIN+n) — musl computes it with a
// function, there is no constant to extract; those pass through raw.
//
// Values come from tables.h (`cargo xtask gen-shim`).

#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <libc/sysv/consts/sig.h>
#include <libc/sysv/consts/sa.h>
#include <libc/sysv/consts/ss.h>

#include "tables.h"

int __ape_shim_errno_host_to_linux(int); // errno.c

struct smap {
    int lin;
    const int *host;
};

#define X(name, lin) { lin, &name },
static const struct smap kSigs[] = { SHIM_SIG_TABLE(X) };
#undef X
#define NSIGS (sizeof(kSigs) / sizeof(kSigs[0]))

static int sig_to_host(int lin) {
    for (size_t i = 0; i < NSIGS; i++)
        if (kSigs[i].lin == lin) return *kSigs[i].host;
    return lin; // universal or unknown: pass through
}

static int sig_to_linux(int host) {
    for (size_t i = 0; i < NSIGS; i++)
        if (*kSigs[i].host == host) return kSigs[i].lin;
    return host;
}

// ---------------------------------------------------------------------------
// struct sigaction, musl's shape. Verified against the libc crate: b64
// sigset_t is [c_ulong; 16] and the field order is handler, mask, flags,
// restorer on both x86_64 and aarch64.
struct lin_sigaction {
    void *handler;
    unsigned long mask[16];
    int flags;
    void (*restorer)(void);
};

static uint64_t sa_flags_to_host(int lin) {
    uint64_t host = 0;
#define X(name, linval) if ((unsigned)lin & (unsigned)(linval)) host |= name;
    SHIM_SA_TABLE(X)
#undef X
    return host;
}

static int sa_flags_to_linux(uint64_t host) {
    unsigned lin = 0;
#define X(name, linval) if (host & name) lin |= (unsigned)(linval);
    SHIM_SA_TABLE(X)
#undef X
    return (int)lin;
}

// ---------------------------------------------------------------------------
// Handler trampolines. Slot table is indexed by HOST signum; cosmo supports
// 1..64. The table is committed before the sigaction() call so a signal
// arriving mid-install never finds a trampoline without a user handler.
#define SHIM_MAXSIG 65

static void (*volatile g_h1[SHIM_MAXSIG])(int);
static void (*volatile g_h3[SHIM_MAXSIG])(int, siginfo_t *, void *);

static int special_disposition(const void *h) {
    return h == (void *)SIG_DFL || h == (void *)SIG_IGN || h == (void *)SIG_ERR;
}

static void tramp1(int hostsig) {
    void (*h)(int) = (hostsig > 0 && hostsig < SHIM_MAXSIG) ? g_h1[hostsig] : 0;
    if (!h) return;
    int saved = errno; // shield the interrupted thread's errno protocol
    h(sig_to_linux(hostsig));
    errno = saved;
}

static void tramp3(int hostsig, siginfo_t *si, void *ctx) {
    void (*h)(int, siginfo_t *, void *) =
        (hostsig > 0 && hostsig < SHIM_MAXSIG) ? g_h3[hostsig] : 0;
    if (!h) return;
    int saved = errno;
    if (si) {
        // cosmo's siginfo_t is Linux-ABI-shaped (si_addr/si_pid/si_status all
        // at the musl offsets); only the coded values need help.
        si->si_signo = sig_to_linux(si->si_signo);
        if (si->si_errno) si->si_errno = __ape_shim_errno_host_to_linux(si->si_errno);
    }
    h(sig_to_linux(hostsig), si, ctx);
    errno = saved;
}

int __ape_shim_sigaction(int lin_sig, const struct lin_sigaction *lin_act,
                         struct lin_sigaction *lin_old) {
    int hostsig = sig_to_host(lin_sig);
    int slot = hostsig > 0 && hostsig < SHIM_MAXSIG;

    // Snapshot for oldact reporting and for rollback on failure.
    void (*prev1)(int) = slot ? g_h1[hostsig] : 0;
    void (*prev3)(int, siginfo_t *, void *) = slot ? g_h3[hostsig] : 0;

    struct sigaction act, old, *pact = NULL;
    if (lin_act) {
        memset(&act, 0, sizeof(act));
        act.sa_flags = sa_flags_to_host(lin_act->flags);
        act.sa_mask = (sigset_t)lin_act->mask[0];
        void *h = lin_act->handler;
        if (special_disposition(h) || !slot) {
            act.sa_handler = (sighandler_t)h;
            if (slot) { g_h1[hostsig] = 0; g_h3[hostsig] = 0; }
        } else if ((unsigned)lin_act->flags & (unsigned)SHIM_LIN_SA_SIGINFO) {
            act.sa_sigaction = tramp3;
            g_h3[hostsig] = (void (*)(int, siginfo_t *, void *))h;
            g_h1[hostsig] = 0;
        } else {
            act.sa_handler = tramp1;
            g_h1[hostsig] = (void (*)(int))h;
            g_h3[hostsig] = 0;
        }
        pact = &act;
    }

    int r = sigaction(hostsig, pact, &old);
    if (r < 0) {
        if (pact && slot) { g_h1[hostsig] = prev1; g_h3[hostsig] = prev3; }
        return r;
    }

    if (lin_old) {
        memset(lin_old, 0, sizeof(*lin_old));
        void *oh = (void *)old.sa_handler;
        if (oh == (void *)tramp1) oh = (void *)prev1;
        else if (oh == (void *)tramp3) oh = (void *)prev3;
        lin_old->handler = oh;
        lin_old->mask[0] = (unsigned long)old.sa_mask;
        lin_old->flags = sa_flags_to_linux(old.sa_flags);
        lin_old->restorer = 0;
    }
    return 0;
}

int __ape_shim_kill(int pid, int sig) {
    return kill(pid, sig_to_host(sig));
}

int __ape_shim_raise(int sig) {
    return raise(sig_to_host(sig));
}

void (*__ape_shim_signal(int sig, void (*handler)(int)))(int) {
    // musl's signal() is sigaction with SA_RESTART; route through the shim
    // sigaction so the trampoline bookkeeping stays in one place.
    struct lin_sigaction act, old;
    memset(&act, 0, sizeof(act));
    act.handler = (void *)handler;
    act.flags = SHIM_LIN_SA_RESTART;
    if (__ape_shim_sigaction(sig, &act, &old) < 0)
        return (void (*)(int))SIG_ERR;
    return (void (*)(int))old.handler;
}

// ---------------------------------------------------------------------------
// sigaltstack: stack_t layout matches (both are {ss_sp, int ss_flags,
// ss_size}); only the ss_flags values need mapping. Unknown bits
// (SS_AUTODISARM) are dropped.
struct lin_stack {
    void *ss_sp;
    int ss_flags;
    size_t ss_size;
};

static int ss_flags_to_host(int lin) {
    int host = 0;
#define X(name, linval) if ((unsigned)lin & (unsigned)(linval)) host |= name;
    SHIM_SS_TABLE(X)
#undef X
    return host;
}

static int ss_flags_to_linux(int host) {
    unsigned lin = 0;
#define X(name, linval) if ((unsigned)host & (unsigned)name) lin |= (unsigned)(linval);
    SHIM_SS_TABLE(X)
#undef X
    return (int)lin;
}

int __ape_shim_sigaltstack(const struct lin_stack *ss, struct lin_stack *old) {
    stack_t hss, hold;
    stack_t *pss = NULL;
    if (ss) {
        hss.ss_sp = ss->ss_sp;
        hss.ss_flags = ss_flags_to_host(ss->ss_flags);
        hss.ss_size = ss->ss_size;
        pss = &hss;
    }
    int r = sigaltstack(pss, &hold);
    if (r == 0 && old) {
        old->ss_sp = hold.ss_sp;
        old->ss_flags = ss_flags_to_linux(hold.ss_flags);
        old->ss_size = hold.ss_size;
    }
    return r;
}

// ---------------------------------------------------------------------------
// Mask plumbing. The caller's sigset_t is musl's 128-byte one; cosmo's
// functions read/write a uint64. Forward direction can pass the pointer
// straight through (low word is the set); writebacks go through a local so
// the caller's tail bytes end up zeroed instead of stale.

static int how_to_host(int lin, int *out) {
    switch (lin) {
        case SHIM_LIN_SIG_BLOCK:   *out = SIG_BLOCK; return 0;
        case SHIM_LIN_SIG_UNBLOCK: *out = SIG_UNBLOCK; return 0;
        case SHIM_LIN_SIG_SETMASK: *out = SIG_SETMASK; return 0;
        default: return errno = EINVAL, -1;
    }
}

struct lin_sigset {
    unsigned long val[16];
};

static void write_back_set(struct lin_sigset *out, sigset_t host) {
    memset(out, 0, sizeof(*out));
    out->val[0] = (unsigned long)host;
}

int __ape_shim_sigprocmask(int how, const struct lin_sigset *set, struct lin_sigset *old) {
    int h = SIG_SETMASK;
    if (set && how_to_host(how, &h) < 0) return -1;
    sigset_t hold;
    int r = sigprocmask(set ? h : SIG_SETMASK, (const sigset_t *)set, old ? &hold : NULL);
    if (r == 0 && old) write_back_set(old, hold);
    return r;
}

int __ape_shim_pthread_sigmask(int how, const struct lin_sigset *set, struct lin_sigset *old) {
    int h = SIG_SETMASK;
    if (set && how_to_host(how, &h) < 0) return EINVAL; // @returnserrno family
    sigset_t hold;
    int r = pthread_sigmask(set ? h : SIG_SETMASK, (const sigset_t *)set, old ? &hold : NULL);
    if (r == 0 && old) write_back_set(old, hold);
    return r;
}

int __ape_shim_sigaddset(sigset_t *set, int sig) {
    return sigaddset(set, sig_to_host(sig));
}

int __ape_shim_sigdelset(sigset_t *set, int sig) {
    return sigdelset(set, sig_to_host(sig));
}

int __ape_shim_sigismember(const sigset_t *set, int sig) {
    return sigismember(set, sig_to_host(sig));
}
