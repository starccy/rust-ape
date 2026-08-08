// Keeping Winsock up for as long as the process lives, on Windows.
//
// Cosmo starts Winsock on first use and registers WSACleanup with atexit
// (WinSockInit). That teardown runs on the exiting thread while other threads
// may still be inside a socket call, and every one of them then fails with
// WSANOTINITIALISED — which cosmo reports as ENETDOWN, indistinguishable from
// a real network failure. A tokio runtime that outlives main dies there: its
// IO driver treats an error out of poll() as unrecoverable and aborts the
// process, after main has already returned 0.
//
// WSAStartup and WSACleanup are reference counted per process, so one extra
// startup that nothing ever matches holds the count above zero and cosmo's
// atexit handler stops being a teardown. This is also what a program built
// against the MSVC or mingw runtimes gets: neither of them calls WSACleanup
// either, and Windows reclaims the sockets when the process dies.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <stdatomic.h>

#include <libc/dce.h>
#include <libc/nt/winsock.h>

static atomic_int pinned;

void __ape_shim_pin_winsock(void) {
    if (!IsWindows()) return;
    if (atomic_exchange(&pinned, 1)) return;
    struct NtWsaData wsa;
    WSAStartup(0x202, &wsa); // the version cosmo itself asks for
}
