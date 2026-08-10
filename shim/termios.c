// The terminal half of the Linux-personality shim, covering termios and the
// tty ioctls. Among the structs audited in layouts.c this is the one that
// needs repacking; the two worlds disagree on the struct itself, not just
// on values:
//
//   musl  struct termios: 4x u32 flags, c_line @16, c_cc[NCCS=32] @17,
//                         speeds @52/56 — 60 bytes
//   cosmo struct termios: 4x u32 flags, c_cc[NCCS=20] @16 (no c_line),
//                         _c_ispeed/_c_ospeed @36/40 — 44 bytes
//
// and on top of that the flag bits (ICANON & co), the c_cc indices (VMIN &
// co) and the baud codes are all runtime constants on the cosmo side.
//
// There are two callers to serve, because the Rust world reaches the tty
// two ways:
//   - crates calling libc::tcgetattr/tcsetattr/cf* — they hand us musl's
//     60-byte struct;
//   - rustix (crossterm's backend, even with rustix_use_libc) — it goes
//     through libc::ioctl(TCGETS2)/ioctl(TCGETS) with the KERNEL structs:
//     termios (36B, c_cc[19]) and termios2 (44B, +speeds @36/40).
// All three Linux-side layouts share the same kernel prefix {flags x4,
// c_line @16, c_cc @17}, so one translation core serves everyone; the ioctl
// shim reroutes the tty-config requests into cosmo's tcgetattr/tcsetattr.
//
// Unknown/unsupported flag bits are DROPPED silently in both directions —
// that is what kernels themselves do with termios bits, and erroring here
// would break raw-mode setup over a bit nobody can act on anyway.

#include <errno.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <termios.h>
#include <unistd.h>

#include <libc/intrin/nomultics.h>
#include <libc/sysv/consts/baud.internal.h>
#include <libc/sysv/consts/fio.h>
#include <libc/sysv/consts/termios.h>

#include "tables.h"

// cosmo's NT tcsetattr rebuilds __ttyconf.magic from the termios flags alone
// (tcsetattr-nt.c starts with `__ttyconf.magic = 0`), which also wipes
// kTtyXtMouse -- a bit owned by the DECSET interceptor on the write path,
// not by termios. Carrying it across the call keeps xterm mouse reporting
// active no matter how raw-mode toggles and mouse-enable writes are
// ordered. A no-op on hosts whose tcsetattr goes to the kernel.
static int tcsetattr_keep_mouse(int fd, int act, const struct termios *t) {
    unsigned mouse = __ttyconf.magic & kTtyXtMouse;
    int rc = tcsetattr(fd, act, t);
    __ttyconf.magic |= mouse;
    return rc;
}

// ---- Linux-side layouts (kernel prefix shared) ----------------------------

struct lin_termios { // musl's struct termios
    uint32_t c_iflag, c_oflag, c_cflag, c_lflag;
    uint8_t c_line;
    uint8_t c_cc[32];
    uint32_t ispeed, ospeed; // musl __c_ispeed/__c_ospeed (cf* use CBAUD instead)
};
_Static_assert(sizeof(struct lin_termios) == 60, "musl termios is 60 bytes");

struct lin_ktermios { // kernel struct termios (TCGETS/TCSETS*)
    uint32_t c_iflag, c_oflag, c_cflag, c_lflag;
    uint8_t c_line;
    uint8_t c_cc[19];
};
_Static_assert(sizeof(struct lin_ktermios) == 36, "kernel termios is 36 bytes");

struct lin_ktermios2 { // kernel struct termios2 (TCGETS2/TCSETS*2)
    uint32_t c_iflag, c_oflag, c_cflag, c_lflag;
    uint8_t c_line;
    uint8_t c_cc[19];
    uint32_t c_ispeed, c_ospeed;
};
_Static_assert(sizeof(struct lin_ktermios2) == 44, "kernel termios2 is 44 bytes");

// ---- single-bit flag translation, table-driven both ways ------------------
//
// Forward drops Linux bits the host lacks (name expands to 0); reverse only
// reports bits whose full host pattern is present.

#define DEFINE_BITS(fn, TABLE)                                                \
    static uint32_t fn##_to_host(uint32_t lin) {                              \
        uint32_t h = 0;                                                       \
        TABLE(fn##_FWD)                                                       \
        return h;                                                             \
    }                                                                         \
    static uint32_t fn##_to_linux(uint32_t host) {                            \
        uint32_t lin = 0;                                                     \
        TABLE(fn##_REV)                                                       \
        return lin;                                                           \
    }
#define iflag_FWD(name, linval) if (lin & (uint32_t)(linval)) h |= (uint32_t)(name);
#define iflag_REV(name, linval) if ((uint32_t)(name) && (host & (uint32_t)(name)) == (uint32_t)(name)) lin |= (uint32_t)(linval);
#define oflag_FWD iflag_FWD
#define oflag_REV iflag_REV
#define cflag_FWD iflag_FWD
#define cflag_REV iflag_REV
#define lflag_FWD iflag_FWD
#define lflag_REV iflag_REV

DEFINE_BITS(iflag, SHIM_TIFLAG_TABLE)
DEFINE_BITS(oflag, SHIM_TOFLAG_TABLE)
DEFINE_BITS(cflag, SHIM_TCFLAG_TABLE)
DEFINE_BITS(lflag, SHIM_TLFLAG_TABLE)

// CSIZE and the output-delay groups are multi-bit enumerated FIELDS; the
// bit tables above must not touch them. cflag: mask CSIZE off before the
// bit pass (CS6/7/8 sit in the table but their lin codes overlap, handled
// here). oflag: same for the delay masks.

#define LIN_ODLY_MASK                                                          \
    (SHIM_LIN_NLDLY | SHIM_LIN_CRDLY | SHIM_LIN_TABDLY | SHIM_LIN_BSDLY |     \
     SHIM_LIN_VTDLY | SHIM_LIN_FFDLY)

static uint32_t odly_to_host(uint32_t lin) {
    uint32_t h = 0;
#define DLY(HM, LM, name) \
    if ((uint32_t)(HM) && (lin & (uint32_t)(LM)) == (uint32_t)(SHIM_LIN_##name)) h |= (uint32_t)(name);
    DLY(NLDLY, SHIM_LIN_NLDLY, NL1)
    DLY(CRDLY, SHIM_LIN_CRDLY, CR1)
    DLY(CRDLY, SHIM_LIN_CRDLY, CR2)
    DLY(CRDLY, SHIM_LIN_CRDLY, CR3)
    DLY(TABDLY, SHIM_LIN_TABDLY, TAB1)
    DLY(TABDLY, SHIM_LIN_TABDLY, TAB2)
    DLY(TABDLY, SHIM_LIN_TABDLY, TAB3)
    DLY(BSDLY, SHIM_LIN_BSDLY, BS1)
    DLY(VTDLY, SHIM_LIN_VTDLY, VT1)
    DLY(FFDLY, SHIM_LIN_FFDLY, FF1)
#undef DLY
    return h;
}

static uint32_t odly_to_linux(uint32_t host) {
    uint32_t lin = 0;
#define DLY(HM, name) \
    if ((uint32_t)(HM) && (host & (uint32_t)(HM)) == (uint32_t)(name) && (uint32_t)(name)) lin |= (uint32_t)(SHIM_LIN_##name);
    DLY(NLDLY, NL1)
    DLY(CRDLY, CR1)
    DLY(CRDLY, CR2)
    DLY(CRDLY, CR3)
    DLY(TABDLY, TAB1)
    DLY(TABDLY, TAB2)
    DLY(TABDLY, TAB3)
    DLY(BSDLY, BS1)
    DLY(VTDLY, VT1)
    DLY(FFDLY, FF1)
#undef DLY
    return lin;
}

static uint32_t csize_to_host(uint32_t lin) {
    uint32_t f = lin & SHIM_LIN_CSIZE;
    if (f == (uint32_t)SHIM_LIN_CS6) return (uint32_t)CS6;
    if (f == (uint32_t)SHIM_LIN_CS7) return (uint32_t)CS7;
    if (f == (uint32_t)SHIM_LIN_CS8) return (uint32_t)CS8;
    return 0; // CS5 is 0 on both sides
}

static uint32_t csize_to_linux(uint32_t host) {
    uint32_t f = host & (uint32_t)CSIZE;
    if ((uint32_t)CS8 && f == (uint32_t)CS8) return SHIM_LIN_CS8;
    if ((uint32_t)CS7 && f == (uint32_t)CS7) return SHIM_LIN_CS7;
    if ((uint32_t)CS6 && f == (uint32_t)CS6) return SHIM_LIN_CS6;
    return 0;
}

// Baud: musl codes live in c_cflag's CBAUD field; cosmo keeps its own code
// in _c_ispeed/_c_ospeed. Unmatched codes translate to none (speed left as
// the host default) — B460800/B921600 exist only on the musl side.
static uint32_t baud_to_host(uint32_t lincode, bool *ok) {
    *ok = true;
#define X(name, linval) if (lincode == (uint32_t)(linval)) return (uint32_t)(name);
    SHIM_BAUD_TABLE(X)
#undef X
    *ok = false;
    return 0;
}

static int64_t baud_to_linux(uint32_t hostcode) {
#define X(name, linval) if (hostcode == (uint32_t)(name)) return (int64_t)(linval);
    SHIM_BAUD_TABLE(X)
#undef X
    return -1;
}

// c_cc: move each control character between the two indexings. The
// "disabled" encoding differs too (Linux 0, BSD 0xff): map it for every
// slot except VMIN/VTIME, where 0 is a legitimate count.
// VMIN/VTIME are counts, everything else is a character: the compare rides
// on the Linux-side index macros so it stays pure preprocessor arithmetic.
#define CC_IS_COUNT(linval) ((linval) == SHIM_LIN_VMIN || (linval) == SHIM_LIN_VTIME)

static void cc_to_host(const uint8_t *lin_cc, int n, uint8_t host_cc[NCCS]) {
#define X(name, linval)                                                        \
    if ((int)(linval) < n && (uint32_t)(name) < NCCS) {                        \
        uint8_t b = lin_cc[(int)(linval)];                                     \
        if (b == 0 && !CC_IS_COUNT(linval)) b = (uint8_t)_POSIX_VDISABLE;      \
        host_cc[(uint32_t)(name)] = b;                                         \
    }
    SHIM_TCC_TABLE(X)
#undef X
}

static void cc_to_linux(const uint8_t host_cc[NCCS], uint8_t *lin_cc, int n) {
#define X(name, linval)                                                        \
    if ((int)(linval) < n && (uint32_t)(name) < NCCS) {                        \
        uint8_t b = host_cc[(uint32_t)(name)];                                 \
        if (b == (uint8_t)_POSIX_VDISABLE && !CC_IS_COUNT(linval)) b = 0;      \
        lin_cc[(int)(linval)] = b;                                             \
    }
    SHIM_TCC_TABLE(X)
#undef X
}

// ---- translation core over the shared kernel prefix -----------------------

static void prefix_to_host(struct termios *t, const uint32_t f[4],
                           const uint8_t *cc, int n) {
    memset(t, 0, sizeof *t);
    t->c_iflag = iflag_to_host(f[0]);
    t->c_oflag = oflag_to_host(f[1] & ~LIN_ODLY_MASK) | odly_to_host(f[1]);
    t->c_cflag = cflag_to_host(f[2] & ~(SHIM_LIN_CSIZE | SHIM_LIN_CBAUD |
                                        SHIM_LIN_CBAUDEX)) |
                 csize_to_host(f[2]);
    t->c_lflag = lflag_to_host(f[3]);
    cc_to_host(cc, n, t->c_cc);
    bool ok;
    uint32_t sp = baud_to_host(f[2] & SHIM_LIN_CBAUD, &ok);
    if (ok) {
        cfsetospeed(t, sp);
        cfsetispeed(t, sp);
    }
}

static void prefix_to_linux(const struct termios *t, uint32_t f[4],
                            uint8_t *cc, int n) {
    f[0] = iflag_to_linux(t->c_iflag);
    f[1] = oflag_to_linux(t->c_oflag) | odly_to_linux(t->c_oflag);
    f[2] = cflag_to_linux(t->c_cflag & ~(uint32_t)CSIZE) |
           csize_to_linux(t->c_cflag);
    f[3] = lflag_to_linux(t->c_lflag);
    memset(cc, 0, n);
    cc_to_linux(t->c_cc, cc, n);
    int64_t bo = baud_to_linux(cfgetospeed(t));
    if (bo >= 0) f[2] |= (uint32_t)bo;
}

// ---- the musl-struct entry points (libc::tcgetattr & co) ------------------

int __ape_shim_tcgetattr(int fd, struct lin_termios *lt) {
    struct termios t;
    if (tcgetattr(fd, &t) < 0) return -1;
    memset(lt, 0, sizeof *lt);
    uint32_t f[4];
    prefix_to_linux(&t, f, lt->c_cc, 32);
    lt->c_iflag = f[0], lt->c_oflag = f[1], lt->c_cflag = f[2], lt->c_lflag = f[3];
    return 0;
}

// TCSANOW/TCSADRAIN/TCSAFLUSH are 0/1/2 on both sides: pass through.
int __ape_shim_tcsetattr(int fd, int act, const struct lin_termios *lt) {
    struct termios t;
    uint32_t f[4] = { lt->c_iflag, lt->c_oflag, lt->c_cflag, lt->c_lflag };
    prefix_to_host(&t, f, lt->c_cc, 32);
    return tcsetattr_keep_mouse(fd, act, &t);
}

int __ape_shim_tcflush(int fd, int qs) {
#define X(name, linval) if (qs == (linval)) return tcflush(fd, name);
    SHIM_TCFLUSH_TABLE(X)
#undef X
    return errno = EINVAL, -1;
}

int __ape_shim_tcflow(int fd, int action) {
#define X(name, linval) if (action == (linval)) return tcflow(fd, name);
    SHIM_TCFLOW_TABLE(X)
#undef X
    return errno = EINVAL, -1;
}

// The cf* family never touches the host: it manipulates the musl struct in
// musl coding, exactly as musl's own implementations do. (Resolving these
// to cosmo's versions would run cosmo semantics on a musl layout.)

uint32_t __ape_shim_cfgetospeed(const struct lin_termios *lt) {
    return lt->c_cflag & SHIM_LIN_CBAUD;
}

uint32_t __ape_shim_cfgetispeed(const struct lin_termios *lt) {
    return lt->c_cflag & SHIM_LIN_CBAUD;
}

int __ape_shim_cfsetospeed(struct lin_termios *lt, uint32_t sp) {
    if (sp & ~(uint32_t)SHIM_LIN_CBAUD) return errno = EINVAL, -1;
    lt->c_cflag &= ~(uint32_t)SHIM_LIN_CBAUD;
    lt->c_cflag |= sp;
    return 0;
}

int __ape_shim_cfsetispeed(struct lin_termios *lt, uint32_t sp) {
    return sp ? __ape_shim_cfsetospeed(lt, sp) : 0;
}

void __ape_shim_cfmakeraw(struct lin_termios *lt) {
    lt->c_iflag &= ~(uint32_t)(SHIM_LIN_IGNBRK | SHIM_LIN_BRKINT |
                               SHIM_LIN_PARMRK | SHIM_LIN_ISTRIP |
                               SHIM_LIN_INLCR | SHIM_LIN_IGNCR |
                               SHIM_LIN_ICRNL | SHIM_LIN_IXON);
    lt->c_oflag &= ~(uint32_t)SHIM_LIN_OPOST;
    lt->c_lflag &= ~(uint32_t)(SHIM_LIN_ECHO | SHIM_LIN_ECHONL |
                               SHIM_LIN_ICANON | SHIM_LIN_ISIG |
                               SHIM_LIN_IEXTEN);
    lt->c_cflag &= ~(uint32_t)(SHIM_LIN_CSIZE | SHIM_LIN_PARENB);
    lt->c_cflag |= (uint32_t)SHIM_LIN_CS8;
    lt->c_cc[SHIM_LIN_VMIN] = 1;
    lt->c_cc[SHIM_LIN_VTIME] = 0;
}

// ---- ioctl ----------------------------------------------------------------
//
// The request codes are musl-coded. tty-config requests reroute through
// tcgetattr/tcsetattr (rustix's path: TCGETS2, falling back to TCGETS);
// winsize/FIONREAD forward with the code translated. Anything else is
// ENOTTY, which is what a kernel answers for an ioctl the target can't do.
// (FIONBIO stays unsupported on purpose; the std patches route
// set_nonblocking through fcntl instead.)

static int tio_get(int fd, void *arg, bool two) {
    struct termios t;
    if (tcgetattr(fd, &t) < 0) return -1;
    if (two) {
        struct lin_ktermios2 *k = arg;
        memset(k, 0, sizeof *k);
        uint32_t f[4];
        prefix_to_linux(&t, f, k->c_cc, 19);
        k->c_iflag = f[0], k->c_oflag = f[1], k->c_cflag = f[2], k->c_lflag = f[3];
        // Numeric speeds stay 0: rustix and friends decode the CBAUD code
        // we set in c_cflag; the host's own code is not a rate number.
    } else {
        struct lin_ktermios *k = arg;
        memset(k, 0, sizeof *k);
        uint32_t f[4];
        prefix_to_linux(&t, f, k->c_cc, 19);
        k->c_iflag = f[0], k->c_oflag = f[1], k->c_cflag = f[2], k->c_lflag = f[3];
    }
    return 0;
}

static int tio_set(int fd, const void *arg, bool two, int act) {
    struct termios t;
    uint32_t f[4];
    const uint8_t *cc;
    if (two) {
        const struct lin_ktermios2 *k = arg;
        f[0] = k->c_iflag, f[1] = k->c_oflag, f[2] = k->c_cflag, f[3] = k->c_lflag;
        cc = k->c_cc;
        // A BOTHER-coded arbitrary rate has no host equivalent; the speed
        // is simply not carried over (terminal work never sets one).
    } else {
        const struct lin_ktermios *k = arg;
        f[0] = k->c_iflag, f[1] = k->c_oflag, f[2] = k->c_cflag, f[3] = k->c_lflag;
        cc = k->c_cc;
    }
    prefix_to_host(&t, f, cc, 19);
    return tcsetattr_keep_mouse(fd, act, &t);
}

int __ape_shim_ioctl(int fd, int lin_req, ...) {
    va_list ap;
    va_start(ap, lin_req);
    void *arg = va_arg(ap, void *);
    va_end(ap);

    if (lin_req == SHIM_LIN_TCGETS) return tio_get(fd, arg, false);
    if (lin_req == SHIM_LIN_TCGETS2) return tio_get(fd, arg, true);
    if (lin_req == SHIM_LIN_TCSETS) return tio_set(fd, arg, false, TCSANOW);
    if (lin_req == SHIM_LIN_TCSETSW) return tio_set(fd, arg, false, TCSADRAIN);
    if (lin_req == SHIM_LIN_TCSETSF) return tio_set(fd, arg, false, TCSAFLUSH);
    if (lin_req == SHIM_LIN_TCSETS2) return tio_set(fd, arg, true, TCSANOW);
    if (lin_req == SHIM_LIN_TCSETSW2) return tio_set(fd, arg, true, TCSADRAIN);
    if (lin_req == SHIM_LIN_TCSETSF2) return tio_set(fd, arg, true, TCSAFLUSH);
    if (lin_req == SHIM_LIN_TIOCGWINSZ && TIOCGWINSZ)
        return ioctl(fd, TIOCGWINSZ, arg); // struct winsize: 4x u16 both sides
    if (lin_req == SHIM_LIN_TIOCSWINSZ && TIOCSWINSZ)
        return ioctl(fd, TIOCSWINSZ, arg);
    if (lin_req == SHIM_LIN_FIONREAD && FIONREAD)
        return ioctl(fd, FIONREAD, arg); // int* both sides

    // Session and job control, which is what a pty child does between fork
    // and exec: setsid, then claim the slave as its controlling terminal.
    // Without these portable-pty's spawn stops at ENOTTY.
    if (lin_req == SHIM_LIN_TIOCSCTTY && TIOCSCTTY)
        return ioctl(fd, TIOCSCTTY, arg); // arg is an int by value, not a pointer
    if (lin_req == SHIM_LIN_TIOCNOTTY && TIOCNOTTY) return ioctl(fd, TIOCNOTTY, arg);
    // cosmo publishes no TIOCGPGRP or TIOCGSID, and its tcsetpgrp knows what
    // to do on NT, so all three go through the tc* wrappers instead.
    if (lin_req == SHIM_LIN_TIOCGPGRP) {
        pid_t p = tcgetpgrp(fd);
        if (p < 0) return -1;
        *(int32_t *)arg = (int32_t)p;
        return 0;
    }
    if (lin_req == SHIM_LIN_TIOCSPGRP) return tcsetpgrp(fd, (pid_t)*(const int32_t *)arg);
    if (lin_req == SHIM_LIN_TIOCGSID) {
        pid_t s = tcgetsid(fd);
        if (s < 0) return -1;
        *(int32_t *)arg = (int32_t)s;
        return 0;
    }

    return errno = ENOTTY, -1;
}
