// The errno half of the Linux-personality shim.
//
// The Rust world is compiled for x86_64/aarch64-unknown-linux-musl, so every
// `libc::E*` constant it compares errno against is baked in as the Linux
// value at compile time. Cosmopolitan resolves errno at runtime to whatever
// the host uses (WSA codes on Windows, XNU numbers on macOS), so those
// comparisons silently go wrong everywhere but Linux.
//
// Instead of chasing every comparison site with a std patch, this file keeps
// the whole Rust world in Linux coding. The patched declarations in std, the
// libc crate and the errno crate all resolve `__errno_location` to
// __ape_shim_errno_location below, which hands out a thread-local copy of
// errno translated host -> Linux. The Rust side never sees a host value.
//
// This file is compiled by cosmocc (see scripts/gcc-linker-wrapper.bash), so
// <errno.h> here is cosmo's, where every E* name is an extern const filled
// in at startup with the host's real value. The right column of the table is
// free; only the Linux values need to be spelled out.

#include <errno.h>
#include <stddef.h>
#include <string.h>

#include "tables.h"

struct map {
    short linux_val;        // what musl bakes into the Rust side
    const errno_t *host;    // cosmo's runtime constant for the same name
};

// Generated from the vendored libc crate and cosmo's errno.h; see tables.h.
#define X(name, lin) { lin, &name },
static const struct map kErrnos[] = { SHIM_ERRNO_TABLE(X) };
#undef X
#define NERRNOS (sizeof(kErrnos) / sizeof(kErrnos[0]))

// Also used by socket.c to translate getsockopt(SO_ERROR) payloads.
int __ape_shim_errno_host_to_linux(int host) {
    if (!host) return 0;
    for (size_t i = 0; i < NERRNOS; i++)
        if (*kErrnos[i].host == host) return kErrnos[i].linux_val;
    return host; // no Linux name for this: pass through raw
}

static int linux_to_host(int lin) {
    if (!lin) return 0;
    for (size_t i = 0; i < NERRNOS; i++)
        if (kErrnos[i].linux_val == lin) return *kErrnos[i].host;
    return lin;
}

// The Rust side reads and writes errno through this thread-local copy.
//
// Telling "a libc call failed since the last visit" apart from "the Rust
// side wrote the copy" cannot be done by comparing errno values: a failing
// call that sets the same errno the host already held is indistinguishable
// from no call at all, and gitoxide's directory walk hit exactly that
// (ENOENT, set_errno(0) from readdir, ENOENT again -> the error read back
// as 0). So values are never compared. After every visit the host errno is
// set to a sentinel no real code produces; seeing anything else on entry means
// the C side wrote errno in between, and only then is the copy retranslated.
// No libc function reads errno as an input, so parking the sentinel there is
// invisible to everyone but us.
#define SHIM_ERRNO_SENTINEL 0x5AFEE44E

static _Thread_local int shim_errno;

int *__ape_shim_errno_location(void) {
    int host = errno;
    if (host != SHIM_ERRNO_SENTINEL) {
        shim_errno = __ape_shim_errno_host_to_linux(host);
    }
    errno = SHIM_ERRNO_SENTINEL;
    return &shim_errno;
}

// std's error_string() would otherwise hand our Linux value to cosmo's
// strerror_r, which expects the host's.
int __ape_shim_strerror_r(int linux_errno, char *buf, size_t buflen) {
    return strerror_r(linux_to_host(linux_errno), buf, buflen);
}

// Same for strerror, which os.strerror and OSError messages go through.
char *__ape_shim_strerror(int linux_errno) {
    return strerror(linux_to_host(linux_errno));
}
