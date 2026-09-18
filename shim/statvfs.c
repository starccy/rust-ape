// statvfs / fstatvfs: f_flag comes back in the host's bits (MNT_* on XNU,
// volume flags on NT). Rebuild it from Linux's ST_* values. The struct
// itself has the same layout on both sides.

#define _COSMO_SOURCE // for libc/dce.h's IsLinux()
#include <stddef.h>
#include <stdint.h>
#include <sys/statvfs.h>
#include <libc/dce.h>
#include <libc/sysv/consts/st.h>

#include "tables.h"

#define X(name, lin) { lin, &name },
static const struct {
    unsigned long lin;
    const int *host;
} kFlags[] = { SHIM_ST_TABLE(X) };
#undef X

static void flags_to_linux(struct statvfs *sv) {
    if (IsLinux()) return;
    unsigned long lin = 0;
    for (size_t i = 0; i < sizeof(kFlags) / sizeof(kFlags[0]); i++) {
        unsigned long h = (unsigned long)*kFlags[i].host;
        if (h && (sv->f_flag & h) == h) lin |= kFlags[i].lin;
    }
    sv->f_flag = lin;
}

int __ape_shim_statvfs(const char *path, struct statvfs *sv) {
    int rc = statvfs(path, sv);
    if (!rc) flags_to_linux(sv);
    return rc;
}

int __ape_shim_fstatvfs(int fd, struct statvfs *sv) {
    int rc = fstatvfs(fd, sv);
    if (!rc) flags_to_linux(sv);
    return rc;
}
