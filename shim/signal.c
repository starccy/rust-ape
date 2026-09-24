// The signal half of the Linux-personality shim. struct sigaction's
// layouts differ outright, so it is repacked field by field in both
// directions. Signal numbers and sa_flags are Linux's on both sides.
//
// Handlers are wrapped in a trampoline that puts the interrupted thread's
// errno back; siginfo_t and ucontext_t pass through as they are.
//
// Only the low 64 bits of a sigset_t are meaningful.

#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <libc/sysv/consts/sig.h>
#include <libc/sysv/consts/sa.h>


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

// bits cosmo knows; anything else (SA_RESTORER and the like) is dropped
#define SA_KNOWN (SA_NOCLDSTOP | SA_NOCLDWAIT | SA_SIGINFO | SA_ONSTACK | \
                  SA_RESTART | SA_NODEFER | SA_RESETHAND)

// ---------------------------------------------------------------------------
// Handler trampolines. Slot table is indexed by signum; cosmo supports
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
    h(hostsig);
    errno = saved;
}

static void tramp3(int hostsig, siginfo_t *si, void *ctx) {
    void (*h)(int, siginfo_t *, void *) =
        (hostsig > 0 && hostsig < SHIM_MAXSIG) ? g_h3[hostsig] : 0;
    if (!h) return;
    int saved = errno;
    // cosmo's siginfo_t is Linux-ABI-shaped (si_addr/si_pid/si_status all
    // at the musl offsets)
    h(hostsig, si, ctx);
    errno = saved;
}

int __ape_shim_sigaction(int lin_sig, const struct lin_sigaction *lin_act,
                         struct lin_sigaction *lin_old) {
    int hostsig = lin_sig; // same numbers on both sides
    int slot = hostsig > 0 && hostsig < SHIM_MAXSIG;

    // Snapshot for oldact reporting and for rollback on failure.
    void (*prev1)(int) = slot ? g_h1[hostsig] : 0;
    void (*prev3)(int, siginfo_t *, void *) = slot ? g_h3[hostsig] : 0;

    struct sigaction act, old, *pact = NULL;
    if (lin_act) {
        memset(&act, 0, sizeof(act));
        act.sa_flags = (unsigned)lin_act->flags & SA_KNOWN;
        act.sa_mask = (sigset_t)lin_act->mask[0];
        void *h = lin_act->handler;
        if (special_disposition(h) || !slot) {
            act.sa_handler = (sighandler_t)h;
            if (slot) { g_h1[hostsig] = 0; g_h3[hostsig] = 0; }
        } else if ((unsigned)lin_act->flags & (unsigned)SA_SIGINFO) {
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
        lin_old->flags = (int)(old.sa_flags & SA_KNOWN);
        lin_old->restorer = 0;
    }
    return 0;
}

void (*__ape_shim_signal(int sig, void (*handler)(int)))(int) {
    // musl's signal() is sigaction with SA_RESTART; route through the shim
    // sigaction so the trampoline bookkeeping stays in one place.
    struct lin_sigaction act, old;
    memset(&act, 0, sizeof(act));
    act.handler = (void *)handler;
    act.flags = SA_RESTART;
    if (__ape_shim_sigaction(sig, &act, &old) < 0)
        return (void (*)(int))SIG_ERR;
    return (void (*)(int))old.handler;
}

// ---------------------------------------------------------------------------
// Mask plumbing. The caller's sigset_t is musl's 128-byte one; cosmo's
// functions read/write a uint64. Forward direction can pass the pointer
// straight through (low word is the set); writebacks go through a local so
// the caller's tail bytes end up zeroed instead of stale.

struct lin_sigset {
    unsigned long val[16];
};

static void write_back_set(struct lin_sigset *out, sigset_t host) {
    memset(out, 0, sizeof(*out));
    out->val[0] = (unsigned long)host;
}

int __ape_shim_sigprocmask(int how, const struct lin_sigset *set, struct lin_sigset *old) {
    sigset_t hold;
    int r = sigprocmask(set ? how : SIG_SETMASK, (const sigset_t *)set, old ? &hold : NULL);
    if (r == 0 && old) write_back_set(old, hold);
    return r;
}

int __ape_shim_pthread_sigmask(int how, const struct lin_sigset *set, struct lin_sigset *old) {
    sigset_t hold;
    int r = pthread_sigmask(set ? how : SIG_SETMASK, (const sigset_t *)set, old ? &hold : NULL);
    if (r == 0 && old) write_back_set(old, hold);
    return r;
}

// ---------------------------------------------------------------------------
// SIGRTMIN/SIGRTMAX. musl exposes these as functions rather than constants, so
// the libc crate calls __libc_current_sigrt{min,max}() and cosmo, which has
// them as runtime `extern const int` instead, defines neither symbol. Without
// these two nothing that pulls tokio's signal feature will link.
//
// The answers are musl's own numbers, not the host's, because these are
// consumed inside the Rust world rather than handed to cosmo. tokio sizes its
// per-signal table with SIGRTMAX() and indexes it by signal number, and those
// numbers are Linux-coded everywhere above the shim boundary.
//
// Real-time signals themselves still don't work off Linux, per the note at
// the top of this file. Nothing here changes that; it only gets the range's
// bounds to agree with the coding everything else uses.
int __libc_current_sigrtmin(void) {
    return 35;
}

int __libc_current_sigrtmax(void) {
    return 64;
}
