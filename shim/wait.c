// waitid for the Linux-personality shim. cosmo has no wrapper at all, only
// the raw syscall number. On Linux the kernel gets the call as is; the
// arguments and the siginfo are Linux-coded on both sides of the shim
// there. Elsewhere it is built out of wait4, which loses WNOWAIT (there is
// no way to look at a status without reaping it, so that one is EINVAL)
// and fills in the siginfo fields waitid defines.
//
// wait, waitpid and wait4 need no help any more: the W* options and the
// status coming back are Linux's on the cosmo side.

#include <errno.h>
#include <stddef.h>
#include <signal.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>

#include "syscall.h"
#include "tables.h"

static int waitid_emulate(int idtype, unsigned id, siginfo_t *infop, int options) {
    // Linux semantics: at least one event class, nothing outside the set.
    int known = WNOHANG | WNOWAIT | WEXITED | WSTOPPED | WCONTINUED;
    if ((options & ~known) || !(options & (WEXITED | WSTOPPED | WCONTINUED)))
        return errno = EINVAL, -1;
    if (options & WNOWAIT) return errno = EINVAL, -1;
    pid_t pid;
    switch (idtype) {
        case P_ALL: pid = -1; break;
        case P_PID:
            if ((int)id <= 0) return errno = EINVAL, -1;
            pid = id;
            break;
        case P_PGID: pid = id ? -(int)id : 0; break;
        default: return errno = EINVAL, -1; // P_PIDFD included, none exist here
    }
    // Without WEXITED a wait4 still reaps an exit that cannot be handed back,
    // so it is reported anyway, with the code an exit gets.
    int wopts = options & WNOHANG;
    if (options & WSTOPPED) wopts |= WUNTRACED;
    if (options & WCONTINUED) wopts |= WCONTINUED;
    int st = 0;
    pid_t rc = wait4(pid, &st, wopts, NULL);
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
    infop->si_signo = SIGCHLD;
    infop->si_pid = rc;
    infop->si_uid = getuid();
    if (WIFSTOPPED(st)) {
        infop->si_code = CLD_STOPPED;
        infop->si_status = WSTOPSIG(st);
    } else if (WIFCONTINUED(st)) {
        infop->si_code = CLD_CONTINUED;
        infop->si_status = SIGCONT;
    } else if (WIFEXITED(st)) {
        infop->si_code = CLD_EXITED;
        infop->si_status = WEXITSTATUS(st);
    } else {
        infop->si_code = WCOREDUMP(st) ? CLD_DUMPED : CLD_KILLED;
        infop->si_status = WTERMSIG(st);
    }
    return 0;
}

int __ape_shim_waitid(int idtype, unsigned id, siginfo_t *infop, int options) {
    if (IsLinux()) {
        long r = __ape_raw_syscall(SHIM_LIN_SYS_waitid, idtype, id,
                                   (long)infop, options, 0);
        if (r < 0) return errno = -r, -1;
        return 0;
    }
    return waitid_emulate(idtype, id, infop, options);
}
