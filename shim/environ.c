// Environment entries whose name is not a name.
//
// NT keeps its per-drive working directories in the environment block under
// names like =C:, so cosmo hands out entries with an empty POSIX name.
// Rust's std rejects such names, meaning env::remove_var fails on them and
// spawning a child with a rebuilt environment errors out, so they are
// dropped once at startup. Only the array cosmo exposes is compacted;
// Windows re-establishes the drive entries for a child on its own.

#include <stddef.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>

extern char **environ;

__attribute__((constructor)) static void __ape_shim_environ_init(void) {
    if (!IsWindows() || !environ) return;
    char **out = environ;
    for (char **in = environ; *in; ++in) {
        // An entry is keyed by what precedes the first '='. Nothing before it
        // means there is no name to look the entry up by.
        if (**in == '=') continue;
        *out++ = *in;
    }
    *out = NULL;
}
