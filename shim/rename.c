// rename/renameat: cosmo's NT path (MoveFileEx) cannot replace a
// destination that is still held open, so an EACCES rename is retried via
// FileRenameInfoEx's POSIX_SEMANTICS flag (Win10 1607+), which unlinks the
// destination name immediately the way unix does.

#include <errno.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>
#include <libc/nt/createfile.h>
#include <libc/nt/enum/accessmask.h>
#include <libc/nt/enum/creationdisposition.h>
#include <libc/nt/enum/fileflagandattributes.h>
#include <libc/nt/enum/filesharemode.h>
#include <libc/nt/files.h>
#include <libc/nt/runtime.h>
#include <libc/nt/struct/filerenameinformation.h>
#include <libc/sysv/consts/at.h>

#include "tables.h"

// shim/mkntpathat.c
int __mkntpathat(int dirfd, const char *path, int flags, char16_t file[]);

// FILE_RENAME_FLAG_*, which cosmo has no names for. They occupy the DWORD
// that struct NtFileRenameInformation calls Replace -- win32 overlays the
// older BOOLEAN ReplaceIfExists with a flags word there, and bit 0 keeps
// its old meaning, so the struct is reused as-is.
#define kNtFileRenameFlagReplaceIfExists 0x00000001u
#define kNtFileRenameFlagPosixSemantics 0x00000002u

// FileRenameInfoEx's FILE_INFO_BY_HANDLE_CLASS number, spelled out rather
// than taken from libc/nt/enum/fileinfobyhandleclass.h, because that header
// is wrong. Its second block (kNtFileRenameInfo through kNtFileRenameInfoEx)
// is off by one against the win32 enum -- visibly so, since the two blocks
// collide with each other: kNtFileStreamInfo and kNtFileEndOfFileInfo are
// both 7, kNtFileRemoteProtocolInfo and kNtFileIoPriorityHintInfo are both
// 13. kNtFileRenameInfoEx (23) is simply not a class and fails with
// ERROR_INVALID_PARAMETER; worse, kNtFileRenameInfo (4) lands on
// FileDispositionInfo, where this struct's leading word reads as
// DeleteFile=TRUE -- it reports success and silently deletes the source
// instead of renaming it. Verified on 10.0.26200 by trying all four numbers
// and checking which file survived with which contents.
#define SHIM_NT_FILE_RENAME_INFO_EX 22

static int at_fdcwd(int dirfd) {
    return dirfd == SHIM_LIN_AT_FDCWD ? AT_FDCWD : dirfd;
}

// Returns 0 on success, -1 without disturbing errno on failure.
static int posix_rename_nt(int olddirfd, const char *oldpath, int newdirfd,
                           const char *newpath) {
    struct {
        char16_t oldpath16[PATH_MAX];
        char16_t newpath16[PATH_MAX];
        struct NtFileRenameInformation info;
        char16_t tail[PATH_MAX];
    } *m;
    if (!(m = malloc(sizeof(*m)))) return -1;

    int rc = -1;
    if (__mkntpathat(olddirfd, oldpath, 0, m->oldpath16) != -1 &&
        __mkntpathat(newdirfd, newpath, 0, m->newpath16) != -1) {
        // DELETE is the access right a rename needs; BACKUP_SEMANTICS lets
        // the same call open a directory, which rename must also handle.
        int64_t h = CreateFile(
            m->oldpath16, kNtDelete | kNtSynchronize,
            kNtFileShareRead | kNtFileShareWrite | kNtFileShareDelete, NULL,
            kNtOpenExisting, kNtFileFlagBackupSemantics, 0);
        if (h != kNtInvalidHandleValue) {
            size_t n = 0;
            while (m->newpath16[n]) n++;
            m->info.Replace = kNtFileRenameFlagReplaceIfExists |
                              kNtFileRenameFlagPosixSemantics;
            m->info.RootDir = NULL;
            m->info.FileNameLength = n * sizeof(char16_t);
            memcpy(m->info.FileName, m->newpath16, (n + 1) * sizeof(char16_t));
            if (SetFileInformationByHandle(
                    h, SHIM_NT_FILE_RENAME_INFO_EX, &m->info,
                    sizeof(m->info) + m->info.FileNameLength)) {
                rc = 0;
            }
            CloseHandle(h);
        }
    }
    free(m);
    return rc;
}

static int rename_common(int olddirfd, const char *oldpath, int newdirfd,
                         const char *newpath) {
    if (renameat(olddirfd, oldpath, newdirfd, newpath) != -1) return 0;
    // Only EACCES on NT is worth a second attempt; everything else is a
    // real error and keeps its errno.
    if (!IsWindows() || errno != EACCES) return -1;
    int saved = errno;
    if (posix_rename_nt(olddirfd, oldpath, newdirfd, newpath) == 0) return 0;
    return errno = saved, -1;
}

int __ape_shim_rename(const char *oldpath, const char *newpath) {
    return rename_common(AT_FDCWD, oldpath, AT_FDCWD, newpath);
}

int __ape_shim_renameat(int olddirfd, const char *oldpath, int newdirfd,
                        const char *newpath) {
    return rename_common(at_fdcwd(olddirfd), oldpath, at_fdcwd(newdirfd),
                         newpath);
}
