// posix_spawnattr flags for the Linux-personality shim.
//
// The POSIX_SPAWN_* bits are numbered differently on cosmo: its USEVFORK
// is 1, which is RESETIDS on Linux, and everything else is shifted up
// one bit. std passes Linux values, so before the redirect cosmo read
// SETSIGDEF|SETSIGMASK as SETPGROUP|SETSIGDEF: every Command::spawn put
// the child in a new process group and the signal mask never applied.
#include <spawn.h>

#include "tables.h"

static const struct {
    short lin;
    short host;
} kFlags[] = {
    {SHIM_LIN_POSIX_SPAWN_RESETIDS, POSIX_SPAWN_RESETIDS},
    {SHIM_LIN_POSIX_SPAWN_SETPGROUP, POSIX_SPAWN_SETPGROUP},
    {SHIM_LIN_POSIX_SPAWN_SETSIGDEF, POSIX_SPAWN_SETSIGDEF},
    {SHIM_LIN_POSIX_SPAWN_SETSIGMASK, POSIX_SPAWN_SETSIGMASK},
    {SHIM_LIN_POSIX_SPAWN_SETSCHEDPARAM, POSIX_SPAWN_SETSCHEDPARAM},
    {SHIM_LIN_POSIX_SPAWN_SETSCHEDULER, POSIX_SPAWN_SETSCHEDULER},
    {SHIM_LIN_POSIX_SPAWN_USEVFORK, POSIX_SPAWN_USEVFORK},
    {SHIM_LIN_POSIX_SPAWN_SETSID, POSIX_SPAWN_SETSID},
};

int __ape_shim_posix_spawnattr_setflags(posix_spawnattr_t *attr, short lin) {
    short host = 0;
    for (unsigned i = 0; i < sizeof(kFlags) / sizeof(kFlags[0]); i++)
        if (lin & kFlags[i].lin) host |= kFlags[i].host;
    return posix_spawnattr_setflags(attr, host);
}

int __ape_shim_posix_spawnattr_getflags(const posix_spawnattr_t *attr,
                                        short *out) {
    short host;
    int rc = posix_spawnattr_getflags(attr, &host);
    if (rc) return rc;
    short lin = 0;
    for (unsigned i = 0; i < sizeof(kFlags) / sizeof(kFlags[0]); i++)
        if (host & kFlags[i].host) lin |= kFlags[i].lin;
    *out = lin;
    return 0;
}
