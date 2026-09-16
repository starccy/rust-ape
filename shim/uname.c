// uname, with the output cleared before cosmo fills it.

#include <string.h>
#include <sys/utsname.h>

int __ape_shim_uname(struct utsname *uts) {
    if (uts) memset(uts, 0, sizeof(*uts));
    return uname(uts);
}
