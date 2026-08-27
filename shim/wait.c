// wait/waitpid/wait4: translate the Linux-coded W* option bits to cosmo's
// runtime values, and the signal number a wait status carries back to Linux
// numbering.
//
// WCONTINUED is dropped on NT (cosmo's wait4-nt accepts only
// WNOHANG|WUNTRACED and EINVALs anything else); losing the resume event
// costs less than failing every reap.
//
// waitid: cosmo has no wrapper at all, only the raw syscall number. On Linux
// the kernel gets the call as is, arguments and siginfo are Linux-coded on
// both sides of the shim there. Elsewhere it is built out of wait4 above,
// which loses WNOWAIT (there is no way to look at a status without reaping
// it, so that one is EINVAL) and fills in the siginfo fields waitid defines.

#include <errno.h>
#include <stddef.h>
#include <sys/resource.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>

#include "syscall.h"
#include "tables.h"

// shim/signal.c
int __ape_shim_signum_to_linux(int host);

// Linux W* -> host. Returns 0, or -1 with errno set.
static int wopts_to_host(int lin, int *out) {
    int host = 0;
    if (lin & SHIM_LIN_WNOHANG) {
        lin &= ~SHIM_LIN_WNOHANG;
        host |= WNOHANG;
    }
    if (lin & SHIM_LIN_WUNTRACED) {
        lin &= ~SHIM_LIN_WUNTRACED;
        host |= WUNTRACED;
    }
    if (lin & SHIM_LIN_WCONTINUED) {
        lin &= ~SHIM_LIN_WCONTINUED;
        if (!IsWindows()) host |= WCONTINUED;
    }
    // Linux itself rejects anything else here (WEXITED and WNOWAIT belong to
    // waitid), so an unknown bit is the caller's bug either way.
    if (lin) return errno = EINVAL, -1;
    *out = host;
    return 0;
}

static int wstatus_to_linux(int st) {
    if (WIFSTOPPED(st)) {
        return (__ape_shim_signum_to_linux(WSTOPSIG(st)) << 8) | 0x7f;
    }
    if (WIFSIGNALED(st)) {
        // low 7 bits are the signal, bit 7 is the core-dump flag
        return (st & 0x80) | __ape_shim_signum_to_linux(WTERMSIG(st));
    }
    return st; // exited (signal-free) or continued (0xffff)
}

// cosmo's NT process tracker raises SIGCHLD only when nobody is in wait4
// at the moment the child exits, so a child reaped by the wait itself
// never produces one and an installed handler never runs. The signal is
// raised here after the reap whenever a real handler is installed. A
// child may then be reported twice, which handlers tolerate on Linux too.
static void nt_raise_sigchld(void) {
    struct sigaction sa;
    if (sigaction(SIGCHLD, NULL, &sa) < 0) return;
    if (sa.sa_handler == SIG_DFL || sa.sa_handler == SIG_IGN) return;
    raise(SIGCHLD);
}

pid_t __ape_shim_wait4(pid_t pid, int *status, int lin_options,
                       struct rusage *rusage) {
    int host_options;
    if (wopts_to_host(lin_options, &host_options) < 0) return -1;
    int st = 0;
    pid_t rc = wait4(pid, status ? &st : NULL, host_options, rusage);
    if (rc > 0 && status) *status = wstatus_to_linux(st);
    if (rc > 0 && IsWindows() && (WIFEXITED(st) || WIFSIGNALED(st)))
        nt_raise_sigchld();
    return rc;
}

pid_t __ape_shim_waitpid(pid_t pid, int *status, int lin_options) {
    return __ape_shim_wait4(pid, status, lin_options, NULL);
}

pid_t __ape_shim_wait(int *status) {
    return __ape_shim_wait4(-1, status, 0, NULL);
}

static int waitid_emulate(int idtype, unsigned id, siginfo_t *infop, int lin) {
    // Linux semantics: at least one event class, nothing outside the set.
    int known = SHIM_LIN_WNOHANG | SHIM_LIN_WNOWAIT | SHIM_LIN_WEXITED |
                SHIM_LIN_WSTOPPED | SHIM_LIN_WCONTINUED;
    if ((lin & ~known) ||
        !(lin & (SHIM_LIN_WEXITED | SHIM_LIN_WSTOPPED | SHIM_LIN_WCONTINUED)))
        return errno = EINVAL, -1;
    if (lin & SHIM_LIN_WNOWAIT) return errno = EINVAL, -1;
    pid_t pid;
    switch (idtype) {
        case SHIM_LIN_P_ALL: pid = -1; break;
        case SHIM_LIN_P_PID:
            if ((int)id <= 0) return errno = EINVAL, -1;
            pid = id;
            break;
        case SHIM_LIN_P_PGID: pid = id ? -(int)id : 0; break;
        default: return errno = EINVAL, -1; // P_PIDFD included, none exist here
    }
    // Without WEXITED a wait4 still reaps an exit that cannot be handed back,
    // so it is reported anyway, with the code an exit gets.
    int wopts = lin & SHIM_LIN_WNOHANG;
    if (lin & SHIM_LIN_WSTOPPED) wopts |= SHIM_LIN_WUNTRACED;
    if (lin & SHIM_LIN_WCONTINUED) wopts |= SHIM_LIN_WCONTINUED;
    int st = 0;
    pid_t rc = __ape_shim_wait4(pid, &st, wopts, NULL);
    if (rc < 0) return -1;
    if (!infop) return 0;
    infop->si_errno = 0;
    if (rc == 0) {
        // WNOHANG with nothing to report zeroes the fields a caller inspects
        infop->si_signo = 0;
        infop->si_code = 0;
        infop->si_pid = 0;
        infop->si_uid = 0;
        infop->si_status = 0;
        return 0;
    }
    infop->si_signo = SHIM_LIN_SIGCHLD;
    infop->si_pid = rc;
    infop->si_uid = getuid();
    // st is already Linux-coded, including the signal number
    if ((st & 0xff) == 0x7f) {
        infop->si_code = SHIM_LIN_CLD_STOPPED;
        infop->si_status = (st >> 8) & 0xff;
    } else if (st == 0xffff) {
        infop->si_code = SHIM_LIN_CLD_CONTINUED;
        infop->si_status = SHIM_LIN_SIGCONT;
    } else if ((st & 0x7f) == 0) {
        infop->si_code = SHIM_LIN_CLD_EXITED;
        infop->si_status = (st >> 8) & 0xff;
    } else {
        infop->si_code = (st & 0x80) ? SHIM_LIN_CLD_DUMPED : SHIM_LIN_CLD_KILLED;
        infop->si_status = st & 0x7f;
    }
    return 0;
}

int __ape_shim_waitid(int idtype, unsigned id, siginfo_t *infop,
                      int lin_options) {
    if (IsLinux()) {
        long r = __ape_raw_syscall(SHIM_LIN_SYS_waitid, idtype, id,
                                   (long)infop, lin_options, 0);
        if (r < 0) return errno = -r, -1;
        return 0;
    }
    return waitid_emulate(idtype, id, infop, lin_options);
}
