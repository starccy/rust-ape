// The signal half of the Linux-personality shim. sa_flags are cosmo
// runtime constants and struct sigaction's layouts differ outright, so it
// is repacked field by field in both directions. Signal numbers are
// Linux's on both sides.
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
#include <sys/auxv.h>
#include <libc/sysv/consts/sig.h>
#include <libc/sysv/consts/sa.h>
#include <libc/sysv/consts/ss.h>
#include <libc/sysv/consts/auxv.h>

#include "tables.h"


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

// ---------------------------------------------------------------------------
// getauxval: the caller passes Linux-coded AT_* keys, but cosmo's are runtime
// constants (the auxv is synthesized with host numbering on non-Linux
// hosts), so keys map through SHIM_AUXV_TABLE. It lives here because of
// AT_MINSIGSTKSZ: std sizes its sigaltstack as max(musl SIGSTKSZ,
// getauxval(AT_MINSIGSTKSZ)), musl's SIGSTKSZ (8k/12k) is below what the XNU
// kernel accepts, and hosts without the auxv entry answer 0 — so that key
// gets a floor of MINSIGSTKSZ (32768, cosmo's highest-minimum-across-hosts).
// Unmapped keys pass through raw: on Linux the numbering already matches,
// elsewhere they were never going to resolve anyway.
unsigned long __ape_shim_getauxval(unsigned long lin) {
    unsigned long key = lin;
#define X(name, linval) if (lin == (unsigned long)(linval)) key = name;
    SHIM_AUXV_TABLE(X)
#undef X
    unsigned long v = getauxval(key);
    if (lin == SHIM_LIN_AT_MINSIGSTKSZ && v < (unsigned long)MINSIGSTKSZ)
        v = MINSIGSTKSZ;
    return v;
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
