// The kernel names a task after the file it exec'd, which under the ape
// loader is the loader itself, so comm carries the loader's name instead
// of the program's. Set the executable's basename once at startup on
// Linux; a direct exec already has it right and the call is a no-op.

#define _COSMO_SOURCE // prctl, libc/dce.h's IsLinux(), GetProgramExecutableName()
#include <string.h>
#include <sys/prctl.h>
#include <libc/dce.h>
#include <libc/runtime/runtime.h>

__attribute__((constructor)) static void __ape_shim_comm_init(void) {
    if (!IsLinux()) return;
    const char *exe = GetProgramExecutableName();
    if (!exe || !*exe) return;
    const char *base = strrchr(exe, '/');
    base = base ? base + 1 : exe;
    if (!*base) return;
    prctl(PR_SET_NAME, base);
}
