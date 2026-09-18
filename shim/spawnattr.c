// posix_spawnattr flags for the Linux-personality shim.
//
// The POSIX_SPAWN_* bits are numbered differently on cosmo: its USEVFORK
// is 1, which is RESETIDS on Linux, and everything else is shifted up
// one bit. std passes Linux values, so before the redirect cosmo read
// SETSIGDEF|SETSIGMASK as SETPGROUP|SETSIGDEF: every Command::spawn put
// the child in a new process group and the signal mask never applied.
//
// On NT the flags also get USEVFORK. cosmo only takes the vfork path
// when asked, and without it posix_spawn is a fork() of the whole
// address space plus an exec, a hundred milliseconds instead of a few.
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <spawn.h>
#include <libc/dce.h>

#define LIN_RESETIDS      0x01
#define LIN_SETPGROUP     0x02
#define LIN_SETSIGDEF     0x04
#define LIN_SETSIGMASK    0x08
#define LIN_SETSCHEDPARAM 0x10
#define LIN_SETSCHEDULER  0x20
#define LIN_USEVFORK      0x40
#define LIN_SETSID        0x80

static const struct {
    short lin;
    short host;
} kFlags[] = {
    {LIN_RESETIDS, POSIX_SPAWN_RESETIDS},
    {LIN_SETPGROUP, POSIX_SPAWN_SETPGROUP},
    {LIN_SETSIGDEF, POSIX_SPAWN_SETSIGDEF},
    {LIN_SETSIGMASK, POSIX_SPAWN_SETSIGMASK},
    {LIN_SETSCHEDPARAM, POSIX_SPAWN_SETSCHEDPARAM},
    {LIN_SETSCHEDULER, POSIX_SPAWN_SETSCHEDULER},
    {LIN_USEVFORK, POSIX_SPAWN_USEVFORK},
    {LIN_SETSID, POSIX_SPAWN_SETSID},
};

int __ape_shim_posix_spawnattr_setflags(posix_spawnattr_t *attr, short lin) {
    short host = 0;
    for (unsigned i = 0; i < sizeof(kFlags) / sizeof(kFlags[0]); i++)
        if (lin & kFlags[i].lin) host |= kFlags[i].host;
    if (IsWindows()) host |= POSIX_SPAWN_USEVFORK;
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
