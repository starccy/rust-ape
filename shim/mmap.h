#ifndef RUST_APE_SHIM_MMAP_H_
#define RUST_APE_SHIM_MMAP_H_

// Marks a map in cosmo's __maps table (host-coded flags) as an NT
// reservation made by shim/mmap.c, meaning address space only, committed
// piecewise by mprotect. fork-nt.c copies such maps by their real page
// state instead of committing the whole span. The bit sits above every
// MAP_* value cosmo gives NT (0x08000000 is the highest) and clear of
// MAP_NOFORK (0x10000000, libc/intrin/maps.h).
#define MAP_APE_RESERVE 0x20000000

#endif
