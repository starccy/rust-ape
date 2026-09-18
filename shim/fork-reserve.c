// Copying an address-space reservation into a forked child, on NT.
//
// cosmo's NT fork rebuilds every private map in the child with a
// full-size VirtualAllocEx(MEM_RESERVE|MEM_COMMIT) and WriteProcessMemory
// of the whole span. shim/mmap.c turns a PROT_NONE anonymous mapping into
// a real NT reservation (address space only, committed piecewise by
// mprotect) and tags it MAP_APE_RESERVE; the upstream loop would commit
// the whole span in the child (4 GiB for one edit document:
// ERROR_COMMITMENT_LIMIT, fork fails) and read uncommitted pages. The
// fork's fork-nt.c offers each private allocation to this hook first.
// Such a map is reserved in the child with one VirtualAllocEx(MEM_RESERVE),
// then walked with VirtualQuery in the parent: each committed run is
// committed in the child, copied, and given the parent's page protection.
// The walk follows NT's own page state, so adjacent 64 KiB pieces a
// growing buffer committed one at a time are one copy, and the cost is
// proportional to committed bytes, not reserved ones. The child side
// needs no change: private maps are already handed over by the parent
// and only get their bookkeeping fixed there.
// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "libc/intrin/maps.h"
#include "libc/nt/enum/memflags.h"
#include "libc/nt/enum/pageflags.h"
#include "libc/nt/memory.h"
#include "libc/nt/struct/memorybasicinformation.h"
#include "mmap.h"

textwindows static bool CopyReservation(int64_t proc, char *addr,
                                        size_t size) {
  if (!VirtualAllocEx(proc, addr, size, kNtMemReserve, kNtPageNoaccess))
    return false;
  struct NtMemoryBasicInformation mbi;
  char *p = addr, *end = addr + size;
  while (p < end) {
    if (VirtualQuery(p, &mbi, sizeof(mbi)) != sizeof(mbi))
      return false;
    char *re = (char *)mbi.BaseAddress + mbi.RegionSize;
    if (re > end)
      re = end;
    if (mbi.State == kNtMemCommit) {
      size_t n = re - p;
      uint32_t prot = mbi.Protect;
      uint32_t old;
      bool readable = prot & (kNtPageReadonly | kNtPageReadwrite |
                              kNtPageExecuteRead | kNtPageExecuteReadwrite |
                              kNtPageWritecopy | kNtPageExecuteWritecopy);
      bool reprotect = !readable || (prot & kNtPageGuard);
      if (!VirtualAllocEx(proc, p, n, kNtMemCommit, kNtPageReadwrite))
        return false;
      if (reprotect && !VirtualProtect(p, n, kNtPageReadwrite, &old))
        return false;
      bool ok = !!WriteProcessMemory(proc, p, p, n, 0);
      if (reprotect)
        ok = !!VirtualProtect(p, n, old, &old) && ok;
      if (!ok)
        return false;
      if (prot != kNtPageReadwrite &&
          !VirtualProtectEx(proc, p, n, prot, &old))
        return false;
    }
    p = re;
  }
  return true;
}

// The hook: 0 leaves the allocation to cosmo, 1 copied it, -1 failed.
textwindows int __ape_shim_fork_copy_map(int64_t proc, struct Map *map,
                                         size_t allocsize) {
  if (!(map->flags & MAP_APE_RESERVE))
    return 0;
  return CopyReservation(proc, map->addr, allocsize) ? 1 : -1;
}
