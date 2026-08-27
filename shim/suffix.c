// The NT executable-suffix retry, like MSYS2's ".exe magic": a bare
// name that misses is retried with ".exe" then ".com", through symlinks.
// Applied only to access/stat/exec on NT; everything else stays truthful.
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

// Probes name+".exe" / name+".com" relative to dirfd, host AT_* coding.
// Returns 1 with the winner in buf[PATH_MAX]; 0 when name has an
// extension already or nothing matched.
static int probe_suffixes(int dirfd, const char *name, char *buf) {
    static const char *const suffixes[] = {".exe", ".com"};
    size_t len = strlen(name);
    if (len == 0 || len + sizeof(".exe") > PATH_MAX) return 0;
    const char *base = strrchr(name, '/');
    base = base ? base + 1 : name;
    if (strchr(base, '.')) return 0;
    for (size_t i = 0; i < sizeof(suffixes) / sizeof(*suffixes); i++) {
        memcpy(buf, name, len);
        strcpy(buf + len, suffixes[i]);
        struct stat st;
        if (!fstatat(dirfd, buf, &st, 0) && S_ISREG(st.st_mode)) return 1;
    }
    return 0;
}

// The magic, applied along a symlink chain as well. A link may point at
// a name that only exists with a suffix; each name in the chain gets
// the same probe.
//
// Returns:
//   1  winner in buf[PATH_MAX]
//   0  magic does not apply or nothing matched; errno preserved
int __ape_shim_exe_fallback(int dirfd, const char *path, char *buf) {
    if (!IsWindows()) return 0;
    int saved = errno;
    char cur[PATH_MAX], target[PATH_MAX];
    size_t len = strlen(path);
    if (len == 0 || len >= PATH_MAX) return 0;
    memcpy(cur, path, len + 1);
    for (int depth = 0; depth < 16; depth++) {
        if (probe_suffixes(dirfd, cur, buf)) {
            errno = saved;
            return 1;
        }
        ssize_t n = readlinkat(dirfd, cur, target, PATH_MAX - 1);
        if (n <= 0) break;
        target[n] = 0;
        if (target[0] == '/') {
            memcpy(cur, target, n + 1);
        } else {
            char *slash = strrchr(cur, '/');
            size_t dir = slash ? (size_t)(slash - cur) + 1 : 0;
            if (dir + n >= PATH_MAX) break;
            memcpy(cur + dir, target, n + 1);
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

// The remaining exec spellings. cosmo's execv family bypasses the
// wrapper above, so each gets the retry itself; for the PATH-searching
// ones it only matters for a name with a slash, which commandv takes
// literally.
extern char **environ;
int execvpe(const char *, char *const[], char *const[]);

int __ape_shim_execv(const char *path, char *const argv[]) {
    return __ape_shim_execve(path, argv, environ);
}

int __ape_shim_execvpe(const char *file, char *const argv[],
                       char *const envp[]) {
    execvpe(file, argv, envp);
    if (errno == ENOENT && strchr(file, '/')) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, file, buf))
            execve(buf, argv, envp);
    }
    return -1;
}

int __ape_shim_execvp(const char *file, char *const argv[]) {
    return __ape_shim_execvpe(file, argv, environ);
}

int __ape_shim_posix_spawnp(int *pid, const char *file,
                            const posix_spawn_file_actions_t *file_actions,
                            const posix_spawnattr_t *attrp, char *const argv[],
                            char *const envp[]) {
    int rc = posix_spawnp(pid, file, file_actions, attrp, argv, envp);
    if (rc == ENOENT && strchr(file, '/')) {
        char buf[PATH_MAX];
        if (__ape_shim_exe_fallback(AT_FDCWD, file, buf))
            rc = posix_spawn(pid, buf, file_actions, attrp, argv, envp);
    }
    return rc;
}
