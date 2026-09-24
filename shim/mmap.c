// mmap()/mprotect() for the Linux-personality shim.
//
// PROT_* and MAP_* are Linux's numbers on the cosmo side too, and cosmo
// drops the hint bits a host doesn't have. Left here: unknown bits are
// EINVAL and huge pages off Linux are EOPNOTSUPP. msync needs no help.
//
// Reserve/commit on NT. Allocators commonly reserve a huge PROT_NONE span
// and grow into it with mprotect, but cosmo's NT mmap commits the whole
// span up front, so big reservations fail with ENOMEM. Here such a mapping
// only reserves address space and mprotect commits the touched part on
// demand. The mapping is tagged MAP_APE_RESERVE (mmap.h) so shim/fork-reserve.c
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

#define MAP_KNOWN                                                          \
    (MAP_TYPE | MAP_FIXED | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE | MAP_HUGETLB | \
     MAP_NORESERVE | MAP_POPULATE | MAP_LOCKED | MAP_DENYWRITE |               \
     MAP_EXECUTABLE | MAP_NONBLOCK | MAP_STACK)

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

void *__ape_shim_mmap(void *addr, unsigned long len, int prot, int host,
                      int fd, long off) {
    if (host & ~MAP_KNOWN) return errno = EINVAL, MAP_FAILED;
    if ((host & MAP_HUGETLB) && !IsLinux()) return errno = EOPNOTSUPP, MAP_FAILED;
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
