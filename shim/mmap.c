// mmap()/mprotect() for the Linux-personality shim.
//
// PROT_* and the MAP_SHARED/MAP_PRIVATE/MAP_FIXED type bits are the same
// numbers on every platform cosmo supports and pass straight through; the
// remaining MAP_* bits are runtime constants and get translated. Semantic
// bits with no host value fail with EOPNOTSUPP, hint bits are dropped,
// unknown bits are EINVAL.
//
// Values come from tables.h (`cargo xtask gen-shim`).
//
// Reserve/commit on NT. Allocators commonly reserve a huge PROT_NONE span
// and grow into it with mprotect, but cosmo's NT mmap commits the whole
// span up front, so big reservations fail with ENOMEM. Here such a mapping
// only reserves address space and mprotect commits the touched part on
// demand. The mapping is tagged MAP_APE_RESERVE (mmap.h) so shim/fork-nt.c
// copies it by real page state instead of recommitting the whole span.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/mman.h>
#include <libc/dce.h>
#include <libc/nt/memory.h>
#include <libc/nt/enum/memflags.h>
#include <libc/nt/enum/pageflags.h>
#include <libc/nt/struct/memorybasicinformation.h>
#include <libc/sysv/consts/madv.h>

int madvise(void *, unsigned long, int); // guarded by a feature macro in cosmo's headers

bool __maps_track(char *, size_t, int, int);
void __maps_lock(void);
void __maps_unlock(void);
int __prot2nt(int, int);

#include "mmap.h"
#include "tables.h"

#define LIN_MAP_FIXED_BITS (1 | 2 | 16) // SHARED, PRIVATE, FIXED: universal

struct mbit {
    int linux_bit;
    const int *host;
    bool droppable;
};

#define X(name, lin, drop) { lin, &name, drop },
static const struct mbit kMaps[] = { SHIM_MAP_TABLE(X) };
#undef X
#define NMAPS (sizeof(kMaps) / sizeof(kMaps[0]))

struct advmap {
    int lin;
    const unsigned *host;
};

#define X(name, lin) { lin, &name },
static const struct advmap kMadvs[] = { SHIM_MADV_TABLE(X) };
#undef X
#define NMADVS (sizeof(kMadvs) / sizeof(kMadvs[0]))

int __ape_shim_madvise(void *addr, unsigned long len, int lin) {
    for (size_t i = 0; i < NMADVS; i++)
        if (kMadvs[i].lin == lin) return madvise(addr, len, (int)*kMadvs[i].host);
    return errno = EINVAL, -1;
}

static void *reserve_nt(unsigned long len, int host) {
    void *p = VirtualAlloc(0, len, kNtMemReserve, kNtPageNoaccess);
    if (!p) return errno = ENOMEM, MAP_FAILED;
    __maps_lock();
    bool ok = __maps_track(p, len, PROT_NONE, host | MAP_APE_RESERVE);
    __maps_unlock();
    if (!ok) {
        VirtualFree(p, 0, kNtMemRelease);
        return errno = ENOMEM, MAP_FAILED;
    }
    return p;
}

void *__ape_shim_mmap(void *addr, unsigned long len, int prot, int lin,
                      int fd, long off) {
    int host = lin & LIN_MAP_FIXED_BITS;
    lin &= ~LIN_MAP_FIXED_BITS;
    lin &= ~SHIM_LIN_MAP_STACK; // pure hint; cosmo has no such flag at all
    for (size_t i = 0; i < NMAPS; i++) {
        if (!(lin & kMaps[i].linux_bit)) continue;
        lin &= ~kMaps[i].linux_bit;
        if (*kMaps[i].host) {
            host |= *kMaps[i].host;
        } else if (!kMaps[i].droppable) {
            return errno = EOPNOTSUPP, MAP_FAILED;
        }
    }
    if (lin) return errno = EINVAL, MAP_FAILED;
    if (IsWindows() && prot == PROT_NONE && len && !(host & MAP_FIXED) &&
        (host & MAP_PRIVATE) && (host & MAP_ANONYMOUS))
        return reserve_nt(len, host);
    return mmap(addr, len, prot, host, fd, off);
}

static int commit_nt(char *addr, size_t len, int prot) {
    struct NtMemoryBasicInformation mbi;
    char *p = addr, *end = addr + len;
    uint32_t page = __prot2nt(prot, false);
    while (p < end && VirtualQuery(p, &mbi, sizeof(mbi)) == sizeof(mbi)) {
        char *re = (char *)mbi.BaseAddress + mbi.RegionSize;
        if (re > end) re = end;
        if (mbi.State == kNtMemReserve && mbi.Type == kNtMemPrivate &&
            !VirtualAlloc(p, re - p, kNtMemCommit, page))
            return errno = ENOMEM, -1;
        p = re;
    }
    return 0;
}

int __ape_shim_mprotect(void *addr, size_t len, int prot) {
    if (IsWindows() && prot != PROT_NONE && len && !((uintptr_t)addr & 4095) &&
        commit_nt(addr, len, prot))
        return -1;
    return mprotect(addr, len, prot);
}
