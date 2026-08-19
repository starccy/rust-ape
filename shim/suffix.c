// The NT executable-suffix retry, like MSYS2's ".exe magic": a bare
// name that misses is retried with ".exe" then ".com". Applied only to
// access/stat/exec on NT; everything else stays truthful.
// __ape_shim_exe_fallback is shared with the wrappers in shim/open.c.

#include <errno.h>
#include <limits.h>
#include <spawn.h>
#include <stddef.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>
#include <libc/sysv/consts/at.h>

// Probes path+".exe" / path+".com" relative to dirfd, host AT_* coding.
// Returns 1 with the winner in buf[PATH_MAX]; 0 (errno preserved) when
// the magic doesn't apply or nothing matched.
int __ape_shim_exe_fallback(int dirfd, const char *path, char *buf) {
    static const char *const suffixes[] = {".exe", ".com"};
    if (!IsWindows()) return 0;
    size_t len = strlen(path);
    if (len == 0 || len + sizeof(".exe") > PATH_MAX) return 0;
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    if (strchr(base, '.')) return 0;
    int saved = errno;
    for (size_t i = 0; i < sizeof(suffixes) / sizeof(*suffixes); i++) {
        memcpy(buf, path, len);
        strcpy(buf + len, suffixes[i]);
        struct stat st;
        if (!fstatat(dirfd, buf, &st, 0) && S_ISREG(st.st_mode)) {
            errno = saved;
            return 1;
        }
    }
    errno = saved;
    return 0;
}

int __ape_shim_execve(const char *path, char *const argv[],
                      char *const envp[]) {
    execve(path, argv, envp);
    if (errno == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf))
            execve(buf, argv, envp);
    }
    return -1;
}

// posix_spawn reports failure in its return value, not errno.
int __ape_shim_posix_spawn(int *pid, const char *path,
                           const posix_spawn_file_actions_t *file_actions,
                           const posix_spawnattr_t *attrp, char *const argv[],
                           char *const envp[]) {
    int rc = posix_spawn(pid, path, file_actions, attrp, argv, envp);
    if (rc == ENOENT) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, path, buf))
            rc = posix_spawn(pid, buf, file_actions, attrp, argv, envp);
    }
    return rc;
}
