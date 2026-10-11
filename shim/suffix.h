// The .exe magic in shim/suffix.c, as shim/open.c calls it.
#pragma once

enum { APE_DIR_UNKNOWN, APE_DIR_THERE, APE_DIR_MISSING };

int __ape_shim_miss_dir(void);
int __ape_shim_exe_fallback(int dirfd, const char *path, char *buf, int dir);
