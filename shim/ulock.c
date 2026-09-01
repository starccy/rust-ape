// [rust-ape] ulock_wait/ulock_wake, replacing cosmo 4.0.2's
// libcosmo.a(ulock.o), with one change: the compare value of the 32-bit
// wait operations is truncated to 32 bits before entering the kernel.
//
// Upstream passes cosmo_futex_wait's int expect through the uint64_t
// value parameter, so a negative word (std's thread parker uses -1 for
// "parked") arrives sign-extended. Current XNU compares all 64 bits and
// answers an immediate "value changed" instead of sleeping, which turns
// every wait on a negative word into a spin that pins a core. The 32-bit
// ulock ops are documented to compare 32 bits, so masking is correct on
// every XNU version.
//
// Derived from cosmopolitan libc/intrin/ulock.c,
// Copyright 2023 Justine Alexandra Roberts Tunney, ISC license.

// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/intrin/ulock.h"
#include "libc/calls/syscall_support-sysv.internal.h"
#include "libc/errno.h"
#include "libc/intrin/describeflags.h"
#include "libc/intrin/kprintf.h"
#include "libc/intrin/strace.h"

int sys_ulock_wait(uint32_t operation, void *addr, uint64_t value,
                   uint32_t timeout_micros) asm("sys_futex_cp");

// [rust-ape] the operations whose compare is 32-bit wide
static int ulock_value_is_32bit(uint32_t operation) {
  switch (operation & 0xff) {
    case UL_COMPARE_AND_WAIT:
    case UL_UNFAIR_LOCK:
    case UL_COMPARE_AND_WAIT_SHARED:
      return 1;
    default:
      return 0;
  }
}

// returns number of other waiters, or -1 w/ errno
int ulock_wait(uint32_t operation, void *addr, uint64_t value,
               uint32_t timeout_micros) {
  int rc;
  operation |= ULF_WAIT_CANCEL_POINT;
  if (ulock_value_is_32bit(operation)) // [rust-ape]
    value = (uint32_t)value;
  LOCKTRACE("ulock_wait(%#x, %p, %lx, %u) → ...", operation, addr, value,
            timeout_micros);
  rc = sys_ulock_wait(operation, addr, value, timeout_micros);
  if (rc == -1) {
    if (errno == ENOMEM)
      errno = EAGAIN;
    if (errno == EFAULT)
      if (!kisdangerous(addr))
        errno = EAGAIN;
  }
  LOCKTRACE("ulock_wait(%#x, %p, %lx, %u) → %d% m", operation, addr, value,
            timeout_micros, rc);
  return rc;
}

// returns -errno
//
// should be dontinstrument because SiliconThreadMain() calls this from
// a stack managed by apple libc.
dontinstrument int ulock_wake(uint32_t operation, void *addr,
                              uint64_t wake_value) {
  int rc;
  rc = __syscall3i(operation, (long)addr, wake_value, 0x2000000 | 516);
  LOCKTRACE("ulock_wake(%#x, %p, %lx) → %s", operation, addr, wake_value,
            DescribeErrno(rc));
  return rc;
}
