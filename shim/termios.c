// The terminal half of the Linux-personality shim.
//
// struct termios, the termios flags, the c_cc indices, the baud codes and
// the tty ioctl request codes are all Linux's on both sides now, and
// cosmo's ioctl() answers the TCGETS family itself. What is left is NT:
// cosmo's tcsetattr rewrites the console mode, and console.c then has to
// re-decide ENABLE_MOUSE_INPUT, which cosmo leaves at its startup value.
// That is what turned the wheel into arrow keys for every raw-mode line
// editor.

#include <stdarg.h>
#include <sys/ioctl.h>
#include <termios.h>

void __ape_shim_console_sync(void); // console.c

int __ape_shim_tcsetattr(int fd, int act, const struct termios *t) {
    int rc = tcsetattr(fd, act, t);
    if (rc == 0) __ape_shim_console_sync();
    return rc;
}

// musl declares the request as int, so the TCGETS2 family arrives sign
// extended; the cast keeps the kernel's 32-bit code.
int __ape_shim_ioctl(int fd, int req, ...) {
    va_list ap;
    va_start(ap, req);
    void *arg = va_arg(ap, void *);
    va_end(ap);
    unsigned long r = (unsigned)req;
    int rc = ioctl(fd, r, arg);
    if (rc == 0 && (r == TCSETS || r == TCSETSW || r == TCSETSF ||
                    r == TCSETS2 || r == TCSETSW2 || r == TCSETSF2))
        __ape_shim_console_sync();
    return rc;
}
