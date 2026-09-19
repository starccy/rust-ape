// The two hooks the fork's read() and readv() call around the read itself.

// cflags: -D_COSMO_SOURCE
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <libc/calls/internal.h>
#include <libc/calls/struct/iovec.h>
#include <libc/calls/struct/iovec.internal.h>
#include <libc/dce.h>
#include <libc/errno.h>
#include <libc/intrin/nomultics.h>
#include <libc/sock/struct/pollfd.h>
#include <libc/sysv/pib.h>

void __ape_shim_epoll_rearm_in(int fd); // shim/epoll.c
int __ape_shim_poll(struct pollfd *, unsigned long, int); // shim/poll.c
void __ape_shim_console_before_wait(int fd);                // shim/console.c
int __ape_shim_console_bracketed_paste(void);               // shim/console.c
long __ape_shim_procfs_memfd_read(int, const struct iovec *, int); // shim/procfs/core/

// ---------------------------------------------------------------------------
// Escape sequences from the NT console arrive one byte per read, so a
// parser fed a lone ESC takes it for the Escape key and stalls on a reply
// that never assembles. Linux delivers the whole sequence in one read, and
// the fix is to do the same, waiting briefly after a console read that ends
// inside an escape sequence and appending the rest to it.

#define ESC_COALESCE_MS 10   // per step; bytes really come ~1ms apart
#define ESC_COALESCE_MAX 64  // longest sequence worth waiting for

// True when the buffer ends inside an escape sequence: a lone ESC, a CSI
// without its final byte, an SS3 without its one following byte, or an OSC
// not yet closed by BEL or ST.
static bool EndsInsideEscape(const unsigned char *b, size_t n) {
  size_t i = n;
  while (i && b[i - 1] != 0x1b)
    i--;
  if (!i)
    return false;  // no ESC at all
  size_t after = n - i;
  if (!after)
    return true;  // lone ESC
  unsigned char c = b[i];
  if (c == '[') {
    for (size_t k = i + 1; k < n; k++)
      if (b[k] >= 0x40 && b[k] <= 0x7e)
        return false;  // final byte seen
    return true;
  }
  if (c == 'O')
    return after < 2;
  if (c == ']') {
    for (size_t k = i + 1; k < n; k++)
      if (b[k] == 0x07)
        return false;
    return true;
  }
  return false;  // ESC + anything else (alt-key) is complete as is
}

static ssize_t CoalesceConsoleEscape(int fd, unsigned char *b, size_t cap,
                                     ssize_t n) {
  while (n > 0 && (size_t)n < cap && n < ESC_COALESCE_MAX &&
         EndsInsideEscape(b, (size_t)n)) {
    struct pollfd p = {fd, 1 /* POLLIN, Linux-coded */, 0};
    if (__ape_shim_poll(&p, 1, ESC_COALESCE_MS) <= 0 || !(p.revents & 1))
      break;
    ssize_t m = sys_readv_nt(fd, &(struct iovec){b + n, cap - (size_t)n}, 1);
    if (m <= 0)
      break;
    n += m;
  }
  return n;
}

// ---------------------------------------------------------------------------
// Bracketed paste for the NT console. ConPTY forwards a program's \e[?2004h
// to the outer terminal, which then wraps pasted text in \e[200~ / \e[201~,
// but conhost drops CSI sequences it cannot map to a key event, so the
// markers never reach the program and pasted lines are treated as typed
// input.
//
// The wrap is reconstructed here. In raw mode the console driver hands out
// one keystroke per read, so the rest of a paste is still queued, and a
// non-empty queue right after a plain-text keystroke is something typing
// does not produce. The queue is drained into the caller's buffer without
// blocking, and a gathered run of plain text containing a line break is
// returned wrapped in the markers. Drain stops at a control sequence, which
// is returned after the closing marker. An oversized or bursty paste comes
// back as several wrapped chunks, which pastes identically.
//
// Another thread reading the same console could steal queued bytes between
// the count and the read and block this one; no real program shares a
// raw-mode stdin.

// A control byte (ESC included) means key or mouse input, not paste content.
static bool IsPasteText(const unsigned char *b, size_t n, bool *got_break,
                        bool *got_text) {
  for (size_t i = 0; i < n; i++) {
    unsigned char c = b[i];
    if (c == '\r' || c == '\n') {
      *got_break = true;
    } else if (c == '\t' || (c >= 0x20 && c != 0x7f)) {
      *got_text = true;
    } else {
      return false;
    }
  }
  return true;
}

static ssize_t WrapConsolePaste(int fd, unsigned char *b, size_t cap,
                                ssize_t n) {
  bool got_break = false, got_text = false;
  if (!__ape_shim_console_bracketed_paste() ||
      !(__ttyconf.magic & kTtyUncanon) || cap < (size_t)n + 12 ||
      !IsPasteText(b, (size_t)n, &got_break, &got_text))
    return n;

  // gather the rest of the burst, stopping before a control sequence and
  // keeping room for the two markers
  ssize_t text_end = n;
  while ((size_t)text_end + 12 < cap && CountConsoleInputBytes() > 0) {
    ssize_t m = sys_readv_nt(
        fd, &(struct iovec){b + text_end, cap - 12 - (size_t)text_end}, 1);
    if (m <= 0)
      break;
    if (!IsPasteText(b + text_end, (size_t)m, &got_break, &got_text)) {
      text_end += m;
      break;
    }
    text_end += m;
    n = text_end;
  }

  // a lone enter, a single-line burst, or key repeat is typed input; a
  // burst of text with a line break in it is a paste
  if (n < 3 || !got_break || !got_text)
    return text_end;
  memmove(b + 6, b, text_end);
  memcpy(b, "\033[200~", 6);
  memmove(b + 6 + n + 6, b + 6 + n, text_end - n);
  memcpy(b + 6 + n, "\033[201~", 6);
  return text_end + 12;
}

static bool IsConsole(int fd) {
  return IsWindows() && fd < __get_pib()->fds.n &&
         __get_pib()->fds.p[fd].kind == kFdConsole;
}

int __ape_shim_read_before(int fd, const struct iovec *iov, int iovlen,
                           ssize_t *rc) {
  if (IsWindows() || IsXnuSilicon()) {
    long n = __ape_shim_procfs_memfd_read(fd, iov, iovlen);
    if (n != -2) {
      *rc = n;
      return 1;
    }
  }
  if (IsConsole(fd))
    __ape_shim_console_before_wait(fd);
  return 0;
}

ssize_t __ape_shim_read_after(int fd, const struct iovec *iov, int iovlen,
                              ssize_t rc) {
  if (rc > 0 && iovlen == 1 && IsConsole(fd)) {
    rc = CoalesceConsoleEscape(fd, iov[0].iov_base, iov[0].iov_len, rc);
    rc = WrapConsolePaste(fd, iov[0].iov_base, iov[0].iov_len, rc);
  }
  if (rc > 0 || (rc == -1 && errno == EAGAIN))
    __ape_shim_epoll_rearm_in(fd);
  return rc;
}
