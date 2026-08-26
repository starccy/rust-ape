// Mouse-wheel policy for the NT console, following xterm instead of cosmo.
//
// cosmo translates every wheel notch into arrow keys whenever a program is
// in raw mode, so a line editor walks its history when the user only meant
// to scroll the window. xterm hands the wheel to a program only while the
// alternate screen is active, so this file watches the write path for the
// relevant private modes and enables console mouse input only for xterm
// mouse reporting, or an alternate screen with alternate scroll on.
//
// The bit is per-console state and every cosmo process that starts on the
// console sets it, so a child can silently re-enable wheel-as-arrows for
// its parent. The decision is therefore repeated after every console write
// and tcsetattr, and on the way into read/poll. The scanner keeps its
// state across writes, since a sequence may straddle two write calls.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/uio.h>
#include <poll.h>
#include <libc/calls/internal.h>
#include <libc/dce.h>
#include <libc/intrin/nomultics.h>
#include <libc/nt/console.h>
#include <libc/nt/enum/consolemodeflags.h>
#include <libc/nt/runtime.h>

static bool altscreen;         // DECSET 1049 / 47 / 1047 active
static bool altscroll = true;  // DECSET 1007, on by default like xterm
static enum { ASC, ESC, CSI, PRIV } st;
static unsigned param;
static bool xt_prev;           // kTtyXtMouse as last seen by sync()

void __ape_shim_console_sync(void) {
    if (!IsWindows()) return;
    bool want = (__ttyconf.magic & kTtyXtMouse) || (altscreen && altscroll);
    intptr_t h = GetStdHandle(kNtStdInputHandle);
    uint32_t m;
    if (!GetConsoleMode(h, &m)) return;
    uint32_t m2 = want ? (m | kNtEnableMouseInput) : (m & ~kNtEnableMouseInput);
    // cosmo's "\e[?1000l" handler turns QuickEdit back on even in raw mode
    // (tcsetattr-nt.c had cleared it), after which conhost swallows every
    // mouse event itself and the alternate-screen wheel goes dead. Keep
    // QuickEdit tied to ICANON the way tcsetattr defines it.
    if (__ttyconf.magic & kTtyUncanon) m2 &= ~kNtEnableQuickEditMode;
    if (m2 != m) SetConsoleMode(h, m2);
    xt_prev = (__ttyconf.magic & kTtyXtMouse) != 0;
}

static void apply(unsigned x, bool on) {
    switch (x) {
    case 47: case 1047: case 1049: altscreen = on; break;
    case 1007: altscroll = on; break;
    default: break;
    }
}

static void scan(const unsigned char *p, size_t n) {
    for (size_t i = 0; i < n; i++) {
        unsigned char c = p[i];
        switch (st) {
        case ASC:
            if (c == 033) st = ESC;
            break;
        case ESC:
            st = c == '[' ? CSI : ASC;
            break;
        case CSI:
            if (c == '?') { st = PRIV; param = 0; }
            else if (c >= 0x40 && c <= 0x7e) st = ASC;   // some other CSI final
            // parameter/intermediate bytes of a non-private CSI: stay
            break;
        case PRIV:
            if (c >= '0' && c <= '9') param = param * 10 + (c - '0');
            else if (c == ';') { param = 0; }  // only the last parameter is examined; the four modes we care about are written alone in practice
            else if (c == 'h' || c == 'l') { apply(param, c == 'h'); st = ASC; }
            else if (c >= 0x40 && c <= 0x7e) st = ASC;   // other final (e.g. $p, s, r)
            break;
        }
    }
}

// Called by shim/io.c after a successful console write.
void __ape_shim_console_wrote(int fd, const void *buf, size_t n) {
    if (!IsWindows() || !__isfdkind(fd, kFdConsole)) return;
    bool a = altscreen, s = altscroll;
    scan(buf, n);
    // cosmo's interceptor ran inside write(); re-decide the bit whenever
    // the screen state moved or cosmo may have flipped it (mouse on/off).
    if (a != altscreen || s != altscroll ||
        ((__ttyconf.magic & kTtyXtMouse) != 0) != xt_prev)
        __ape_shim_console_sync();
}

void __ape_shim_console_wrotev(int fd, const struct iovec *iov, int cnt) {
    if (!IsWindows() || !__isfdkind(fd, kFdConsole)) return;
    for (int i = 0; i < cnt; i++) __ape_shim_console_wrote(fd, iov[i].iov_base, iov[i].iov_len);
}

// Before a read() or poll() on a raw-mode console: re-assert the policy, in
// case another process on this console changed the bit
void __ape_shim_console_before_wait(int fd) {
    if (!IsWindows() || !(__ttyconf.magic & kTtyUncanon) || !__isfdkind(fd, kFdConsole)) return;
    __ape_shim_console_sync();
}

void __ape_shim_console_before_poll(const struct pollfd *fds, unsigned long n) {
    if (!IsWindows() || !(__ttyconf.magic & kTtyUncanon)) return;
    for (unsigned long i = 0; i < n; i++) {
        if (fds[i].fd >= 0 && (fds[i].events & POLLIN) && __isfdkind(fds[i].fd, kFdConsole)) {
            __ape_shim_console_sync();
            return;
        }
    }
}
