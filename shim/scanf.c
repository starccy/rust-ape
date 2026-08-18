// The glibc `__isoc99_*` scanf aliases, which cosmo does not carry.
//
// Variadics cannot be passed along directly, so each one goes through the
// `v*` form.

#include <stdarg.h>
#include <stdio.h>

int __isoc99_scanf(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int rc = vscanf(fmt, ap);
    va_end(ap);
    return rc;
}

int __isoc99_fscanf(FILE *f, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int rc = vfscanf(f, fmt, ap);
    va_end(ap);
    return rc;
}

int __isoc99_sscanf(const char *s, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int rc = vsscanf(s, fmt, ap);
    va_end(ap);
    return rc;
}
