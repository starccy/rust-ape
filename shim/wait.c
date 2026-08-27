// wait/waitpid/wait4: translate the Linux-coded W* option bits to cosmo's
// runtime values, and the signal number a wait status carries back to Linux
// numbering.
//
// WCONTINUED is dropped on NT (cosmo's wait4-nt accepts only
// WNOHANG|WUNTRACED and EINVALs anything else); losing the resume event
// costs less than failing every reap.

#include <errno.h>
#include <stddef.h>
#include <sys/resource.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>

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
