/* Force-included into every C and C++ dependency by xtask's C_PREDEFS.
 *
 * Two constraints shape what may go in here. cc-rs hands CFLAGS to .S files
 * too, so nothing may reach the assembler. And autoconf probes compile
 * things like `char pthread_create ();` and read a compile error as "the
 * function is missing", so nothing may pull in a real declaration either.
 * That rules out <sys/types.h>, which reaches libc/thread/thread.h and turns
 * jemalloc's configure into "libpthread is missing".
 */
#ifndef __ASSEMBLER__

/* POSIX has signal.h define pid_t, and XSI adds uid_t along with the
 * siginfo_t accessors. cosmo's does neither, which stops signal-hook's
 * extract.c at the first function it declares. This is the leaf header the
 * three types live in, with nothing else in it. */
#include "libc/calls/weirdtypes.h"

#endif
