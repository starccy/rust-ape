// Raw Linux syscalls for the shim.
//
// A few APIs the Rust world expects have no cosmo counterpart at all: cosmo
// ships the __NR_* numbers for them but no wrapper, its sys_* slots are
// generated assembly that doesn't cover them, and its syscall() is a stub
// that answers ENOSYS to nearly everything. Where the API is Linux-only to
// begin with -- xattr, prlimit -- the honest implementation is to issue the
// syscall directly on Linux and degrade everywhere else.
//
// Every caller MUST gate on IsLinux() first. These instructions are only
// meaningful there; on Windows or XNU the same numbers mean nothing.
//
// On errno: the shim convention is to set errno HOST-coded and let errno.c
// hand the Rust side the Linux coding it compares against. A Linux syscall
// reports -errno in the LINUX coding -- but since this only ever runs under
// IsLinux(), where cosmo's runtime E* constants are themselves the Linux
// values, the two codings coincide and the value can be stored directly.

#ifndef RUST_APE_SHIM_SYSCALL_H_
#define RUST_APE_SHIM_SYSCALL_H_

#include <errno.h>

#ifdef __x86_64__
static inline long __ape_raw_syscall6(long n, long a, long b, long c, long d,
                                      long e, long f) {
    long r;
    register long r10 asm("r10") = d;
    register long r8 asm("r8") = e;
    register long r9 asm("r9") = f;
    asm volatile("syscall"
                 : "=a"(r)
                 : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                 : "rcx", "r11", "memory");
    return r;
}
#elif defined(__aarch64__)
static inline long __ape_raw_syscall6(long n, long a, long b, long c, long d,
                                      long e, long f) {
    register long x8 asm("x8") = n;
    register long x0 asm("x0") = a;
    register long x1 asm("x1") = b;
    register long x2 asm("x2") = c;
    register long x3 asm("x3") = d;
    register long x4 asm("x4") = e;
    register long x5 asm("x5") = f;
    asm volatile("svc #0"
                 : "+r"(x0)
                 : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5)
                 : "memory");
    return x0;
}
#else
#error "no raw syscall for this architecture"
#endif

static inline long __ape_raw_syscall(long n, long a, long b, long c, long d,
                                     long e) {
    return __ape_raw_syscall6(n, a, b, c, d, e, 0);
}

// Linux packs -errno into the low 4K of the return value; anything else is a
// valid result (a length, or 0).
static inline long __ape_syscall_ret(long r) {
    if (r < 0 && r > -4096) {
        errno = (int)-r;
        return -1;
    }
    return r;
}

#endif // RUST_APE_SHIM_SYSCALL_H_
