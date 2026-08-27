// /proc/sys/**. Nearly all of it is constants: the point of these files is
// that a reader finds a parseable value, not that the value steers anything,
// since there is no kernel behind them to steer. The exceptions are hostname,
// which is real, and kernel/random/{uuid,boot_id}, which carry the Linux
// semantics of fresh per read and fixed per boot.
//
// Writes are not modeled: a write through the materialized tree lands in a
// temp file nothing reads back. That is a lie, but the honest alternative
// (EACCES) needs open() to know write intent for a path this file owns, and
// no program seen so far does more than warn on a failed sysctl write.

#define _COSMO_SOURCE // for libc/dce.h's IsWindows()

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <libc/dce.h>
#include <libc/stdio/rand.h> // getentropy

#include "procfs.h"

const char *const pfs_sys_dirs[] = {"kernel", "kernel/random", "vm", "fs", 0};

const char *const pfs_sys_files[] = {
    "kernel/ostype",     "kernel/osrelease",     "kernel/version",
    "kernel/hostname",   "kernel/pid_max",       "kernel/threads-max",
    "kernel/random/uuid", "kernel/random/boot_id",
    "vm/overcommit_memory", "fs/file-max", 0};

static void emit_uuid(struct pfs_buf *b, const uint8_t r[16]) {
    pfs_printf(b,
               "%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-"
               "%02x%02x%02x%02x%02x%02x\n",
               r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8], r[9],
               r[10], r[11], r[12], r[13], r[14], r[15]);
}

bool pfs_gen_sys_file(struct pfs_buf *b, const char *rest) {
    if (!strcmp(rest, "kernel/ostype"))
        return pfs_printf(b, "%s\n", pfs_kernel_sysname()), true;
    if (!strcmp(rest, "kernel/osrelease"))
        return pfs_printf(b, "%s\n", pfs_kernel_release()), true;
    if (!strcmp(rest, "kernel/version"))
        return pfs_printf(b, "%s\n", pfs_kernel_version()), true;
    if (!strcmp(rest, "kernel/hostname")) {
        char h[256] = "localhost";
        gethostname(h, sizeof h);
        h[sizeof h - 1] = 0;
        return pfs_printf(b, "%s\n", h), true;
    }
    if (!strcmp(rest, "kernel/pid_max"))
        return pfs_printf(b, "4194304\n"), true;
    if (!strcmp(rest, "kernel/threads-max"))
        return pfs_printf(b, "65536\n"), true;
    if (!strcmp(rest, "kernel/random/uuid")) {
        uint8_t r[16] = {0};
        getentropy(r, sizeof r);
        r[6] = (r[6] & 0x0f) | 0x40; // version 4
        r[8] = (r[8] & 0x3f) | 0x80; // variant 1
        return emit_uuid(b, r), true;
    }
    if (!strcmp(rest, "kernel/random/boot_id")) {
        // fixed for as long as the machine is up: hashed from the boot time
        uint64_t boot = pfs_boot_filetime();
        uint8_t r[16];
        uint64_t h = 0xcbf29ce484222325ull;
        for (int i = 0; i < 16; i++) {
            h ^= (boot >> (i * 4 % 64)) & 0xff;
            h *= 0x100000001b3ull;
            r[i] = (uint8_t)(h >> 32);
        }
        r[6] = (r[6] & 0x0f) | 0x40;
        r[8] = (r[8] & 0x3f) | 0x80;
        return emit_uuid(b, r), true;
    }
    if (!strcmp(rest, "vm/overcommit_memory"))
        return pfs_printf(b, "0\n"), true;
    if (!strcmp(rest, "fs/file-max"))
        return pfs_printf(b, "1048576\n"), true;
    return false;
}
