// [rust-ape] sys_ftruncate_nt, replacing cosmo 4.0.2's
// libc.a(ftruncate-nt.o): a file extended past its current end is
// marked sparse first. ftruncate() and truncate() both funnel through
// this one symbol.
//
// On unix the extended area is a hole: it reads as zeros and costs
// nothing until written. An NTFS file is not sparse by default, so the
// first write past the old end makes the filesystem synchronously
// zero-fill everything in between, which can take seconds and real disk
// for a large extend. FSCTL_SET_SPARSE beforehand restores the unix
// cost model; a filesystem that refuses it (FAT, exFAT, some SMB
// servers) just keeps its eager fill. The side effects -- space is
// allocated at write time rather than reserved by the extend -- are
// exactly ftruncate's behavior on Linux, which is what the programs
// running under this shim are written against. Only an extend marks
// the file; a shrink or same-size call leaves the attributes alone.
//
// The rest is a faithful copy of upstream, except that the
// SetFileInformationByHandle class number is spelled out (see
// SHIM_NT_FILE_END_OF_FILE_INFO below). Compiled with -D_COSMO_SOURCE
// by the linker wrapper. Revisit on toolchain upgrade.
//
// Derived from cosmopolitan libc/calls/ftruncate-nt.c,
// Copyright 2020 Justine Alexandra Roberts Tunney, ISC license.

// cflags: -D_COSMO_SOURCE
#include <stdbool.h>  // [rust-ape] cosmo's own build has C23 bool
#include "libc/calls/syscall-nt.internal.h"
#include "libc/calls/syscall_support-nt.internal.h"
#include "libc/nt/enum/fsctl.h"
#include "libc/nt/errors.h"
#include "libc/nt/files.h"
#include "libc/nt/runtime.h"
#include "libc/nt/struct/byhandlefileinformation.h"
#include "libc/sysv/errfuns.h"

// FileEndOfFileInfo's FILE_INFO_BY_HANDLE_CLASS number, spelled out
// rather than taken from libc/nt/enum/fileinfobyhandleclass.h, because
// that header's second block is off by one (see shim/rename.c). Its
// kNtFileAllocationInfo is 6, which in the real win32 enum means
// FileEndOfFileInfo -- so upstream has always, correctly if
// accidentally, been setting the end-of-file mark here. A literal keeps
// that behavior whether or not the header ever gets fixed.
#define SHIM_NT_FILE_END_OF_FILE_INFO 6

textwindows int sys_ftruncate_nt(int64_t handle, uint64_t length) {
  // [rust-ape] mark the file sparse before extending it
  struct NtByHandleFileInformation info;
  if (GetFileInformationByHandle(handle, &info)) {
    uint64_t size =
        (uint64_t)info.nFileSizeHigh << 32 | info.nFileSizeLow;
    if (length > size) {
      uint8_t yes = 1;  // FILE_SET_SPARSE_BUFFER.SetSparse
      uint32_t br;
      DeviceIoControl(handle, kNtFsctlSetSparse, &yes, sizeof(yes),
                      0, 0, &br, 0);
    }
  }
  if (SetFileInformationByHandle(handle, SHIM_NT_FILE_END_OF_FILE_INFO,
                                 &length, sizeof(length))) {
    return 0;
  } else if (GetLastError() == kNtErrorAccessDenied) {
    return einval();  // ftruncate() doesn't raise EACCES
  } else {
    return __winerr();
  }
}
