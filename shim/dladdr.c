// dladdr for the Linux-personality shim.
//
// cosmo's dlfcn has no dladdr, so the libc crate's `dladdr` lands
// on the linker as an undefined symbol.
//
// This defines the real name rather than an __ape_shim_* redirect, because
// there is no cosmo function to redirect to -- the same situation as
// stack_chk.c, filling a hole instead of translating across one.
//
// On Linux the answer comes from /proc/self/maps: the mapping that contains
// the address names the file it was mapped from, which is the only field
// the known caller reads (dli_fname). dli_fbase is that mapping's start,
// and the symbol-level fields stay NULL, which is also what glibc reports
// for an address it cannot pin to a symbol. Every other host answers 0
// ("no information"), and callers already treat that as the miss case.
//
// The returned dli_fname points into a static buffer overwritten by the
// next call. glibc hands out pointers into loader internals that live
// forever, so this is a shade less durable, but the call sites read the
// path immediately and cosmo's dlerror is a static buffer already.

#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <libc/dce.h>

struct ape_dl_info {
    const char *dli_fname;
    void *dli_fbase;
    const char *dli_sname;
    void *dli_saddr;
};

static char g_dladdr_fname[4096];

static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    return -1;
}

// One maps line: "start-end perms offset dev inode      path". Returns 1
// when the line's range contains addr and names a file, 0 otherwise. The
// fields between the range and the path never contain '/', so the first
// '/' starts the path; it runs to end of line, spaces included.
static int check_line(char *line, uintptr_t addr, struct ape_dl_info *info) {
    uintptr_t start = 0, end = 0;
    char *p = line;
    int v;
    while ((v = hexval(*p)) >= 0) {
        start = (start << 4) | (uintptr_t)v;
        p++;
    }
    if (p == line || *p != '-') return 0;
    char *q = ++p;
    while ((v = hexval(*p)) >= 0) {
        end = (end << 4) | (uintptr_t)v;
        p++;
    }
    if (p == q || *p != ' ') return 0;
    if (addr < start || addr >= end) return 0;
    char *path = strchr(p, '/');
    if (!path) return 0;
    size_t n = strlen(path);
    if (n >= sizeof(g_dladdr_fname)) return 0;
    memcpy(g_dladdr_fname, path, n + 1);
    info->dli_fname = g_dladdr_fname;
    info->dli_fbase = (void *)start;
    info->dli_sname = 0;
    info->dli_saddr = 0;
    return 1;
}

int dladdr(const void *addr, struct ape_dl_info *info) {
    if (!IsLinux()) return 0;
    int fd = open("/proc/self/maps", O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;

    // Read in chunks, hand check_line one NUL-terminated line at a time.
    // A line longer than the buffer (only a pathological path could get
    // there) is dropped whole: `skipping` eats input until its newline.
    static char buf[8192];
    size_t len = 0;
    int found = 0, skipping = 0;
    for (;;) {
        ssize_t n = read(fd, buf + len, sizeof(buf) - len);
        if (n <= 0) break;
        len += (size_t)n;
        char *line = buf;
        for (;;) {
            char *nl = memchr(line, '\n', len - (size_t)(line - buf));
            if (!nl) break;
            *nl = 0;
            if (skipping) {
                skipping = 0;
            } else if (check_line(line, (uintptr_t)addr, info)) {
                found = 1;
                break;
            }
            line = nl + 1;
        }
        if (found) break;
        len -= (size_t)(line - buf);
        memmove(buf, line, len);
        if (len == sizeof(buf)) {
            len = 0;
            skipping = 1;
        }
    }
    close(fd);
    return found;
}
