// [rust-ape] GetNtOpenFlags, replacing cosmo 4.0.2's libc.a(createfileflags.o),
// plus real O_SYNC and O_DSYNC values on Windows.
//
// 4.0.2 leaves both constants at the ~0 "unsupported" sentinel on NT, even
// though win32 opens files write-through on request. Oring either into an
// open() mode then yields an all-ones flags word and an unrelated errno, and a
// program has no other way to ask that a write reach the disk. The constants
// are corrected at startup and the bit is translated here into
// kNtFileFlagWriteThrough. cosmopolitan master maps O_DSYNC the same way, so
// this file can go when the toolchain moves there.
//
// Everything below GetNtOpenFlags's flag mask is upstream, verbatim. Compiled
// with -D_COSMO_SOURCE by the linker wrapper.
//
// Derived from cosmopolitan libc/calls/createfileflags.c,
// Copyright 2022 Justine Alexandra Roberts Tunney, ISC license.

// cflags: -D_COSMO_SOURCE

/*-*- mode:c;indent-tabs-mode:nil;c-basic-offset:2;tab-width:8;coding:utf-8 -*-│
│ vi: set et ft=c ts=2 sts=2 sw=2 fenc=utf-8                               :vi │
╞══════════════════════════════════════════════════════════════════════════════╡
│ Copyright 2022 Justine Alexandra Roberts Tunney                              │
│                                                                              │
│ Permission to use, copy, modify, and/or distribute this software for         │
│ any purpose with or without fee is hereby granted, provided that the         │
│ above copyright notice and this permission notice appear in all copies.      │
│                                                                              │
│ THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL                │
│ WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED                │
│ WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE             │
│ AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL         │
│ DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR        │
│ PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER               │
│ TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR             │
│ PERFORMANCE OF THIS SOFTWARE.                                                │
╚─────────────────────────────────────────────────────────────────────────────*/
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/assert.h"
#include "libc/calls/calls.h"
#include "libc/calls/createfileflags.internal.h"
#include "libc/dce.h"
#include "libc/nt/createfile.h"
#include "libc/nt/enum/accessmask.h"
#include "libc/nt/enum/creationdisposition.h"
#include "libc/nt/enum/fileflagandattributes.h"
#include "libc/nt/enum/filesharemode.h"
#include "libc/sysv/consts/o.h"
#include "libc/sysv/errfuns.h"

// NT values for the two flags, matching the Linux coding the rest of this tree
// speaks. Neither bit collides with the O_* values cosmo already uses on NT,
// and O_SYNC keeps O_DSYNC as a subset the way Linux has it.
#define _O_DSYNC 0x00001000
#define _O_SYNC  0x00101000

// cosmo fills these in at startup from a per-host table; alias them without
// const so an unsupported host value can be replaced. A forked NT process
// inherits the corrected value with the rest of the image, and only a fresh
// startup runs this again.
extern unsigned __nt_o_sync __asm__("O_SYNC");
extern unsigned __nt_o_dsync __asm__("O_DSYNC");

__attribute__((__constructor__(101))) static void FixNtSyncFlags(void) {
  if (!IsWindows())
    return;
  if (__nt_o_dsync == 0xffffffffu)
    __nt_o_dsync = _O_DSYNC;
  if (__nt_o_sync == 0xffffffffu)
    __nt_o_sync = _O_SYNC;
}

textwindows int GetNtOpenFlags(int flags, int mode, uint32_t *out_perm,
                               uint32_t *out_share, uint32_t *out_disp,
                               uint32_t *out_attr) {
  bool is_creating_file;
  uint32_t perm, share, disp, attr;

  if (flags &
      ~(O_ACCMODE | _O_APPEND | _O_CREAT | _O_EXCL | _O_TRUNC | _O_DIRECTORY |
        _O_UNLINK | _O_NONBLOCK | _O_RANDOM | _O_SEQUENTIAL | _O_COMPRESSED |
        _O_INDEXED | _O_CLOEXEC | _O_DIRECT | _O_SYNC | _O_DSYNC)) {
    return einval();
  }

  // "Some of these flags should not be combined. For instance,
  //  combining kNtFileFlagRandomAccess with kNtFileFlagSequentialScan
  //  is self-defeating." -Quoth MSDN § CreateFileW
  if ((flags & _O_SEQUENTIAL) && (flags & _O_RANDOM)) {
    return einval();
  }

  switch (flags & O_ACCMODE) {
    case O_RDONLY:
      perm = kNtFileGenericRead;
      break;
    case O_WRONLY:
      perm = kNtFileGenericWrite;
      if (flags & _O_APPEND) {
        // kNtFileAppendData is already present in kNtFileGenericWrite.
        // WIN32 wont act on append access when write is already there.
        perm = kNtFileAppendData;
      }
      break;
    case O_RDWR:
      if (flags & _O_APPEND) {
        perm = kNtFileGenericRead | kNtFileAppendData;
      } else {
        perm = kNtFileGenericRead | kNtFileGenericWrite;
      }
      break;
    default:
      return einval();
  }

  attr = 0;
  is_creating_file = !!(flags & _O_CREAT);

  // POSIX O_EXEC doesn't mean the same thing as kNtGenericExecute. We
  // request execute access when we can determine it from mode's bits.
  // When opening existing files we greedily request execute access so
  // mmap() won't fail. If it causes CreateFile() to fail, our wrapper
  // will try calling CreateFile a second time without execute access.
  if (is_creating_file) {
    if (mode & 0111) {
      perm |= kNtGenericExecute;
    }
    if (~mode & 0200) {
      attr |= kNtFileAttributeReadonly;  // not sure why anyone would
    }
  } else {
    perm |= kNtGenericExecute;
  }

  // Be as generous as possible in sharing file access. Not doing this
  // you'll quickly find yourself no longer able to administer Windows
  // and we need this to be the case even in O_EXCL mode otherwise the
  // shm_open_test.c won't behave consistently on Windows like on UNIX
  share = kNtFileShareRead | kNtFileShareWrite | kNtFileShareDelete;

  // These POSIX to WIN32 mappings are relatively straightforward.
  if (flags & _O_EXCL) {
    if (is_creating_file) {
      disp = kNtCreateNew;
    } else {
      return einval();
    }
  } else if (is_creating_file) {
    if (flags & _O_TRUNC) {
      disp = kNtCreateAlways;
    } else {
      disp = kNtOpenAlways;
    }
  } else if (flags & _O_TRUNC) {
    disp = kNtTruncateExisting;
  } else {
    disp = kNtOpenExisting;
  }

  if (~flags & _O_INDEXED) {
    // The Windows content indexing service ravages performance similar to
    // Windows Defender. Please pass O_INDEXED to openat() to enable this.
    attr |= kNtFileAttributeNotContentIndexed;
  }

  if (flags & _O_COMPRESSED) {
    // Windows' transparent filesystem compression is really cool, as such
    // we've introduced a nonstandard O_COMPRESSED flag to help you use it
    attr |= kNtFileAttributeCompressed;
  }

  if (flags & kNtFileAttributeTemporary) {  // subset of _O_UNLINK
    // "Specifying the kNtFileAttributeTemporary attribute causes file
    //  systems to avoid writing data back to mass storage if sufficient
    //  cache memory is available, because an application deletes a
    //  temporary file after a handle is closed. In that case, the
    //  system can entirely avoid writing the data. Although it does not
    //  directly control data caching in the same way as the
    //  [kNtFileFlagNoBuffering, kNtFileFlagWriteThrough,
    //  kNtFileFlagSequentialScan, and kNtFileFlagRandomAccess] flags,
    //  the kNtFileAttributeTemporary attribute does tell the system to
    //  hold as much as possible in the system cache without writing and
    //  therefore may be of concern for certain applications." -MSDN
    attr |= kNtFileAttributeTemporary;
  }

  if (!attr) {
    // "All other file attributes override kNtFileAttributeNormal [...]
    //  [which] is valid only if used alone." -Quoth MSDN § CreateFileW
    attr |= kNtFileAttributeNormal;
  }

  attr |= flags & kNtFileFlagDeleteOnClose;  // subset of _O_UNLINK

  if (flags & _O_DIRECTORY) {
    // "You must set this flag to obtain a handle to a directory."
    // -Quoth MSDN § CreateFileW
    attr |= kNtFileFlagBackupSemantics;
  }

  // Not certain yet what benefit these flags offer.
  if (flags & _O_SEQUENTIAL)
    attr |= kNtFileFlagSequentialScan;
  if (flags & _O_RANDOM)
    attr |= kNtFileFlagRandomAccess;
  if (flags & _O_DIRECT)
    attr |= kNtFileFlagNoBuffering;

  // Write-through covers both flags: it reaches metadata as well as data,
  // which is what O_SYNC asks for and more than O_DSYNC needs.
  if (flags & (_O_SYNC | _O_DSYNC))
    attr |= kNtFileFlagWriteThrough;

  // TODO(jart): Should we *always* open with write permission if the
  //             kernel will give it to us? We'd then deny write access
  //             in libc system call wrappers.
  //
  // "When an application creates a file across a network, it is better
  //  to use kNtGenericRead | kNtGenericWrite for dwDesiredAccess than
  //  to use kNtGenericWrite alone. The resulting code is faster,
  //  because the redirector can use the cache manager and send fewer
  //  SMBs with more data. This combination also avoids an issue where
  //  writing to a file across a network can occasionally return
  //  kNtErrorAccessDenied." -Quoth MSDN

  if (out_perm)
    *out_perm = perm;
  if (out_share)
    *out_share = share;
  if (out_disp)
    *out_disp = disp;
  if (out_attr)
    *out_attr = attr;
  return 0;
}
