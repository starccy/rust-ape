// cosmo starts an NT process with umask 0777.
// Reset it to 0022, so a caller that reads
// it back and computes `mode & ~umask` won't lost
// the write bit. Prevent it to create a read-only file.

#include <sys/stat.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>

__attribute__((constructor)) static void __ape_shim_umask_init(void) {
    if (IsWindows()) umask(0022);
}
