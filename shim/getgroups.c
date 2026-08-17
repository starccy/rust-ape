// getgroups, for hosts with no supplementary-group model.
//
// Windows has one identity per process and no group list to enumerate, so
// cosmo's getgroups fails there. A failure is not how a caller learns that a
// host is different: code that decides which of a file's owner, group and
// other bits apply to it reads the group list first, and an error there
// leaves it with no answer to give rather than a permissive or restrictive
// one.
//
// A process belonging to exactly its own group is the honest reading of what
// NT reports, so that is the answer when the host call cannot give one. The
// host call is tried first and its result passed through untouched wherever
// it succeeds, which leaves the real list intact on Linux.

#include <errno.h>
#include <stddef.h>
#include <sys/types.h>
#include <unistd.h>

int __ape_shim_getgroups(int size, gid_t *list) {
    int n = getgroups(size, list);
    if (n >= 0) return n;

    if (size < 0) return errno = EINVAL, -1;
    if (size == 0) return 1; // the query form: report the count, write nothing
    if (!list) return errno = EFAULT, -1;
    list[0] = getgid();
    return 1;
}
